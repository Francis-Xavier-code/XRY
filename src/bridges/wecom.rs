//! `hilia wecom`：企业微信桥接管理。
//!
//! 子命令：
//! - `status`：查看配置与自启动状态
//! - `install`：安装桥接自启动（Windows 计划任务 / macOS LaunchAgent）
//! - `uninstall`：移除自启动（不删配置与数据）
//! - `config <key> <value>`：设置 corp_id / agent_id / secret / token / encoding_aes_key /
//!   callback_port / bin / enabled
//!
//! 原理：企业微信自建应用「接收消息」回调 → 本地 HTTP 服务（默认 127.0.0.1:4097），
//! 回复消息走发送 API。回调 URL 需要公网可访问：企业微信管理后台 →
//! 应用管理 → 自建应用 → 接收消息 → 设置 API 接收（URL 指向你的内网穿透地址，
//! 如 `https://你的域名/wecom` 穿透到 127.0.0.1:4097）。

use super::WecomConfig;
use crate::paths::GqyPaths;
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde_json::json;


const BRIDGE_LABEL: &str = "HiliaWecomBridge";

#[derive(Debug, Args)]
pub struct WecomArgs {
    #[command(subcommand)]
    pub command: Option<WecomCommand>,
}

#[derive(Debug, Subcommand)]
pub enum WecomCommand {
    Status,
    Install,
    Uninstall,
    /// 设置配置项：corp_id / agent_id / secret / token / encoding_aes_key / callback_port / bin / enabled
    Config {
        key: String,
        value: String,
    },
}

pub async fn run(paths: &GqyPaths, args: WecomArgs) -> Result<()> {
    match args.command.unwrap_or(WecomCommand::Status) {
        WecomCommand::Status => run_status(paths),
        WecomCommand::Install => run_install(paths),
        WecomCommand::Uninstall => run_uninstall(paths),
        WecomCommand::Config { key, value } => run_config(paths, &key, &value),
    }
}

fn config_for(paths: &GqyPaths) -> Result<WecomConfig> {
    let mut bridges = super::load(paths)?;
    let mut config = bridges.wecom.clone().unwrap_or(WecomConfig {
        corp_id: String::new(),
        agent_id: String::new(),
        secret: String::new(),
        token: String::new(),
        encoding_aes_key: String::new(),
        callback_port: 4097,
        bin: super::node_bin(),
        enabled: true,
    });
    if config.bin.is_empty() {
        config.bin = super::node_bin();
    }
    bridges.wecom = Some(config.clone());
    super::save(paths, &bridges)?;
    Ok(config)
}

fn run_status(paths: &GqyPaths) -> Result<()> {
    let bridges = super::load(paths)?;
    let config = bridges.wecom.clone().unwrap_or_else(|| WecomConfig {
        corp_id: String::new(),
        agent_id: String::new(),
        secret: String::new(),
        token: String::new(),
        encoding_aes_key: String::new(),
        callback_port: 4097,
        bin: super::node_bin(),
        enabled: true,
    });
    println!("企业微信桥接配置:");
    println!(
        "  corp_id:       {}",
        if config.corp_id.is_empty() {
            "(未设置)"
        } else {
            &config.corp_id
        }
    );
    println!(
        "  agent_id:      {}",
        if config.agent_id.is_empty() {
            "(未设置)"
        } else {
            &config.agent_id
        }
    );
    println!(
        "  secret:        {}",
        if config.secret.is_empty() {
            "(未设置)"
        } else {
            "已设置"
        }
    );
    println!(
        "  token:         {}",
        if config.token.is_empty() {
            "(未设置)"
        } else {
            "已设置"
        }
    );
    println!(
        "  encoding_aes_key: {}",
        if config.encoding_aes_key.is_empty() {
            "(未设置)"
        } else {
            "已设置"
        }
    );
    println!("  callback_port: {}", config.callback_port);
    println!("  node:          {}", config.bin);
    println!("  启用:          {}", if config.enabled { "是" } else { "否" });
    println!("  桥接脚本:      {}", super::bridge_script(paths, "wecom")?.display());
    println!(
        "  桥接自启动:    {}",
        if super::service_status(BRIDGE_LABEL)? {
            "运行中"
        } else {
            "未安装"
        }
    );
    println!();
    println!("回调 URL（企业微信后台 → 接收消息 → API 接收）：");
    println!("  http://127.0.0.1:{}/wecom （需用内网穿透映射到公网，如 https://你的域名/wecom）", config.callback_port);
    Ok(())
}

