#!/usr/bin/env node
/**
 * 希尔娅 飞书桥接层
 * 飞书开放平台自建应用 + 事件订阅长连接模式（WebSocket 主动连出，无需公网回调地址）。
 * 消息路由到 hilia ask，回复通过官方 API 发回。
 *
 * 依赖：官方 SDK @larksuiteoapi/node-sdk（首次部署执行 `npm ci`）
 *
 * 环境变量：HILIA_FEISHU_APP_ID / HILIA_FEISHU_APP_SECRET /
 *           HILIA_BIN / HILIA_HOME / HILIA_BRIDGE_LOG
 */
'use strict';

const path = require('node:path');
const lark = require('@larksuiteoapi/node-sdk');
const {
  SESSIONS_DIR,
  log,
  ensureSession,
  askGqy,
  splitReply,
  enqueueSession,
} = require('../lib/bridge-common.cjs');

const APP_ID = process.env.HILIA_FEISHU_APP_ID || '';
const APP_SECRET = process.env.HILIA_FEISHU_APP_SECRET || '';
const LOG_FILE = process.env.HILIA_BRIDGE_LOG || path.join(process.env.USERPROFILE || process.env.HOME || '', 'hilia', 'feishu-bridge.log');

if (!APP_ID || !APP_SECRET) {
  log(LOG_FILE, '错误：飞书配置不完整（app_id/app_secret），先执行 hilia feishu config 设置');
  process.exit(1);
}

const client = new lark.Client({ appId: APP_ID, appSecret: APP_SECRET, appType: lark.AppType.SelfBuild });

// 机器人自己的 open_id（用于群聊 @ 判定）
let botOpenId = '';

async function fetchBotInfo() {
  try {
    const res = await client.bot.info({});
    botOpenId = res.data?.bot?.open_id || '';
    log(LOG_FILE, `机器人信息：${res.data?.bot?.app_name || '?'}（open_id=${botOpenId}）`);
  } catch (e) {
    log(LOG_FILE, `获取机器人信息失败: ${e.message}`);
  }
}

// content 是 JSON 字符串：{ "text": "..." }；去掉 @_user_N 占位符
function parseText(content) {
  try {
    const parsed = JSON.parse(content);
    const text = (parsed.text || '').replace(/@_user_\d+/g, '').replace(/\s+/g, ' ').trim();
    return text;
  } catch (_) {
    return '';
  }
}

async function sendReply(messageId, text) {
  for (const part of splitReply(text, 4000)) {
    const res = await client.im.message.reply({
      path: { message_id: messageId },
      data: { msg_type: 'text', content: JSON.stringify({ text: part }) },
    });
    if (res.code !== 0) {
      throw new Error(`发送消息失败: code=${res.code} msg=${res.msg}`);
    }
  }
}

async function handleMessage(data) {
  const message = data.message;
  const sender = data.sender;
  if (!message || !sender) return;

  const chatId = message.chat_id || '';
  const chatType = message.chat_type || 'p2p'; // p2p | group
  const openId = sender.sender_id?.open_id || '';
  const text = parseText(message.content);

  if (message.message_type !== 'text' || !text) {
    log(LOG_FILE, `忽略非文本消息（type=${message.message_type || '?'}）`);
    return;
  }

  // 群聊：只在 @ 机器人时响应
  if (chatType === 'group') {
    const mentions = message.mentions || [];
    const atBot = mentions.some((m) => m.id?.open_id === botOpenId);
    if (!atBot) {
      log(LOG_FILE, `群 ${chatId} 消息忽略（未@机器人）: ${text.slice(0, 60)}`);
      return;
    }
  }

  const sessionKey = chatType === 'group' ? `feishu-group-${chatId}` : `feishu-user-${openId}`;
  const sessionHome = path.join(SESSIONS_DIR, sessionKey);
  const identity = { platform: 'feishu', userId: openId, chatId: chatId || openId };

  log(LOG_FILE, `收到 ${sessionKey} 来自 ${openId}: ${text.slice(0, 120)}`);
  const reply = await enqueueSession(sessionKey, async () => {
    const extraEnv = ensureSession(sessionHome, LOG_FILE);
    return askGqy(text.slice(0, 2000), sessionHome, extraEnv, LOG_FILE, identity);
  });
  await sendReply(message.message_id, reply);
  log(LOG_FILE, `回复 ${openId}: ${reply.slice(0, 120)}`);
}

async function main() {
  await fetchBotInfo();
  const eventDispatcher = new lark.EventDispatcher({}).register({
    'im.message.receive_v1': async (data) => {
      try {
        await handleMessage(data);
      } catch (e) {
        log(LOG_FILE, `消息处理失败: ${e.message}`);
      }
    },
  });
  const wsClient = new lark.ws.Client({ appId: APP_ID, appSecret: APP_SECRET, loggerLevel: lark.LoggerLevel.WARN });
  log(LOG_FILE, '飞书长连接启动中...');
  wsClient.start({ eventDispatcher });
}

main().catch((e) => {
  log(LOG_FILE, `启动失败: ${e.message}`);
  process.exit(1);
});
