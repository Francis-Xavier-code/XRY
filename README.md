# 希尔娅 Hilia —— Windows 班级学分管理 AI 助理

希尔娅（Hilia）是一个运行在 Windows 上的 AI 助理，住在你的终端、系统托盘和面板里。

她的主业是**班级学分管理**：为大学辅导员提供学分记录的新增、修改、删除与查询，为学生提供个人学分与信息的查询。同时她也像任何一位靠谱的助理一样，帮你解答 Windows 使用问题、软件折腾、日常事务。

- 服务对象：大学辅导员（管理员）与学生
- 通信渠道：QQ（NapCat）、企业微信、飞书
- 开发者：

## 功能一览

| 功能 | 说明 |
|---|---|
| 学分管理 | 班级/学生/学分记录增删改查、按类型/学期汇总、CSV 批量导入 |
| 信息查询 | 学生按学号/姓名/班级查询；学生自助绑定后可查自己的学分 |
| 面板 | 浏览器内的对话面板（聊天 + 设置 + 学分管理页） |
| 迷你对话 | 独立小窗口，Alt+G 随时呼出 |
| 托盘 | 系统托盘菜单：面板/迷你/配置/备份/主目录/开机自启/检查更新 |
| 终端 | PowerShell 里直接输入 `hilia` 对话，或 `hilia "问题"` 单次问答 |
| Android App | 学生端 APK：扫码配对 → 查学分 / 与希尔娅对话（走互联网中继） |
| 三平台接入 | QQ（NapCat）/ 企业微信 / 飞书 群聊 @机器人 提问 |
| JSON 更新 | update.json 签名清单 + 多 GitHub 加速源 + 上游切换 + 强制更新 |
| 付费预留 | 离线激活码（Ed25519 签名）/ 在线订阅接口占位 / 功能门控 |
| 记忆 | 对话记忆、心情日志、自动归档（数据全在本地） |
| 知识库 | 内置 Windows 维护 + 学分系统使用手册，可继续追加 |

## 安装（Windows 10/11 x64）

1. 从 GitHub Releases 下载 `Hilia-x.y.z-windows.zip`
2. 解压后以管理员身份运行 `install.ps1`（安装到 `C:\hilia`，创建开始菜单快捷方式）
3. 启动托盘程序 `hilia-tray.exe`，右键菜单 →「打开面板」开始使用

### 手动方式

把 zip 解压到任意目录，直接运行：

```
hilia.exe            # 命令行对话（PowerShell 里输入 hilia 或 hilia "你好"）
hilia-tray.exe       # 系统托盘
hilia web            # 只启动面板服务（默认 http://127.0.0.1:4096）
```

### 需要 Node.js（仅桥接需要）

QQ/企业微信/飞书/APK 中继桥接由 Node.js 运行（Node ≥ 22）。不需要桥接的话，只装希尔娅即可。

### Android App（学生端）

从 GitHub Releases 下载 `Hilia-Android-x.y.z.apk` 安装（允许未知来源应用）：

1. 打开 App →「扫码配对」扫描 Windows 面板 → 设置 → 设备配对 → 生成的二维码
2. 辅导员在面板弹出的确认框点「允许」
3. 配对成功后即可在手机上查学分、和希尔娅对话

> 手机与电脑之间走互联网中继（relay-server），需要先部署中继并配置 `hilia relay`，详见 `docs/01-指南/中继部署指南.md`。

## 学分管理快速上手（辅导员）

1. 打开面板 → 左侧「学分管理」
2. 左栏「+ 新建班级」，如 计科2301
3. 「+ 添加学生」逐个录入，或「CSV 导入」批量导入
   （CSV 每行：`学号,姓名,班级,性别,电话`）
4. 「+ 记学分」录加分；分值填负数即扣分
5. 学生发消息给机器人「绑定 学号 姓名」完成绑定后，即可自助查询

也可以在对话里直接说：

- 「给 2023010101 加 2 分 志愿公益」
- 「查一下计科2301这学期的学分汇总」
- 「把张三昨天那条记录删了」

## 通信接入（QQ / 企业微信 / 飞书）

三个平台共用一套桥接框架，配置在 `%LOCALAPPDATA%\hilia\config\bridges.json`：

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

