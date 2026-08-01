# communication/ — 消息平台桥接层源码

这里只放**桥接层的源代码**，不放运行时二进制、日志、会话数据。

## 目录结构

```
communication/
├── README.md
├── lib/
│   └── bridge-common.cjs   # 公共模块：askGqy / 会话隔离 / 输出清理 / 回复分片 / 串行队列
├── napcat/
│   └── bridge.cjs          # OneBot 11 WebSocket 客户端（QQ，NapCat）
├── wecom/
│   └── bridge.cjs          # 企业微信自建应用回调桥接（本地 HTTP + 发送 API）
└── feishu/
    ├── bridge.cjs          # 飞书长连接桥接（官方 SDK，无需公网地址）
    └── package.json        # 依赖 @larksuiteoapi/node-sdk（首次部署 `npm ci`）
```

## 部署约定（Windows）

- 源码以 git 管理，运行时产物**不要**提交进仓库。
- 三个平台都通过 `hilia napcat|wecom|feishu install` 注册**开机自启计划任务**（schtasks ONLOGON），
  登录后自动运行；`hilia <platform> status` 查看状态，`uninstall` 移除自启动。
- 启动脚本（设环境变量后执行 node bridge.cjs）写在 `%LOCALAPPDATA%\hilia\config\bridges\`，
  日志写 `%LOCALAPPDATA%\hilia\cache\logs\<platform>-bridge.log`。
- NapCat 本体（QQ 客户端 + NapCat 插件）在 Windows 上自行安装（NapCat.QQ 一键版），
  桥接连接 `ws://127.0.0.1:3001`（NapCat 默认 WebSocket 端口）。
- 会话数据：`%LOCALAPPDATA%\hilia\sessions\<平台>-<会话>`（每个会话一个独立 HILIA_HOME）。

## 环境变量

| 变量 | 默认 | 说明 |
|---|---|---|
| `HILIA_WS_URL` | `ws://127.0.0.1:3001` | NapCat OneBot WebSocket 地址 |
| `HILIA_SELF_ID` | 空 | 你的 QQ 号；未设置时**群聊 @ 响应不可用**（启动有警告） |
| `HILIA_WECOM_CORP_ID` | 空 | 企业微信企业 ID（wecom 必需） |
| `HILIA_WECOM_AGENT_ID` | 空 | 企业微信自建应用 AgentId |
| `HILIA_WECOM_SECRET` | 空 | 企业微信自建应用 Secret |
| `HILIA_WECOM_TOKEN` | 空 | 企业微信回调 Token（后台生成） |
| `HILIA_WECOM_AES_KEY` | 空 | 企业微信回调 EncodingAESKey |
| `HILIA_WECOM_PORT` | `4097` | 本地回调端口（需内网穿透映射到公网） |
| `HILIA_FEISHU_APP_ID` | 空 | 飞书自建应用 App ID（feishu 必需） |
| `HILIA_FEISHU_APP_SECRET` | 空 | 飞书自建应用 App Secret |
| `HILIA_BIN` | `hilia` | hilia 可执行文件路径 |
| `HILIA_TIMEOUT_MS` | `120000` | 单次 ask 超时（超时自动终止并回提示） |
| `HILIA_SESSIONS_DIR` | `%LOCALAPPDATA%\hilia\sessions` | 隔离会话根目录 |
| `HILIA_BRIDGE_LOG` | 平台默认日志 | 日志路径 |

## 会话隔离设计

- **每个隔离会话一个独立 HILIA_HOME**：对话历史（conversation.db）、记忆（memory.db）互不串扰。
  私聊按用户隔离（`qq-private-<id>` / `wecom-user-<id>` / `feishu-user-<id>`），
  群聊按群隔离（`qq-group-<id>` / `wecom-group-<id>` / `feishu-group-<id>`）。
- **同一会话的消息串行处理**（`enqueueSession`）：同一 HILIA_HOME 下同一时刻只有一个 hilia 进程，
  避免并发读写 SQLite 的竞态与回复乱序。
- **密钥不进会话目录**：首次创建会话时复制主 `config.jsonc`，但 api_key/token/password 等敏感字段
  会被替换为 `$env:HILIA_BRIDGE_KEY_n` 引用（hilia 原生支持 `$env:` 引用），真实密钥只经进程环境
  注入子进程。主配置更新（换模型/换 key）后，会话自动跟随，无需重建。
- 会话目录权限私有；主 HILIA_HOME（辅导员上下文）永远不写入会话目录。

## 身份上下文（辅导员 / 学生）

桥接调用 `hilia ask` 时带上：

- `--bridge-platform qq|wecom|feishu`
- `--bridge-user-id <平台用户ID>`（QQ 号 / 企业微信 userid / 飞书 open_id）
- `--bridge-chat-id <群号或用户ID>`

学分等权限工具据此判断提问者身份：`bridges.json` 的 `admins` 映射中列出的平台 ID 是辅导员
（管理员），其余为普通学生。学生只能查询自己的学分与信息。

配置辅导员（示例，`%LOCALAPPDATA%\hilia\config\bridges.json`）：

```json
{
  "napcat": { "ws_url": "ws://127.0.0.1:3001", "self_id": "机器人QQ号" },
  "wecom": { "corp_id": "...", "agent_id": "...", "secret": "...", "token": "...", "encoding_aes_key": "..." },
  "feishu": { "app_id": "...", "app_secret": "..." },
  "admins": {
    "qq": ["辅导员QQ号"],
    "wecom": ["辅导员企业微信userid"],
    "feishu": ["辅导员飞书open_id"]
  }
}
```

## 平台接入

### QQ（NapCat）

1. 安装运行 NapCat（NapCat.QQ 一键版），确保监听 `ws://127.0.0.1:3001`。
2. `hilia napcat config self_id <机器人QQ号>`
3. `hilia napcat install` → 计划任务开机自启。
4. 群聊里学生 `@机器人` 提问；私聊直接发消息。

### 企业微信

1. 企业微信管理后台 → 应用管理 → 创建自建应用，开启「接收消息」，生成 Token 与 EncodingAESKey。
2. `hilia wecom config corp_id|agent_id|secret|token|encoding_aes_key <值>`
3. `hilia wecom install`
4. **回调 URL 需要公网可访问**：用 frp / 花生壳 / cloudflared 把
   `https://你的域名/wecom` 穿透到 `127.0.0.1:4097`，把该 URL 填进后台「API 接收」。
5. 应用加入班级群后，群里 `@应用` 提问即可；学生也可在企业微信里单独给应用发消息。

### 飞书

1. 飞书开放平台 → 开发者后台 → 创建企业自建应用，开启「机器人」能力。
2. 事件订阅选择「长连接模式」，订阅 `im.message.receive_v1`。
3. `hilia feishu config app_id|app_secret <值>`
4. 首次部署安装 SDK：`cd <bridge目录>/feishu && npm ci`
5. `hilia feishu install` —— **无需公网地址**，适合个人电脑直接部署。

## 修改流程

改完对应 `bridge.cjs` 后重启桥接生效：

```powershell
# 在计划任务里重新触发，或直接结束进程后由 KeepAlive/计划任务重启
schtasks /Run /TN HiliaNapcatBridge
# 彻底重建：
hilia napcat uninstall; hilia napcat install
```

冒烟测试（本地、无需网络）：

```bash
node -e "const m = require('./lib/bridge-common.cjs'); console.log(m.splitReply('你好'.repeat(2001), 4000).length)"
```
