//! 消息平台桥接管理（hilia napcat / hilia wecom / hilia feishu）。
//!
//! 把 communication/ 里的桥接从「手工部署」变成 CLI 可管理：
//! - 配置统一存 HILIA_HOME/config/bridges.json（token/self_id/ws/admins 等）
//! - Windows：schtasks 计划任务自启动（ONLOGON）；macOS/Linux 开发环境：LaunchAgent
//! - 桥接脚本来源：安装版为 share/hilia/bridges/，源码开发时为仓库 communication/
//!
//! 身份上下文：桥接把平台用户 ID 通过 `--bridge-platform/--bridge-user-id/
//! --bridge-chat-id` 传给 hilia，学分等权限工具据此判断辅导员/学生。

pub mod feishu;
pub mod napcat;
pub mod wecom;

use crate::paths::GqyPaths;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub const BRIDGES_FILE: &str = "bridges.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BridgesConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub napcat: Option<NapcatConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wecom: Option<WecomConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feishu: Option<FeishuConfig>,
    /// 各平台管理员（辅导员）ID 列表：{ "qq": ["123456"], "wecom": ["userid"], "feishu": ["open_id"] }
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub admins: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NapcatConfig {
    #[serde(default = "default_ws_url")]
    pub ws_url: String,
    #[serde(default)]
    pub self_id: String,
    #[serde(default)]
    pub bin: String,
    #[serde(default)]
    pub install_dir: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WecomConfig {
    #[serde(default)]
    pub corp_id: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub secret: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub encoding_aes_key: String,
    #[serde(default)]
    pub callback_port: u16,
    #[serde(default)]
    pub bin: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuConfig {
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub app_secret: String,
    #[serde(default)]
    pub bin: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_ws_url() -> String {
    "ws://127.0.0.1:3001".to_string()
}

fn default_enabled() -> bool {
    true
}

pub fn bridges_file(paths: &GqyPaths) -> PathBuf {
    paths.config_dir.join(BRIDGES_FILE)
}

pub fn load(paths: &GqyPaths) -> Result<BridgesConfig> {
    let file = bridges_file(paths);
    if !file.exists() {
        return Ok(BridgesConfig::default());
    }
    let text = std::fs::read_to_string(&file)?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", file.display()))
}

pub fn save(paths: &GqyPaths, config: &BridgesConfig) -> Result<()> {
    std::fs::create_dir_all(&paths.config_dir)?;
    let file = bridges_file(paths);
    let temp = tempfile::NamedTempFile::new_in(&paths.config_dir)?;
    std::fs::write(temp.path(), serde_json::to_string_pretty(config)?)?;
    temp.persist(file)?;
    Ok(())
}

/// 某平台用户是否为管理员（辅导员）。本地（CLI/面板）默认是管理员。
/// 由学分工具（src/tools/credits.rs）在权限判定时调用。
#[allow(dead_code)]
pub fn is_admin(config: &BridgesConfig, platform: &str, user_id: &str) -> bool {
    if platform.is_empty() || user_id.is_empty() {
        return false;
    }
    config
        .admins
        .get(platform)
        .map(|ids| ids.iter().any(|id| id == user_id))
        .unwrap_or(false)
}

/// 桥接脚本目录：安装版 = share/hilia/bridges；源码开发 = 仓库 communication/。
pub fn bridges_dir(paths: &GqyPaths) -> PathBuf {
    let share_bridges = paths.share_dir.join("bridges");
    if share_bridges.join("napcat/bridge.cjs").is_file() {
        return share_bridges;
    }
    // 源码树：share_dir 在源码模式下指向仓库根
    let source_bridges = paths.share_dir.join("communication");
    if source_bridges.join("napcat/bridge.cjs").is_file() {
        return source_bridges;
    }
    share_bridges
}

pub fn bridge_script(paths: &GqyPaths, platform: &str) -> Result<PathBuf> {
    let path = bridges_dir(paths).join(platform).join("bridge.cjs");
    if !path.is_file() {
        bail!(
            "找不到桥接脚本 {}（安装版请确认随包文件，或从源码克隆 communication/）",
            path.display()
        );
    }
    Ok(path)
}

pub fn node_bin() -> String {
    std::env::var("HILIA_NODE_BIN")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "node".to_string()
            } else {
                "/opt/homebrew/bin/node".to_string()
            }
        })
}

