//! `hilia relay`：中继桥接管理（APK ↔ Windows 互联网通信）。
//!
//! 子命令：
//! - `status`：查看配置与自启动状态
//! - `install`：向中继注册桌面设备 → 安装自启动（计划任务 / LaunchAgent）
//! - `uninstall`：移除自启动（不删配置与数据）
//! - `config <key> <value>`：设置 relay_url / panel_password / bin / enabled
//!
//! 原理：桌面端连接中继 WebSocket（wss），APK 扫码配对后消息经中继转发；
//! 身份判定平台为 `apk`，复用 bridges.json 的 admins / 学生绑定（学分工具零改动）。
//! 中继服务器代码见 relay-server/。

use super::RelayConfig;
use crate::paths::GqyPaths;
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde_json::json;
use std::time::Duration;

const BRIDGE_LABEL: &str = "HiliaRelayBridge";

#[derive(Debug, Args)]
pub struct RelayArgs {
    #[command(subcommand)]
    pub command: Option<RelayCommand>,
}

#[derive(Debug, Subcommand)]
pub enum RelayCommand {
    Status,
    Install,
    Uninstall,
    /// 设置配置项：relay_url / panel_password / bin / enabled
    Config {
        key: String,
        value: String,
    },
}

pub async fn run(paths: &GqyPaths, args: RelayArgs) -> Result<()> {
    match args.command.unwrap_or(RelayCommand::Status) {
        RelayCommand::Status => run_status(paths),
        RelayCommand::Install => run_install(paths).await,
        RelayCommand::Uninstall => run_uninstall(paths),
        RelayCommand::Config { key, value } => run_config(paths, &key, &value),
    }
}

fn config_for(paths: &GqyPaths) -> Result<RelayConfig> {
    let mut bridges = super::load(paths)?;
    let mut config = bridges.relay.clone().unwrap_or(RelayConfig {
        relay_url: String::new(),
        token: String::new(),
        device_id: String::new(),
        panel_password: String::new(),
        bin: super::node_bin(),
        enabled: true,
    });
    if config.bin.is_empty() {
        config.bin = super::node_bin();
    }
    bridges.relay = Some(config.clone());
    super::save(paths, &bridges)?;
    Ok(config)
}

fn run_status(paths: &GqyPaths) -> Result<()> {
    let bridges = super::load(paths)?;
    let config = bridges.relay.clone().unwrap_or(RelayConfig {
        relay_url: String::new(),
        token: String::new(),
        device_id: String::new(),
        panel_password: String::new(),
        bin: super::node_bin(),
        enabled: true,
    });
    println!("中继桥接配置:");
    println!(
        "  relay_url:  {}",
        if config.relay_url.is_empty() {
            "(未设置 —— APK 通信不可用)"
        } else {
            &config.relay_url
        }
    );
    println!(
        "  device_id:  {}",
        if config.device_id.is_empty() {
            "(未注册)"
        } else {
            &config.device_id
        }
    );
    println!(
        "  token:      {}",
        if config.token.is_empty() {
            "(未注册)"
        } else {
            "已注册"
        }
    );
    println!(
        "  面板密码:   {}",
        if config.panel_password.is_empty() {
            "未设置（本地配对确认免密）"
        } else {
            "已设置"
        }
    );
    println!("  node:       {}", config.bin);
    println!("  启用:       {}", if config.enabled { "是" } else { "否" });
    println!("  桥接脚本:   {}", super::bridge_script(paths, "relay")?.display());
    println!(
        "  桥接自启动: {}",
        if super::service_status(BRIDGE_LABEL)? {
            "运行中"
        } else {
            "未安装"
        }
    );
    let bridge_log = super::bridge_log_path(paths, "relay");
    if bridge_log.exists() {
        let last = std::fs::read_to_string(&bridge_log)
            .ok()
            .and_then(|text| text.lines().rev().next().map(str::to_string))
            .unwrap_or_default();
        println!("  最近日志:   {}", last.chars().take(120).collect::<String>());
    }
    Ok(())
}

