#!/usr/bin/env node
/**
 * 希尔娅 中继桥接层（桌面端）
 * 连接中继 WebSocket，把 APK 消息路由到 hilia ask，回复经中继返回。
 *
 * 流程：
 *  - 启动时读 HILIA_RELAY_TOKEN_FILE 里的 token 连接中继；
 *  - token 失效（中继重启）→ 重新 POST /pairing/register 注册并保存新 token；
 *  - 收到配对请求 pair_request → 调用面板 /api/pairing/request（本地，弹确认）；
 *  - 收到 message（来自 APK）→ hilia --bridge-platform apk ask → 回复回 APK。
 *
 * 环境变量：HILIA_RELAY_URL / HILIA_RELAY_TOKEN / HILIA_RELAY_DEVICE_ID /
 *           HILIA_PANEL_PASSWORD（可选）/ HILIA_BIN / HILIA_BRIDGE_LOG /
 *           HILIA_RELAY_TOKEN_FILE（token 持久化文件，自愈用）
 */
'use strict';

const path = require('node:path');
const fs = require('node:fs');
const {
  SESSIONS_DIR,
  log,
  ensureSession,
  askGqy,
  splitReply,
  enqueueSession,
} = require('../lib/bridge-common.cjs');

const RELAY_URL = process.env.HILIA_RELAY_URL || '';
const TOKEN = process.env.HILIA_RELAY_TOKEN || '';
const DEVICE_ID = process.env.HILIA_RELAY_DEVICE_ID || '';
const PANEL_PASSWORD = process.env.HILIA_PANEL_PASSWORD || '';
const LOG_FILE = process.env.HILIA_BRIDGE_LOG || path.join(process.env.USERPROFILE || process.env.HOME || '', 'hilia', 'relay-bridge.log');
const TOKEN_FILE = process.env.HILIA_RELAY_TOKEN_FILE || path.join(process.env.USERPROFILE || process.env.HOME || '', 'hilia', 'cache', 'relay-token.json');
const HILIA_BIN = process.env.HILIA_BIN || 'hilia';

if (!RELAY_URL) {
  log(LOG_FILE, '错误：未设置 HILIA_RELAY_URL（先执行 hilia relay config relay_url <wss://...>）');
  process.exit(1);
}

// 中继 REST 基地址（wss://host/ws → https://host）
function httpBase() {
  return RELAY_URL.replace(/^wss:\/\//, 'https://').replace(/^ws:\/\//, 'http://').replace(/\/ws$/, '');
}

let ws = null;
let reconnectDelay = 3000;

function readTokenFile() {
  try {
    const raw = fs.readFileSync(TOKEN_FILE, 'utf8');
    const data = JSON.parse(raw);
    if (data.token && data.device_id) return data;
  } catch (_) { /* ignore */ }
  return null;
}

function writeTokenFile(token, deviceId) {
  try {
    fs.mkdirSync(path.dirname(TOKEN_FILE), { recursive: true });
    fs.writeFileSync(TOKEN_FILE, JSON.stringify({ token, device_id: deviceId }));
  } catch (e) {
    log(LOG_FILE, `保存 token 失败: ${e.message}`);
  }
}

async function registerDesktop() {
  const hostname = process.env.COMPUTERNAME || os_hostname();
  const res = await fetch(`${httpBase()}/pairing/register`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ label: hostname }),
  });
  if (!res.ok) throw new Error(`register HTTP ${res.status}`);
  const data = await res.json();
  if (!data.token || !data.device_id) throw new Error('register 返回缺少 token/device_id');
  writeTokenFile(data.token, data.device_id);
  return data;
}

function os_hostname() {
  try { return require('node:os').hostname(); } catch (_) { return 'Windows'; }
}

/** 通知面板有配对请求（面板弹确认框）。面板无密码时本地 API 免认证。 */
async function notifyPanel(code, apkLabel, apkDeviceId) {
  try {
    const headers = { 'Content-Type': 'application/json' };
    if (PANEL_PASSWORD) {
      // 有面板密码：先登录拿 cookie
      const login = await fetch('http://127.0.0.1:4096/api/auth/login', {
        method: 'POST',
        headers,
        body: JSON.stringify({ password: PANEL_PASSWORD }),
      });
      const cookie = login.headers.get('set-cookie') || '';
      if (cookie) headers['Cookie'] = cookie.split(';')[0];
    }
    const res = await fetch('http://127.0.0.1:4096/api/pairing/request', {
      method: 'POST',
      headers,
      body: JSON.stringify({ code, apk_label: apkLabel, apk_device_id: apkDeviceId }),
    });
    if (!res.ok) {
      log(LOG_FILE, `面板配对通知失败 HTTP ${res.status}`);
    }
  } catch (e) {
    log(LOG_FILE, `面板配对通知失败: ${e.message}`);
  }
}

/** 面板本地 API（带密码时自动登录拿 cookie）。 */
async function panelPost(path, payload) {
  const headers = { 'Content-Type': 'application/json' };
  if (PANEL_PASSWORD) {
    const login = await fetch('http://127.0.0.1:4096/api/auth/login', {
      method: 'POST',
      headers,
      body: JSON.stringify({ password: PANEL_PASSWORD }),
    });
    const cookie = login.headers.get('set-cookie') || '';
    if (cookie) headers['Cookie'] = cookie.split(';')[0];
  }
  const res = await fetch(`http://127.0.0.1:4096${path}`, {
    method: 'POST',
    headers,
    body: JSON.stringify(payload),
  });
  return res;
}