/// 桥接自启动目录：各平台启动脚本/plist 放在这里。
pub fn bridge_launchers_dir(paths: &GqyPaths) -> Result<PathBuf> {
    let dir = paths.config_dir.join("bridges");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

// ─────────────────────── 自启动服务管理（schtasks / LaunchAgent） ───────────────────────

/// 注册开机自启服务。
/// - Windows：schtasks ONLOGON，指向启动脚本（.cmd）
/// - 其他（开发）：LaunchAgent plist
pub fn service_install(label: &str, program: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let output = Command::new("schtasks")
            .args([
                "/Create",
                "/TN",
                label,
                "/TR",
                &format!("\"{}\"", program.display()),
                "/SC",
                "ONLOGON",
                "/RL",
                "LIMITED",
                "/F",
            ])
            .output()
            .context("running schtasks /Create")?;
        if !output.status.success() {
            bail!(
                "schtasks /Create 失败: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (label, program);
        bail!("service_install 仅支持 Windows 计划任务；开发环境请使用 launchctl_install")
    }
}

/// 查询自启动服务是否已注册/运行。
#[cfg(windows)]
pub fn service_status(label: &str) -> Result<bool> {
    let output = Command::new("schtasks")
        .args(["/Query", "/TN", label])
        .output()
        .context("running schtasks /Query")?;
    Ok(output.status.success())
}

#[cfg(not(windows))]
pub fn service_status(label: &str) -> Result<bool> {
    let output = Command::new("/bin/launchctl")
        .args(["print", &format!("{}/{}", launchctl_target(), label)])
        .output()
        .context("running launchctl print")?;
    Ok(output.status.success())
}

/// 移除自启动服务。
#[cfg(windows)]
pub fn service_uninstall(label: &str) -> Result<()> {
    let output = Command::new("schtasks")
        .args(["/Delete", "/TN", label, "/F"])
        .output()
        .context("running schtasks /Delete")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not found") || stderr.contains("找不到") {
            return Ok(());
        }
        bail!("schtasks /Delete 失败: {}", stderr.trim());
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn service_uninstall(label: &str) -> Result<()> {
    let output = Command::new("/bin/launchctl")
        .args(["bootout", &format!("{}/{}", launchctl_target(), label)])
        .output()
        .context("running launchctl bootout")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Could not find service") || stderr.contains("No such process") {
            return Ok(());
        }
        bail!("launchctl bootout 失败: {}", stderr.trim());
    }
    Ok(())
}

// ─────────────────────────── macOS/Linux 开发环境：LaunchAgent ───────────────────────────

#[cfg(not(windows))]
pub fn launchctl_target() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

/// 开发环境（macOS/Linux）安装 LaunchAgent：写 plist 并 bootstrap。
#[cfg(not(windows))]
pub fn launchctl_install(label: &str, plist: &serde_json::Value) -> Result<()> {
    let plist_path = launch_agents_dir()?.join(format!("{label}.plist"));
    write_plist(&plist_path, plist)?;
    let output = Command::new("/bin/launchctl")
        .args(["bootstrap", &launchctl_target(), plist_path.to_str().unwrap_or_default()])
        .output()
        .context("running launchctl bootstrap")?;
    if !output.status.success() {
        // 已加载时 bootstrap 会报错，视为成功
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("service already loaded") || stderr.contains("already bootstrapped") {
            return Ok(());
        }
        bail!("launchctl bootstrap 失败: {}", stderr.trim());
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn write_plist(path: &Path, plist: &serde_json::Value) -> Result<()> {
    let xml = plist_to_xml(plist).context("serializing LaunchAgent plist")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, xml)?;
    Ok(())
}

/// 把 JSON 结构转成 LaunchAgent XML plist（只支持本模块用到的类型）。
#[cfg(not(windows))]
pub fn plist_to_xml(value: &serde_json::Value) -> Result<String> {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n",
    );
    append_plist_value(&mut out, value, 1)?;
    out.push_str("</plist>\n");
    Ok(out)
}

