#!/usr/bin/env node
/**
 * 希尔娅 中继服务器（APK ↔ Windows 端互联网通信）
 *
 * 部署（VPS）：
 *   cd relay-server && npm ci && npm start
 *   生产建议置于 Nginx/Caddy 反向代理后启用 WSS/TLS：
 *     Caddy: your.domain.com { reverse_proxy 127.0.0.1:8787 }
 *   Cloudflare Workers 适配说明见 README.md
 *
 * 协议：
 *  WebSocket（路径 /ws）：
 *    客户端连上后第一条消息必须是 auth：
 *      {type:"auth", token:"<session_token>"}             桌面/已配对 APK
 *      {type:"auth", code:"<配对码>"}                      APK 扫码配对中
 *    服务端回复 auth_ok / auth_error / pair_pending / pair_result
 *    之后双向消息：
 *      {type:"message", to:"<device_id>", body:{...}, msg_id:"<uuid>"}
 *    心跳：客户端每 30s 发 {type:"ping"}，服务端回 {type:"pong"}；90s 无心跳断开
 *
 *  REST：
 *    POST /pairing/register   {label:"..."}                     → {code, expires_in}（桌面生成配对码）
 *    POST /pairing/confirm    {code, accept:true|false}         → {ok}（桌面确认/拒绝）
 *    GET  /status                                                  → {devices, pairings} 概况
 *    GET  /health                                                 → {ok}
 *
 *  安全：配对码一次性 + 5 分钟过期；session token 32 字节随机；消息限流；
 *        生产必须走 WSS + 反向代理加 TLS。
 */
'use strict';

const http = require('node:http');
const crypto = require('node:crypto');
const { WebSocketServer } = require('ws');

const PORT = Number(process.env.PORT || 8787);
const PAIR_TTL_MS = 5 * 60 * 1000;
const HEARTBEAT_TIMEOUT_MS = 90 * 1000;
const MAX_OFFLINE_MESSAGES = 100;
const MAX_MESSAGE_BYTES = 64 * 1024;
const RATE_LIMIT_PER_SEC = 10;

// ─────────────────────────── 状态 ───────────────────────────

/** device_id -> { token, role: "desktop"|"apk", label, ws, last_seen, offlineQueue: [] } */
const devices = new Map();
/** code -> { device_id, expires_at, confirmed: null|boolean } */
const pairings = new Map();
/** device_id -> { count, reset_at } 简单限流 */
const rateLimits = new Map();

function randomToken(bytes = 32) {
  return crypto.randomBytes(bytes).toString('base64url');
}

function randomCode() {
  // 8 位字母数字（排除易混字符），扫码 + 手动输入均可
  const alphabet = 'ABCDEFGHJKLMNPQRSTUVWXYZ23456789';
  let code = '';
  for (let i = 0; i < 8; i += 1) {
    code += alphabet[crypto.randomInt(alphabet.length)];
  }
  return code;
}

function uniqueDeviceId(role) {
  const prefix = role === 'desktop' ? 'win' : 'apk';
  let id;
  do {
    id = `${prefix}-${crypto.randomBytes(4).toString('hex')}`;
  } while (devices.has(id));
  return id;
}

function allowMessage(deviceId) {
  const now = Date.now();
  const entry = rateLimits.get(deviceId) || { count: 0, reset_at: now + 1000 };
  if (entry.reset_at <= now) {
    entry.count = 0;
    entry.reset_at = now + 1000;
  }
  entry.count += 1;
  rateLimits.set(deviceId, entry);
  return entry.count <= RATE_LIMIT_PER_SEC;
}

function send(ws, payload) {
  if (ws && ws.readyState === 1) {
    try {
      ws.send(JSON.stringify(payload));
    } catch (_) { /* ignore */ }
  }
}

function sendToDevice(deviceId, payload) {
  const device = devices.get(deviceId);
  if (!device) return false;
  if (device.ws && device.ws.readyState === 1) {
    send(device.ws, payload);
    return true;
  }
  // 离线：入队（只对 desktop 入队，APK 短连接不入队）
  if (device.role === 'desktop' && payload.type === 'message') {
    device.offlineQueue.push(payload);
    if (device.offlineQueue.length > MAX_OFFLINE_MESSAGES) {
      device.offlineQueue.shift();
    }
  }
  return false;
}

// ─────────────────────────── WebSocket ───────────────────────────