fn bridge_env(paths: &GqyPaths, config: &WecomConfig) -> Vec<(&'static str, String)> {
    vec![
        ("HILIA_HOME", paths.home_hint()),
        ("HILIA_WECOM_CORP_ID", config.corp_id.clone()),
        ("HILIA_WECOM_AGENT_ID", config.agent_id.clone()),
        ("HILIA_WECOM_SECRET", config.secret.clone()),
        ("HILIA_WECOM_TOKEN", config.token.clone()),
        ("HILIA_WECOM_AES_KEY", config.encoding_aes_key.clone()),
        (
            "HILIA_WECOM_PORT",
            config.callback_port.to_string(),
        ),
        ("HILIA_BIN", paths.bin_hint()),
        (
            "HILIA_BRIDGE_LOG",
            super::bridge_log_path(paths, "wecom").display().to_string(),
        ),
    ]
}

fn run_install(paths: &GqyPaths) -> Result<()> {
    let config = config_for(paths)?;
    if config.corp_id.is_empty()
        || config.agent_id.is_empty()
        || config.secret.is_empty()
        || config.token.is_empty()
        || config.encoding_aes_key.is_empty()
    {
        bail!(
            "企业微信配置不完整。请先设置：hilia wecom config corp_id|agent_id|secret|token|encoding_aes_key <值>"
        );
    }
    if !config.enabled {
        println!("桥接当前处于禁用状态（enabled=false），仅写入自启动配置不加载。");
    }
    let script = super::bridge_script(paths, "wecom")?;

    #[cfg(windows)]
    {
        let launcher = super::bridge_launchers_dir(paths)?.join("wecom-bridge.cmd");
        super::write_launcher_cmd(&launcher, &bridge_env(paths, &config), &config.bin)?;
        println!("已写入 {}", launcher.display());
        if config.enabled {
            super::service_uninstall(BRIDGE_LABEL).ok();
            super::service_install(BRIDGE_LABEL, &launcher)?;
            println!("✅ 桥接自启动已注册（计划任务 ONLOGON）");
        }
    }
    #[cfg(not(windows))]
    {
        let log = super::bridge_log_path(paths, "wecom");
        let plist = json!({
            "Label": BRIDGE_LABEL,
            "ProgramArguments": [config.bin, script.to_str().unwrap_or_default()],
            "EnvironmentVariables": {
                "HOME": std::env::var_os("HOME").map(|v| v.to_string_lossy().into_owned()).unwrap_or_default(),
                "PATH": "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
                "HILIA_HOME": paths.home_hint(),
                "HILIA_WECOM_CORP_ID": config.corp_id,
                "HILIA_WECOM_AGENT_ID": config.agent_id,
                "HILIA_WECOM_SECRET": config.secret,
                "HILIA_WECOM_TOKEN": config.token,
                "HILIA_WECOM_AES_KEY": config.encoding_aes_key,
                "HILIA_WECOM_PORT": config.callback_port.to_string(),
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
    println!("  1. 企业微信管理后台 → 应用管理 → 自建应用 → 接收消息 → 设置 API 接收");
    println!("  2. 回调 URL 填你的内网穿透公网地址（如 https://你的域名/wecom，穿透到 127.0.0.1:{}）", config.callback_port);
    println!("  3. Token 与 EncodingAESKey 填企业微信后台生成的值（与上方配置一致）");
    println!("  4. 查看状态：hilia wecom status");
    Ok(())
}

fn run_uninstall(paths: &GqyPaths) -> Result<()> {
    super::service_uninstall(BRIDGE_LABEL)?;
    let launcher = super::bridge_launchers_dir(paths)?.join("wecom-bridge.cmd");
    if launcher.exists() {
        std::fs::remove_file(&launcher)?;
    }
    println!("✅ 已移除企业微信桥接自启动（配置与数据保留）");
    Ok(())
}

fn run_config(paths: &GqyPaths, key: &str, value: &str) -> Result<()> {
    let mut bridges = super::load(paths)?;
    let mut config = bridges.wecom.clone().unwrap_or(WecomConfig {
        corp_id: String::new(),
        agent_id: String::new(),
        secret: String::new(),
        token: String::new(),
        encoding_aes_key: String::new(),
        callback_port: 4097,
        bin: super::node_bin(),
        enabled: true,
    });
    match key {
        "corp_id" => config.corp_id = value.to_string(),
        "agent_id" => config.agent_id = value.to_string(),
        "secret" => config.secret = value.to_string(),
        "token" => config.token = value.to_string(),
        "encoding_aes_key" => config.encoding_aes_key = value.to_string(),
        "callback_port" => {
            let port: u16 = value
                .parse()
                .with_context(|| format!("callback_port 必须是端口号：{value}"))?;
            config.callback_port = port;
        }
        "bin" => config.bin = value.to_string(),
        "enabled" => {
            config.enabled = value == "true" || value == "1" || value == "yes";
        }
        _ => bail!(
            "未知配置项 {key}（支持：corp_id / agent_id / secret / token / encoding_aes_key / callback_port / bin / enabled）"
        ),
    }
    bridges.wecom = Some(config.clone());
    super::save(paths, &bridges)?;
    println!("wecom.{key} = {value}");
    Ok(())
}
