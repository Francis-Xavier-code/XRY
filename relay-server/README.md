# relay-server — 希尔娅中继服务器

APK（Android）与 Windows 端之间走互联网通信的中继：Windows 在 NAT 后没有公网 IP，
所有消息经本服务器转发。

## 协议总览

- **WebSocket** `/ws`：设备连接 + 消息转发（JSON 帧）
  - 连接后第一条消息必须 `auth`（token 或配对码）
  - 心跳：每 30s `{type:"ping"}`，90s 无心跳断开
  - 消息：`{type:"message", to:"<device_id>", body:{...}, msg_id:"<uuid>"}`
- **REST**：
  - `POST /pairing/register`（桌面生成配对码）→ `{code, device_id, token, expires_in}`
  - `POST /pairing/confirm`（桌面确认/拒绝配对）
  - `GET /status`、`GET /health`

## 配对流程（扫码）

1. **Windows 面板** →「生成配对二维码」→ 调用 `POST /pairing/register` 拿到 `code`
   （一次性，5 分钟过期），二维码内容：`hilia://pair?relay=wss://你的域名/ws&code=xxx`
2. **APK** 扫码 → 连接中继 WS → `{type:"auth", code}` → 收到 `pair_pending`
   → 中继同时向桌面推送 `pair_request`（面板弹确认框）
3. **Windows 面板** 确认 → `POST /pairing/confirm {code, accept:true}`
   → 中继向 APK 回 `pair_result{accepted:true, desktop_id}`
4. APK 之后的 `message` 消息 `to` 填 `desktop_id` 即可与 Windows 端对话

## 部署（VPS，推荐）

```bash
cd relay-server
npm ci
npm start          # 监听 8787
```

生产必须置于 TLS 反向代理后（否则扫码的 wss:// 连不上）：

**Caddy（最省事）**：

```
your.domain.com {
    reverse_proxy 127.0.0.1:8787
}
```

**Nginx**：

```nginx
server {
    listen 443 ssl;
    server_name your.domain.com;
    # ssl_certificate ...; ssl_certificate_key ...;
    location / {
        proxy_pass http://127.0.0.1:8787;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_read_timeout 300s;
    }
}
```

## 部署（Cloudflare Workers 适配）

本实现使用 Node `ws` 库。Workers 使用原生 WebSocket（`ws` 事件模型不同），
需要把 `handleWs` 迁移为 `worker.onmessage` 风格，并将 `devices/pairings` 状态
放进 `Durable Objects`（否则多实例状态不同步）。状态逻辑（配对/消息路由/限流）
可直接复用本文件，仅传输层不同。建议单机 VPS 部署为主。

## 环境变量

| 变量 | 默认 | 说明 |
|---|---|---|
| `PORT` | `8787` | HTTP/WS 监听端口 |
| `PAIR_TTL_MS` | `300000` | 配对码有效期（5 分钟） |

## 安全说明

- 配对码一次性 + 5 分钟过期；session token 32 字节随机
- 消息大小上限 64KB；每设备限流 10 条/秒
- **必须** WSS（TLS）；不要在公网裸跑 HTTP/WS
- 中继只做转发，不存消息内容（离线消息仅内存暂存 100 条/设备，桌面重连即投递）