function handleWs(ws, req) {
  let device = null;

  const heartbeat = setInterval(() => {
    if (device && device.ws === ws && Date.now() - device.last_seen > HEARTBEAT_TIMEOUT_MS) {
      ws.terminate();
    }
  }, 30 * 1000);

  ws.on('message', (raw) => {
    if (raw.length > MAX_MESSAGE_BYTES) {
      send(ws, { type: 'error', error: 'message too large' });
      return;
    }
    let msg;
    try {
      msg = JSON.parse(raw.toString());
    } catch (_) {
      send(ws, { type: 'error', error: 'invalid json' });
      return;
    }

    // 未认证：只接受 auth
    if (!device) {
      handleAuth(ws, msg);
      return;
    }
    device.last_seen = Date.now();

    if (msg.type === 'ping') {
      send(ws, { type: 'pong', ts: Date.now() });
      return;
    }
    if (msg.type === 'message') {
      handleMessage(device, msg);
      return;
    }
    send(ws, { type: 'error', error: `unknown type: ${msg.type}` });
  });

  ws.on('close', () => {
    clearInterval(heartbeat);
    if (device && devices.get(device.id)?.ws === ws) {
      devices.get(device.id).ws = null;
      device.ws = null;
    }
  });

  ws.on('error', () => {
    clearInterval(heartbeat);
  });
}

function handleAuth(ws, msg) {
  if (msg.type !== 'auth') {
    send(ws, { type: 'auth_error', error: 'auth required' });
    ws.close(4001, 'auth required');
    return;
  }
  // 配对码模式（APK 扫码）
  if (msg.code) {
    const pairing = pairings.get(msg.code);
    if (!pairing) {
      send(ws, { type: 'auth_error', error: 'invalid pairing code' });
      ws.close(4002, 'invalid pairing code');
      return;
    }
    if (pairing.expires_at < Date.now()) {
      pairings.delete(msg.code);
      send(ws, { type: 'auth_error', error: 'pairing code expired' });
      ws.close(4003, 'expired');
      return;
    }
    // APK 侧先用 code 建立会话（等待桌面确认）
    const deviceId = uniqueDeviceId('apk');
    device = {
      id: deviceId,
      role: 'apk',
      label: msg.label || 'Android',
      token: randomToken(),
      ws,
      last_seen: Date.now(),
      offlineQueue: [],
      pairing_code: msg.code,
    };
    devices.set(deviceId, device);
    send(ws, { type: 'pair_pending', device_id: deviceId, token: device.token, expires_in: Math.max(0, pairing.expires_at - Date.now()) });
    // 通知桌面有配对请求
    sendToDevice(pairing.device_id, {
      type: 'pair_request',
      code: msg.code,
      apk_device_id: deviceId,
      apk_label: device.label,
    });
    return;
  }
  // token 模式（桌面 / 已配对 APK）
  if (msg.token) {
    for (const [, candidate] of devices) {
      if (candidate.token === msg.token && candidate.ws !== ws) {
        candidate.ws = ws;
        device = candidate;
        device.last_seen = Date.now();
        send(ws, { type: 'auth_ok', device_id: candidate.id, role: candidate.role });
        // 投递离线消息
        const queued = candidate.offlineQueue;
        candidate.offlineQueue = [];
        for (const item of queued) {
          send(ws, item);
        }
        return;
      }
    }
    send(ws, { type: 'auth_error', error: 'invalid token' });
    ws.close(4004, 'invalid token');
    return;
  }
  send(ws, { type: 'auth_error', error: 'auth requires token or code' });
  ws.close(4001, 'auth required');
}

function handleMessage(device, msg) {
  if (!allowMessage(device.id)) {
    send(device.ws, { type: 'error', error: 'rate limited' });
    return;
  }
  const target = String(msg.to || '');
  if (!target) {
    send(device.ws, { type: 'error', error: 'missing to' });
    return;
  }
  if (device.role === 'desktop') {
    // 桌面只能发给已配对的 APK（target 是 apk-xxx）
    const targetDevice = devices.get(target);
    if (!targetDevice || targetDevice.role !== 'apk') {
      send(device.ws, { type: 'error', error: `target ${target} not found` });
      return;
    }
    const delivered = sendToDevice(target, {
      type: 'message',
      from: device.id,
      body: msg.body || {},
      msg_id: msg.msg_id,
    });
    if (!delivered) {
      send(device.ws, { type: 'offline', to: target, msg_id: msg.msg_id });
    }
    return;
  }
  // APK：发给它的配对桌面（binding）
  const binding = devices.get(target);
  if (!binding || binding.role !== 'desktop') {
    send(device.ws, { type: 'error', error: 'desktop not found' });
    return;
  }
  const delivered = sendToDevice(target, {
    type: 'message',
    from: device.id,
    body: msg.body || {},
    msg_id: msg.msg_id,
  });
  if (!delivered) {
    send(device.ws, { type: 'offline', to: target, msg_id: msg.msg_id });
  }
}