#[cfg(not(windows))]
fn append_plist_value(out: &mut String, value: &serde_json::Value, depth: usize) -> Result<()> {
    let indent = "    ".repeat(depth);
    match value {
        serde_json::Value::Object(map) => {
            out.push_str(&format!("{indent}<dict>\n"));
            for (key, value) in map {
                out.push_str(&format!("{}{indent}<key>{}</key>\n", "    ", key));
                append_plist_value(out, value, depth + 1)?;
            }
            out.push_str(&format!("{indent}</dict>\n"));
        }
        serde_json::Value::Array(items) => {
            out.push_str(&format!("{indent}<array>\n"));
            for item in items {
                append_plist_value(out, item, depth + 1)?;
            }
            out.push_str(&format!("{indent}</array>\n"));
        }
        serde_json::Value::String(text) => {
            out.push_str(&format!("{indent}<string>{}</string>\n", xml_escape(text)));
        }
        serde_json::Value::Bool(flag) => {
            out.push_str(&format!(
                "{indent}<{} />\n",
                if *flag { "true" } else { "false" }
            ));
        }
        serde_json::Value::Number(number) => {
            out.push_str(&format!("{indent}<integer>{number}</integer>\n"));
        }
        _ => bail!("unsupported plist value: {value}"),
    }
    Ok(())
}

#[cfg(not(windows))]
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(not(windows))]
fn launch_agents_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/Users/Shared"));
    let dir = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir).context("creating LaunchAgents directory")?;
    Ok(dir)
}

/// Windows 启动脚本（.cmd）：设好环境变量后执行 node bridge.cjs。
#[cfg(windows)]
pub fn write_launcher_cmd(path: &Path, env: &[(&str, String)], program: &str) -> Result<()> {
    let mut content = String::from("@echo off\r\n");
    for (key, value) in env {
        content.push_str(&format!("set {key}={}\r\n", cmd_escape(value)));
    }
    content.push_str(&format!("\"{program}\"\r\n"));
    std::fs::write(path, content)?;
    Ok(())
}

#[cfg(windows)]
fn cmd_escape(value: &str) -> String {
    value.replace('%', "%%")
}

/// 平台标签 -> 日志文件（放 HILIA_HOME/cache/logs/ 下，随备份）。
pub fn bridge_log_path(paths: &GqyPaths, platform: &str) -> PathBuf {
    paths.logs_dir().join(format!("{platform}-bridge.log"))
}

// ─────────────────────────── 桥接身份上下文 ───────────────────────────

/// 当前进程的桥接身份（由 CLI `--bridge-*` 参数注入；CLI/面板本地为管理员）。
#[derive(Debug, Clone, Default)]
pub struct BridgeIdentity {
    pub platform: String,
    pub user_id: String,
    pub chat_id: String,
}

static CURRENT_IDENTITY: OnceLock<BridgeIdentity> = OnceLock::new();

pub fn set_identity(identity: BridgeIdentity) {
    let _ = CURRENT_IDENTITY.set(identity);
}

pub fn current_identity() -> &'static BridgeIdentity {
    CURRENT_IDENTITY.get_or_init(BridgeIdentity::default)
}

/// 当前是否为桥接消息（平台 + 用户 ID 都非空）。
pub fn is_bridged() -> bool {
    let identity = current_identity();
    !identity.platform.is_empty() && !identity.user_id.is_empty()
}
