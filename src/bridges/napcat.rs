//! `hilia napcat`：NapCat (QQ) 桥接管理。
//!
//! 子命令：
//! - `status`：查看配置与自启动状态
//! - `install`：安装桥接自启动（Windows 计划任务 / macOS LaunchAgent，KeepAlive 托管）
//! - `uninstall`：移除自启动（不删配置与数据）
//! - `config <key> <value>`：设置 ws_url / self_id / bin / enabled
//!
//! NapCat 本体（QQ 客户端 + NapCat 插件）在 Windows 上由用户自行安装运行
//! （NapCat.QQ 一键版或手动版），桥接通过 `ws://127.0.0.1:3001` 连接 OneBot v11。

use super::NapcatConfig;
use crate::paths::GqyPaths;
use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use serde_json::json;


const BRIDGE_LABEL: &str = "HiliaNapcatBridge";

#[derive(Debug, Args)]
pub struct NapcatArgs {
    #[command(subcommand)]
    pub command: Option<NapcatCommand>,
}

#[derive(Debug, Subcommand)]
pub enum NapcatCommand {
    Status,
    Install,
    Uninstall,
    /// 设置配置项：ws_url / self_id / bin / enabled
    Config {
        key: String,
        value: String,
    },
}

pub async fn run(paths: &GqyPaths, args: NapcatArgs) -> Result<()> {
    match args.command.unwrap_or(NapcatCommand::Status) {
        NapcatCommand::Status => run_status(paths),
        NapcatCommand::Install => run_install(paths),
        NapcatCommand::Uninstall => run_uninstall(paths),
        NapcatCommand::Config { key, value } => run_config(paths, &key, &value),
    }
}

fn config_for(paths: &GqyPaths) -> Result<NapcatConfig> {
    let mut bridges = super::load(paths)?;
    let mut config = bridges.napcat.clone().unwrap_or(NapcatConfig {
        ws_url: super::default_ws_url(),
        self_id: String::new(),
        bin: super::node_bin(),
        install_dir: String::new(),
        enabled: true,
    });
    // bin 默认跟随当前 node
    if config.bin.is_empty() {
        config.bin = super::node_bin();
    }
    bridges.napcat = Some(config.clone());
    super::save(paths, &bridges)?;
    Ok(config)
}

fn run_status(paths: &GqyPaths) -> Result<()> {
    let bridges = super::load(paths)?;
    let config = bridges
        .napcat
        .clone()
        .unwrap_or_else(|| NapcatConfig {
            ws_url: super::default_ws_url(),
            self_id: String::new(),
            bin: super::node_bin(),
            install_dir: String::new(),
            enabled: true,
        });
    println!("NapCat 桥接配置:");
    println!("  ws_url:    {}", config.ws_url);
    println!(
        "  self_id:   {}",
        if config.self_id.is_empty() {
            "(未设置 —— 群聊 @ 响应不可用)"
        } else {
            &config.self_id
        }
    );
    println!("  node:      {}", config.bin);
    println!("  启用:      {}", if config.enabled { "是" } else { "否" });
    println!("  桥接脚本:  {}", super::bridge_script(paths, "napcat")?.display());
    println!(
        "  桥接自启动: {}",
        if super::service_status(BRIDGE_LABEL)? {
            "运行中"
        } else {
            "未安装"
        }
    );
    let bridge_log = super::bridge_log_path(paths, "napcat");
    if bridge_log.exists() {
        let last = std::fs::read_to_string(&bridge_log)
            .ok()
            .and_then(|text| text.lines().rev().next().map(str::to_string))
            .unwrap_or_default();
        println!("  最近日志:  {}", last.chars().take(120).collect::<String>());
    }
    Ok(())
}

fn bridge_env(paths: &GqyPaths, config: &NapcatConfig) -> Vec<(&'static str, String)> {
    vec![
        ("HILIA_HOME", paths.home_hint()),
        ("HILIA_WS_URL", config.ws_url.clone()),
        ("HILIA_SELF_ID", config.self_id.clone()),
        ("HILIA_BIN", paths.bin_hint()),
        (
            "HILIA_BRIDGE_LOG",
            super::bridge_log_path(paths, "napcat").display().to_string(),
        ),
    ]
}

fn run_install(paths: &GqyPaths) -> Result<()> {
    let config = config_for(paths)?;
    if !config.enabled {
        println!("桥接当前处于禁用状态（enabled=false），仅写入自启动配置不加载。");
    }
    let script = super::bridge_script(paths, "napcat")?;

    #[cfg(windows)]
    {
        let launcher = super::bridge_launchers_dir(paths)?.join("napcat-bridge.cmd");
        super::write_launcher_cmd(&launcher, &bridge_env(paths, &config), &config.bin)?;
        println!("已写入 {}", launcher.display());
        if config.enabled {
            super::service_uninstall(BRIDGE_LABEL).ok();
            super::service_install(BRIDGE_LABEL, &launcher)?;
            println!("✅ 桥接自启动已注册（计划任务 ONLOGON，登录后自动运行）");
        }
    }
    #[cfg(not(windows))]
    {
        let log = super::bridge_log_path(paths, "napcat");
        let plist = json!({
            "Label": BRIDGE_LABEL,
            "ProgramArguments": [config.bin, script.to_str().unwrap_or_default()],
            "EnvironmentVariables": {
                "HOME": std::env::var_os("HOME").map(|v| v.to_string_lossy().into_owned()).unwrap_or_default(),
                "PATH": "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
                "HILIA_HOME": paths.home_hint(),
                "HILIA_WS_URL": config.ws_url,
                "HILIA_SELF_ID": config.self_id,
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
    if config.self_id.is_empty() {
        println!("  · 设置 QQ 号：hilia napcat config self_id <你的QQ号>");
    }
    println!("  · 查看状态：hilia napcat status");
    println!("  · NapCat 本体请自行安装运行（Windows：NapCat.QQ 一键版），监听 {}", config.ws_url);
    Ok(())
}

fn run_uninstall(paths: &GqyPaths) -> Result<()> {
    super::service_uninstall(BRIDGE_LABEL)?;
    let launcher = super::bridge_launchers_dir(paths)?.join("napcat-bridge.cmd");
    if launcher.exists() {
        std::fs::remove_file(&launcher)?;
    }
    println!("✅ 已移除 NapCat 桥接自启动（配置与数据保留）");
    Ok(())
}

fn run_config(paths: &GqyPaths, key: &str, value: &str) -> Result<()> {
    let mut bridges = super::load(paths)?;
    let mut config = bridges.napcat.clone().unwrap_or(NapcatConfig {
        ws_url: super::default_ws_url(),
        self_id: String::new(),
        bin: super::node_bin(),
        install_dir: String::new(),
        enabled: true,
    });
    match key {
        "ws_url" => config.ws_url = value.to_string(),
        "self_id" => config.self_id = value.to_string(),
        "bin" => config.bin = value.to_string(),
        "enabled" => {
            config.enabled = value == "true" || value == "1" || value == "yes";
        }
        _ => bail!("未知配置项 {key}（支持：ws_url / self_id / bin / enabled）"),
    }
    bridges.napcat = Some(config.clone());
    super::save(paths, &bridges)?;
    println!("napcat.{key} = {value}");
    Ok(())
}
