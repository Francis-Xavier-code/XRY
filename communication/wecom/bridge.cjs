#!/usr/bin/env node
/**
 * 希尔娅 企业微信桥接层
 * 自建应用「接收消息」回调 → 本地 HTTP 服务（默认 127.0.0.1:4097），
 * 文本消息路由到 hilia ask，回复走企业微信发送 API。
 *
 * 部署要求：
 *  - 企业微信管理后台 → 应用管理 → 自建应用 → 接收消息 → 设置 API 接收
 *  - 回调 URL 需公网可访问（用 frp/花生壳/cloudflared 等穿透到本机端口，
 *    例如 https://你的域名/wecom → 127.0.0.1:4097）
 *  - Token 与 EncodingAESKey 由后台生成，与 hilia wecom config 保持一致
 *
 * 环境变量：HILIA_WECOM_CORP_ID / HILIA_WECOM_AGENT_ID / HILIA_WECOM_SECRET /
 *           HILIA_WECOM_TOKEN / HILIA_WECOM_AES_KEY / HILIA_WECOM_PORT /
 *           HILIA_BIN / HILIA_HOME / HILIA_BRIDGE_LOG
 */
'use strict';

const http = require('node:http');
const path = require('node:path');
const crypto = require('node:crypto');
const {
  SESSIONS_DIR,
  log,
  ensureSession,
  askGqy,
  splitReply,
  enqueueSession,
} = require('../lib/bridge-common.cjs');

const CORP_ID = process.env.HILIA_WECOM_CORP_ID || '';
const AGENT_ID = process.env.HILIA_WECOM_AGENT_ID || '';
const SECRET = process.env.HILIA_WECOM_SECRET || '';
const TOKEN = process.env.HILIA_WECOM_TOKEN || '';
const AES_KEY = process.env.HILIA_WECOM_AES_KEY || '';
const PORT = Number(process.env.HILIA_WECOM_PORT || 4097);
const LOG_FILE = process.env.HILIA_BRIDGE_LOG || path.join(process.env.USERPROFILE || process.env.HOME || '', 'hilia', 'wecom-bridge.log');

if (!CORP_ID || !AGENT_ID || !SECRET || !TOKEN || !AES_KEY) {
  log(LOG_FILE, '错误：企业微信配置不完整（corp_id/agent_id/secret/token/encoding_aes_key），先执行 hilia wecom config 设置');
  process.exit(1);
}

// ───────────────────────── 加解密（企业微信 AES-CBC） ─────────────────────────

function decryptMessage(encryptedBase64, aesKey) {
  const key = Buffer.from(aesKey + '=', 'base64');
  const iv = key.slice(0, 16);
  const decipher = crypto.createDecipheriv('aes-256-cbc', key, iv);
  decipher.setAutoPadding(false);
  let decrypted = Buffer.concat([decipher.update(Buffer.from(encryptedBase64, 'base64')), decipher.final()]);
  // 去掉 PKCS7 填充
  const padLen = decrypted[decrypted.length - 1];
  if (padLen > 0 && padLen <= 32) decrypted = decrypted.slice(0, decrypted.length - padLen);
  // 格式：random(16) + msg_len(4, 大端) + msg + receiveid
  if (decrypted.length < 20) throw new Error('解密结果过短');
  const msgLen = decrypted.readUInt32BE(16);
  const msg = decrypted.slice(20, 20 + msgLen).toString('utf8');
  return msg;
}

function verifySignature(token, timestamp, nonce, encrypt, signature) {
  const content = [token, timestamp, nonce, encrypt].sort().join('');
  const digest = crypto.createHash('sha1').update(content).digest('hex');
  return digest === signature;
}

// 从企业微信回调 XML 提取字段（自实现轻量解析，避免额外依赖）
function extractXml(xml, tag) {
  const match = xml.match(new RegExp(`<${tag}>\\s*<!\\[CDATA\\[([\\s\\S]*?)\\]\\]>\\s*</${tag}>`));
  if (match) return match[1];
  const plain = xml.match(new RegExp(`<${tag}>([\\s\\S]*?)</${tag}>`));
  return plain ? plain[1] : '';
}

// ───────────────────────── 发送消息 API ─────────────────────────

let accessToken = '';
let tokenExpiresAt = 0;