// ─────────────────────────── REST ───────────────────────────

function readBody(req) {
  return new Promise((resolve) => {
    let body = '';
    req.on('data', (chunk) => {
      body += chunk;
      if (body.length > 16 * 1024) {
        req.destroy();
        resolve(null);
      }
    });
    req.on('end', () => {
      try {
        resolve(body ? JSON.parse(body) : {});
      } catch (_) {
        resolve(null);
      }
    });
  });
}

function json(res, status, payload) {
  const text = JSON.stringify(payload);
  res.writeHead(status, {
    'Content-Type': 'application/json; charset=utf-8',
    'Content-Length': Buffer.byteLength(text),
  });
  res.end(text);
}

async function handleHttp(req, res) {
  const url = new URL(req.url, `http://${req.headers.host || 'localhost'}`);
  const method = req.method || 'GET';

  if (method === 'GET' && url.pathname === '/health') {
    json(res, 200, { ok: true, service: 'hilia-relay', ts: Date.now() });
    return;
  }
  if (method === 'GET' && url.pathname === '/status') {
    const deviceList = [...devices.values()].map((d) => ({
      id: d.id,
      role: d.role,
      label: d.label,
      online: Boolean(d.ws && d.ws.readyState === 1),
      queued: d.offlineQueue.length,
    }));
    json(res, 200, { devices: deviceList, pairings: pairings.size });
    return;
  }
  if (method === 'POST' && url.pathname === '/pairing/register') {
    const body = await readBody(req);
    if (!body) {
      json(res, 400, { error: 'invalid json' });
      return;
    }
    const code = randomCode();
    const deviceId = uniqueDeviceId('desktop');
    const token = randomToken();
    devices.set(deviceId, {
      id: deviceId,
      role: 'desktop',
      label: String(body.label || 'Windows 桌面'),
      token,
      ws: null,
      last_seen: Date.now(),
      offlineQueue: [],
    });
    pairings.set(code, { device_id: deviceId, expires_at: Date.now() + PAIR_TTL_MS, confirmed: null });
    json(res, 200, { code, device_id: deviceId, token, expires_in: PAIR_TTL_MS / 1000 });
    return;
  }
  if (method === 'POST' && url.pathname === '/pairing/confirm') {
    const body = await readBody(req);
    if (!body || !body.code) {
      json(res, 400, { error: 'code required' });
      return;
    }
    const pairing = pairings.get(body.code);
    if (!pairing) {
      json(res, 404, { error: 'pairing not found' });
      return;
    }
    pairing.confirmed = body.accept !== false;
    if (!pairing.confirmed) {
      // 拒绝：清理该 code 下的 apk 会话
      for (const [id, device] of devices) {
        if (device.pairing_code === body.code) {
          send(device.ws, { type: 'pair_result', accepted: false });
          device.ws?.close(4005, 'rejected');
          devices.delete(id);
        }
      }
      pairings.delete(body.code);
      json(res, 200, { ok: true, accepted: false });
      return;
    }
    // 接受：告诉 APK 配对成功
    for (const [id, device] of devices) {
      if (device.pairing_code === body.code) {
        send(device.ws, { type: 'pair_result', accepted: true, desktop_id: pairing.device_id, token: device.token, device_id: device.id });
        device.pairing_code = null;
        device.binding = pairing.device_id;
      }
    }
    pairings.delete(body.code);
    json(res, 200, { ok: true, accepted: true });
    return;
  }

  json(res, 404, { error: 'not found' });
}

// ─────────────────────────── 启动 ───────────────────────────

const server = http.createServer(handleHttp);
const wss = new WebSocketServer({ server, path: '/ws' });
wss.on('connection', handleWs);

server.listen(PORT, () => {
  console.log(`hilia-relay listening on :${PORT} (ws /ws)`);
});