/** 结构化消息（申报/审批/管理员确认等）→ 面板统一入口，与直连路径共用逻辑。 */
async function dispatchMobile(from, body) {
  try {
    const res = await panelPost('/api/mobile/dispatch', { device_id: from, body });
    const data = await res.json().catch(() => null);
    if (data && data.reply) return String(data.reply);
    return `出错了：${(data && data.error) || `HTTP ${res.status}`}`;
  } catch (e) {
    return `调用出错：${e.message}`;
  }
}

/** 处理来自 APK 的消息：结构化消息走面板统一入口，文本转 hilia ask，回复经中继返回。 */
async function handleApkMessage(msg) {
  const from = String(msg.from || '');
  const body = msg.body || {};
  const kind = String(body.kind || '');
  if (kind && kind !== 'text' && kind !== 'question') {
    // 结构化消息（credit_apply / admin_auth / credit_approve ...）
    log(LOG_FILE, `APK ${from} 结构化消息: ${kind}`);
    const reply = await dispatchMobile(from, body);
    for (const part of splitReply(reply, 4000)) {
      sendToRelay({ type: 'message', to: from, msg_id: msg.msg_id, body: { reply: part } });
    }
    return;
  }
  const question = String(body.text || body.question || '');
  if (!question) {
    sendToRelay({ type: 'message', to: from, msg_id: msg.msg_id, body: { reply: '（收到空消息）' } });
    return;
  }
  const sessionKey = `relay-apk-${from}`;
  const sessionHome = path.join(SESSIONS_DIR, sessionKey);
  // 身份：平台 apk，user_id = APK 设备 ID（学生绑定/管理员判定复用学分工具）
  const identity = { platform: 'apk', userId: from, chatId: from };
  log(LOG_FILE, `APK ${from} 消息: ${question.slice(0, 80)}`);
  const reply = await enqueueSession(sessionKey, async () => {
    const extraEnv = ensureSession(sessionHome, LOG_FILE);
    return askGqy(question.slice(0, 2000), sessionHome, extraEnv, LOG_FILE, identity);
  });
  // 长回复分片
  for (const part of splitReply(reply, 4000)) {
    sendToRelay({ type: 'message', to: from, msg_id: msg.msg_id, body: { reply: part } });
  }
}

function sendToRelay(payload) {
  if (ws && ws.readyState === 1) {
    ws.send(JSON.stringify(payload));
  }
}

function connect() {
  log(LOG_FILE, `连接中继 ${RELAY_URL} ...`);
  ws = new WebSocket(RELAY_URL);

  ws.onopen = async () => {
    log(LOG_FILE, '已连接中继，发送认证...');
    // token 优先；失效则重新注册
    let token = TOKEN;
    let deviceId = DEVICE_ID;
    const saved = readTokenFile();
    if (!token && saved) {
      token = saved.token;
      deviceId = saved.device_id;
    }
    if (!token) {
      try {
        const registered = await registerDesktop();
        token = registered.token;
        deviceId = registered.device_id;
        log(LOG_FILE, `已向中继重新注册：${registered.device_id}`);
      } catch (e) {
        log(LOG_FILE, `注册失败: ${e.message}`);
        ws.close();
        return;
      }
    }
    ws.send(JSON.stringify({ type: 'auth', token, device_id: deviceId }));
  };

  ws.onmessage = async (ev) => {
    let msg;
    try {
      msg = JSON.parse(String(ev.data));
    } catch (_) {
      return;
    }
    if (msg.type === 'auth_ok') {
      log(LOG_FILE, `认证成功：${msg.device_id}（${msg.role}）`);
      reconnectDelay = 3000;
      return;
    }
    if (msg.type === 'auth_error') {
      log(LOG_FILE, `认证失败：${msg.error}，尝试重新注册...`);
      ws.close();
      return;
    }
    if (msg.type === 'pair_request') {
      log(LOG_FILE, `收到配对请求：${msg.apk_label}（${msg.apk_device_id}）code=${msg.code}`);
      await notifyPanel(msg.code, msg.apk_label, msg.apk_device_id);
      return;
    }
    if (msg.type === 'message' && msg.from) {
      try {
        await handleApkMessage(msg);
      } catch (e) {
        log(LOG_FILE, `消息处理失败: ${e.message}`);
        sendToRelay({ type: 'message', to: msg.from, msg_id: msg.msg_id, body: { reply: `处理失败：${e.message}` } });
      }
      return;
    }
    if (msg.type === 'pong') {
      return;
    }
  };

  ws.onerror = (e) => log(LOG_FILE, 'WS 错误: ' + (e.message || e.type || 'unknown'));

  ws.onclose = () => {
    log(LOG_FILE, `连接断开，${reconnectDelay / 1000}s 后重连...`);
    setTimeout(connect, reconnectDelay);
  };
}

// 心跳：每 30s 发 ping
setInterval(() => {
  if (ws && ws.readyState === 1) {
    ws.send(JSON.stringify({ type: 'ping' }));
  }
}, 30 * 1000);

connect();