| 平台 | 步骤 | 要点 |
|---|---|---|
| QQ | `hilia napcat config self_id <QQ号>` → `hilia napcat install` | 先装好 NapCat（监听 3001 端口）；群聊 @机器人 提问 |
| 企业微信 | `hilia wecom config ...` → `hilia wecom install` | 自建应用回调，**需公网地址**（frp/花生壳/cloudflared 穿透到 4097 端口） |
| 飞书 | `hilia feishu config ...` → `cd <bridges>/feishu && npm ci` → `hilia feishu install` | 官方 SDK 长连接，**无需公网地址** |

详细说明见 `communication/README.md` 与 `docs/01-指南/通信接入指南.md`。

### APK 中继（互联网通信）

```powershell
hilia relay config relay_url wss://你的中继域名/ws
hilia relay install
```

中继服务器代码在 `relay-server/`，部署文档见 `docs/01-指南/中继部署指南.md`。

## 版本更新（update.json）

- `hilia update check`：拉取签名清单（update.json，多加速源轮询）对比版本
- `hilia update apply`：下载 → sha256 + Ed25519 签名校验 → 自动替换并重启
- 默认上游：`https://raw.githubusercontent.com/Francis-Xavier-code/XRY/main/update.json`
- 加速源：内置 ghproxy / gh-proxy / ghfast 等，`hilia config set update.mirrors [...]` 可自定义
- 上游切换：`hilia config set update.upstream_url <新地址>`，或 update.json 的 `next` 字段提示
- 强制更新：update.json 的 `min_version` 或 `hilia config set update.force true`
- 启动时自动检查（面板收到提示）；清单无签名一律拒绝
- Android 客户端从 update.json 的 `apk` 段（或 update-apk.json）检查更新

## 激活与授权（预留付费能力）

- `hilia license status`：查看激活状态与功能门控
- `hilia license activate <激活码>`：输入开发者签发的激活码（HILIA1.xxx.yyy）
- 激活码由开发者私钥签发（`hilia keys sign-license`），客户端内置公钥验签
- 在线订阅接口已预留（`license.server`），接入支付后即可启用
- 当前免费版可用全部基础功能；`multi_device` 等多设备特性预留门控

> 防逆向说明：发布二进制 strip + 字符串编译期混淆（obfstr）+ 激活码/更新清单 Ed25519 签名验证 + 下载包 sha256 校验。私钥只存在开发者侧（GitHub Secrets），客户端仅有公钥，因此伪造激活码或篡改更新包在签名层即被拒绝。完全防止本地破解不可能，关键价值（激活、更新、授权）全部建立在签名与服务器侧。

## Android 端安全

- 扫码内容为一次性配对码（5 分钟过期），配对需辅导员在面板确认
- 消息经中继转发，中继不落盘；生产部署必须启用 WSS/TLS
- APK 内置与 Windows 端相同的签名公钥，拒绝未签名更新

## 数据与隐私

- 所有数据都在本机：`%LOCALAPPDATA%\hilia\`（对话、记忆、学分数据库）
- 学分数据库：`%LOCALAPPDATA%\hilia\data\credit.db`
- 定期备份：`hilia backup now`（或托盘菜单「立即备份」）
- 平台消息不经过任何第三方服务器（模型 API 除外）

## 开发构建

编译由 GitHub Actions 远程完成（windows-latest），推送 `v*` 标签自动发布 Release：

- 工作流：`.github/workflows/build-windows.yml`
- 本地（macOS/Linux 开发机）也可构建核心：`cargo build --release`

```
cargo build --release          # 核心 hilia
cd windows/tray && cargo build --release   # 托盘（仅 Windows）
cargo test --bin hilia -- credits::   # 学分模块测试
```

## 常见问题

**面板打不开？** 托盘菜单 → 重启面板服务；或手动运行 `hilia web` 看报错。

**学生说「查学分」没反应？** 群聊需先 @机器人；检查是否已绑定（发「绑定 学号 姓名」）。

**桥接收不到消息？** `hilia napcat status` / `wecom status` / `feishu status` 看自启动状态与最近日志。

## 许可证

GPL-3.0，见 `LICENSE`。本项目 fork 自 [Miyu](https://github.com/SHORiN-KiWATA/Miyu)（MIT License），上游 MIT 部分仍按 MIT 授权，新增代码与修改部分按 GPL-3.0 授权。