async function getAccessToken() {
  if (accessToken && Date.now() < tokenExpiresAt - 60000) return accessToken;
  const url = `https://qyapi.weixin.qq.com/cgi-bin/gettoken?corpid=${encodeURIComponent(CORP_ID)}&corpsecret=${encodeURIComponent(SECRET)}`;
  const res = await fetch(url);
  const data = await res.json();
  if (data.errcode !== 0) {
    throw new Error(`获取 access_token 失败: ${data.errcode} ${data.errmsg}`);
  }
  accessToken = data.access_token;
  tokenExpiresAt = Date.now() + data.expires_in * 1000;
  return accessToken;
}

async function sendText(toUser, text) {
  const token = await getAccessToken();
  const url = `https://qyapi.weixin.qq.com/cgi-bin/message/send?access_token=${encodeURIComponent(token)}`;
  const body = {
    touser: toUser,
    msgtype: 'text',
    agentid: Number(AGENT_ID),
    text: { content: text },
  };
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  const data = await res.json();
  if (data.errcode !== 0) {
    throw new Error(`发送消息失败: ${data.errcode} ${data.errmsg}`);
  }
}

// ───────────────────────── 消息处理 ─────────────────────────

async function handleText(fromUser, chatId, content) {
  const sessionKey = chatId && chatId !== fromUser
    ? `wecom-group-${chatId}`
    : `wecom-user-${fromUser}`;
  const sessionHome = path.join(SESSIONS_DIR, sessionKey);
  const identity = { platform: 'wecom', userId: fromUser, chatId: chatId || fromUser };

  log(LOG_FILE, `收到 ${sessionKey} 来自 ${fromUser}: ${content.slice(0, 120)}`);
  const reply = await enqueueSession(sessionKey, async () => {
    const extraEnv = ensureSession(sessionHome, LOG_FILE);
    return askGqy(content.slice(0, 2000), sessionHome, extraEnv, LOG_FILE, identity);
  });
  // 长回复分片（企业微信单条消息有限制）
  for (const part of splitReply(reply, 4000)) {
    await sendText(fromUser, part);
  }
  log(LOG_FILE, `回复 ${fromUser}: ${reply.slice(0, 120)}`);
}

// ───────────────────────── HTTP 回调服务器 ─────────────────────────

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  if (url.pathname !== '/wecom') {
    res.writeHead(404).end('not found');
    return;
  }
  const timestamp = url.searchParams.get('timestamp') || '';
  const nonce = url.searchParams.get('nonce') || '';
  const msgSignature = url.searchParams.get('msg_signature') || '';

  if (req.method === 'GET') {
    // URL 验证：校验签名后解密 echostr 原样返回
    const echostr = url.searchParams.get('echostr') || '';
    if (!verifySignature(TOKEN, timestamp, nonce, echostr, msgSignature)) {
      log(LOG_FILE, 'URL 验证签名校验失败');
      res.writeHead(403).end('signature mismatch');
      return;
    }
    try {
      const reply = decryptMessage(echostr, AES_KEY);
      res.writeHead(200, { 'Content-Type': 'text/plain' });
      res.end(reply);
    } catch (e) {
      log(LOG_FILE, `echostr 解密失败: ${e.message}`);
      res.writeHead(500).end('decrypt failed');
    }
    return;
  }

  if (req.method === 'POST') {
    let raw = '';
    req.on('data', (chunk) => { raw += chunk; });
    req.on('end', async () => {
      // 立即回 success，避免回调超时；回复走发送 API
      res.writeHead(200, { 'Content-Type': 'text/plain' });
      res.end('success');
      try {
        const encrypt = extractXml(raw, 'Encrypt');
        if (!verifySignature(TOKEN, timestamp, nonce, encrypt, msgSignature)) {
          log(LOG_FILE, '消息签名校验失败');
          return;
        }
        const xml = decryptMessage(encrypt, AES_KEY);
        const msgType = extractXml(xml, 'MsgType');
        const fromUser = extractXml(xml, 'FromUserName');
        const chatId = extractXml(xml, 'ChatId');
        const content = extractXml(xml, 'Content');
        if (msgType === 'text' && content && fromUser) {
          await handleText(fromUser, chatId, content);
        } else {
          log(LOG_FILE, `忽略非文本消息（MsgType=${msgType || '?'}）`);
        }
      } catch (e) {
        log(LOG_FILE, `回调处理失败: ${e.message}`);
      }
    });
    return;
  }

  res.writeHead(405).end();
});

server.listen(PORT, '127.0.0.1', () => {
  log(LOG_FILE, `企业微信桥接已启动，监听 http://127.0.0.1:${PORT}/wecom`);
});