/// 向中继注册桌面设备：POST /pairing/register → {code, device_id, token, expires_in}
fn register_desktop(paths: &GqyPaths, config: &RelayConfig) -> Result<(String, String)> {
    let base = relay_http_base(&config.relay_url)?;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()
        .context("构建 HTTP 客户端失败")?;
    let hostname = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Windows 桌面".to_string());
    let response = client
        .post(format!("{base}/pairing/register"))
        .json(&json!({ "label": hostname }))
        .send()
        .with_context(|| format!("连接中继失败：{}", config.relay_url))?;
    if !response.status().is_success() {
        bail!("中继注册失败：HTTP {}", response.status());
    }
    let body: serde_json::Value = response.json().context("解析中继响应失败")?;
    let device_id = body
        .get("device_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let token = body
        .get("token")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if device_id.is_empty() || token.is_empty() {
        bail!("中继返回缺少 device_id/token");
    }
    let _ = paths;
    Ok((device_id, token))
}

fn relay_http_base(relay_url: &str) -> Result<String> {
    let url = relay_url.trim();
    if url.is_empty() {
        bail!("relay_url 未设置（先 `hilia relay config relay_url <地址>`）");
    }
    // wss://host/ws → https://host ；ws://host/ws → http://host
    if let Some(rest) = url.strip_prefix("wss://") {
        Ok(format!("https://{}", rest.trim_end_matches('/').trim_end_matches("/ws")))
    } else if let Some(rest) = url.strip_prefix("ws://") {
        Ok(format!("http://{}", rest.trim_end_matches('/').trim_end_matches("/ws")))
    } else {
        bail!("relay_url 必须以 wss:// 或 ws:// 开头（如 wss://relay.example.com/ws）");
    }
}

async fn run_install(paths: &GqyPaths) -> Result<()> {
    let mut config = config_for(paths)?;
    if config.relay_url.trim().is_empty() {
        bail!("relay_url 未设置：先 `hilia relay config relay_url wss://你的中继域名/ws`");
    }
    // 向中继注册（幂等：已有 token 则复用）
    if config.token.is_empty() || config.device_id.is_empty() {
        println!("正在向中继注册桌面设备 ...");
        let reg_paths = paths.clone();
        let reg_config = config.clone();
        let (device_id, token) =
            tokio::task::spawn_blocking(move || register_desktop(&reg_paths, &reg_config))
                .await??;
        config.device_id = device_id;
        config.token = token;
        let mut bridges = super::load(paths)?;
        bridges.relay = Some(config.clone());
        super::save(paths, &bridges)?;
        println!("✅ 已注册：{}", config.device_id);
    }
    if !config.enabled {
        println!("桥接当前处于禁用状态（enabled=false），仅写入自启动配置不加载。");
    }
    let script = super::bridge_script(paths, "relay")?;

    #[cfg(windows)]
    {
        let launcher = super::bridge_launchers_dir(paths)?.join("relay-bridge.cmd");
        let env = bridge_env(paths, &config);
        super::write_launcher_cmd(&launcher, &env, &config.bin)?;
        println!("已写入 {}", launcher.display());
        if config.enabled {
            super::service_uninstall(BRIDGE_LABEL).ok();
            super::service_install(BRIDGE_LABEL, &launcher)?;
            println!("✅ 桥接自启动已注册（计划任务 ONLOGON）");
        }
    }
    #[cfg(not(windows))]
    {
        let log = super::bridge_log_path(paths, "relay");
        let plist = json!({
            "Label": BRIDGE_LABEL,
            "ProgramArguments": [config.bin, script.to_str().unwrap_or_default()],
            "EnvironmentVariables": {
                "HOME": std::env::var_os("HOME").map(|v| v.to_string_lossy().into_owned()).unwrap_or_default(),
                "PATH": "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
                "HILIA_RELAY_URL": config.relay_url,
                "HILIA_RELAY_TOKEN": config.token,
                "HILIA_RELAY_DEVICE_ID": config.device_id,
                "HILIA_PANEL_PASSWORD": config.panel_password,
                "HILIA_BIN": paths.bin_hint(),
                "HILIA_BRIDGE_LOG": log.to_str().unwrap_or_default(),
            },
            "RunAtLoad": true,
            "KeepAlive": true,
            "StandardOutPath": log.to_str().unwrap_or_default(),
            "StandardErrorPath": log.to_str().unwrap_or_default(),
        });
        if config.enabled {
            super::service_uninstall(BRIDGE_LABEL).ok();
            super::launchctl_install(BRIDGE_LABEL, &plist)?;
            println!("✅ 桥接自启动已加载（LaunchAgent KeepAlive 托管）");
        }
    }
    let _ = script;

    println!();
    println!("下一步：");
    println!("  1. 面板 → 设置 → 设备配对 → 生成二维码");
    println!("  2. 学生用希尔娅 APK 扫码完成配对");
    println!("  3. 查看状态：hilia relay status");
    Ok(())
}

fn bridge_env(paths: &GqyPaths, config: &RelayConfig) -> Vec<(&'static str, String)> {
    vec![
        ("HILIA_HOME", paths.home_hint()),
        ("HILIA_RELAY_URL", config.relay_url.clone()),
        ("HILIA_RELAY_TOKEN", config.token.clone()),
        ("HILIA_RELAY_DEVICE_ID", config.device_id.clone()),
        ("HILIA_PANEL_PASSWORD", config.panel_password.clone()),
        ("HILIA_BIN", paths.bin_hint()),
        (
            "HILIA_BRIDGE_LOG",
            super::bridge_log_path(paths, "relay").display().to_string(),
        ),
    ]
}

fn run_uninstall(paths: &GqyPaths) -> Result<()> {
    super::service_uninstall(BRIDGE_LABEL)?;
    let launcher = super::bridge_launchers_dir(paths)?.join("relay-bridge.cmd");
    if launcher.exists() {
        std::fs::remove_file(&launcher)?;
    }
    println!("✅ 已移除中继桥接自启动（配置与数据保留）");
    Ok(())
}

fn run_config(paths: &GqyPaths, key: &str, value: &str) -> Result<()> {
    let mut bridges = super::load(paths)?;
    let mut config = bridges.relay.clone().unwrap_or(RelayConfig {
        relay_url: String::new(),
        token: String::new(),
        device_id: String::new(),
        panel_password: String::new(),
        bin: super::node_bin(),
        enabled: true,
    });
    match key {
        "relay_url" => config.relay_url = value.to_string(),
        "panel_password" => config.panel_password = value.to_string(),
        "bin" => config.bin = value.to_string(),
        "enabled" => {
            config.enabled = value == "true" || value == "1" || value == "yes";
        }
        _ => bail!("未知配置项 {key}（支持：relay_url / panel_password / bin / enabled）"),
    }
    bridges.relay = Some(config.clone());
    super::save(paths, &bridges)?;
    println!("relay.{key} = {value}");
    Ok(())
}
