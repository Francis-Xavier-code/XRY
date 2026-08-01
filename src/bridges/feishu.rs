//! `hilia feishu`：飞书桥接管理。
//!
//! 子命令：
//! - `status`：查看配置与自启动状态
//! - `install`：安装桥接自启动（Windows 计划任务 / macOS LaunchAgent）
//! - `uninstall`：移除自启动（不删配置与数据）
//! - `config <key> <value>`：设置 app_id / app_secret / bin / enabled
//!
//! 原理：飞书开放平台自建应用，事件订阅走**长连接模式**
//! （官方 @larksuiteoapi/node-sdk，WebSocket 主动连出），**无需公网回调地址**，
//! 适合辅导员个人电脑直接部署。
//!
//! 首次使用需在桥接目录安装依赖：
//! `cd <bridges>/feishu && npm ci`（仅一次，之后随包分发无需再装）。

use super::FeishuConfig;
use crate::paths::GqyPaths;
use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use serde_json::json;


const BRIDGE_LABEL: &str = "HiliaFeishuBridge";

#[derive(Debug, Args)]
pub struct FeishuArgs {
    #[command(subcommand)]
    pub command: Option<FeishuCommand>,
}

#[derive(Debug, Subcommand)]
pub enum FeishuCommand {
    Status,
    Install,
    Uninstall,
    /// 设置配置项：app_id / app_secret / bin / enabled
    Config {
        key: String,
        value: String,
    },
}

pub async fn run(paths: &GqyPaths, args: FeishuArgs) -> Result<()> {
    match args.command.unwrap_or(FeishuCommand::Status) {
        FeishuCommand::Status => run_status(paths),
        FeishuCommand::Install => run_install(paths),
        FeishuCommand::Uninstall => run_uninstall(paths),
        FeishuCommand::Config { key, value } => run_config(paths, &key, &value),
    }
}

fn config_for(paths: &GqyPaths) -> Result<FeishuConfig> {
    let mut bridges = super::load(paths)?;
    let mut config = bridges.feishu.clone().unwrap_or(FeishuConfig {
        app_id: String::new(),
        app_secret: String::new(),
        bin: super::node_bin(),
        enabled: true,
    });
    if config.bin.is_empty() {
        config.bin = super::node_bin();
    }
    bridges.feishu = Some(config.clone());
    super::save(paths, &bridges)?;
    Ok(config)
}

fn run_status(paths: &GqyPaths) -> Result<()> {
    let bridges = super::load(paths)?;
    let config = bridges.feishu.clone().unwrap_or_else(|| FeishuConfig {
        app_id: String::new(),
        app_secret: String::new(),
        bin: super::node_bin(),
        enabled: true,
    });
    println!("飞书桥接配置:");
    println!(
        "  app_id:     {}",
        if config.app_id.is_empty() {
            "(未设置)"
        } else {
            &config.app_id
        }
    );
    println!(
        "  app_secret: {}",
        if config.app_secret.is_empty() {
            "(未设置)"
        } else {
            "已设置"
        }
    );
    println!("  node:       {}", config.bin);
    println!("  启用:       {}", if config.enabled { "是" } else { "否" });
    println!("  桥接脚本:   {}", super::bridge_script(paths, "feishu")?.display());
    println!(
        "  桥接自启动: {}",
        if super::service_status(BRIDGE_LABEL)? {
            "运行中"
        } else {
            "未安装"
        }
    );
    let bridge_log = super::bridge_log_path(paths, "feishu");
    if bridge_log.exists() {
        let last = std::fs::read_to_string(&bridge_log)
            .ok()
            .and_then(|text| text.lines().rev().next().map(str::to_string))
            .unwrap_or_default();
        println!("  最近日志:   {}", last.chars().take(120).collect::<String>());
    }
    Ok(())
}

fn bridge_env(paths: &GqyPaths, config: &FeishuConfig) -> Vec<(&'static str, String)> {
    vec![
        ("HILIA_HOME", paths.home_hint()),
        ("HILIA_FEISHU_APP_ID", config.app_id.clone()),
        ("HILIA_FEISHU_APP_SECRET", config.app_secret.clone()),
        ("HILIA_BIN", paths.bin_hint()),
        (
            "HILIA_BRIDGE_LOG",
            super::bridge_log_path(paths, "feishu").display().to_string(),
        ),
    ]
}

fn run_install(paths: &GqyPaths) -> Result<()> {
    let config = config_for(paths)?;
    if config.app_id.is_empty() || config.app_secret.is_empty() {
        bail!(
            "飞书配置不完整。请先设置：hilia feishu config app_id|app_secret <值>"
        );
    }
    if !config.enabled {
        println!("桥接当前处于禁用状态（enabled=false），仅写入自启动配置不加载。");
    }
    let script = super::bridge_script(paths, "feishu")?;

    // 提示依赖安装
    let feishu_dir = super::bridges_dir(paths).join("feishu");
    if feishu_dir.join("node_modules").is_dir() {
        println!("✅ 飞书 SDK 依赖已安装（node_modules 存在）");
    } else {
        println!(
            "⚠ 飞书桥接依赖官方 SDK，请先安装（仅一次）：cd {} && npm ci",
            feishu_dir.display()
        );
    }

    #[cfg(windows)]
    {
        let launcher = super::bridge_launchers_dir(paths)?.join("feishu-bridge.cmd");
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
        let log = super::bridge_log_path(paths, "feishu");
        let plist = json!({
            "Label": BRIDGE_LABEL,
            "ProgramArguments": [config.bin, script.to_str().unwrap_or_default()],
            "EnvironmentVariables": {
                "HOME": std::env::var_os("HOME").map(|v| v.to_string_lossy().into_owned()).unwrap_or_default(),
                "PATH": "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
                "HILIA_HOME": paths.home_hint(),
                "HILIA_FEISHU_APP_ID": config.app_id,
                "HILIA_FEISHU_APP_SECRET": config.app_secret,
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
    println!("  1. 飞书开放平台 → 开发者后台 → 创建企业自建应用，开启「机器人」能力");
    println!("  2. 事件订阅选择「长连接模式」，订阅 im.message.receive_v1");
    println!("  3. 设置 app_id / app_secret：hilia feishu config app_id|app_secret <值>");
    println!("  4. 查看状态：hilia feishu status");
    Ok(())
}

fn run_uninstall(paths: &GqyPaths) -> Result<()> {
    super::service_uninstall(BRIDGE_LABEL)?;
    let launcher = super::bridge_launchers_dir(paths)?.join("feishu-bridge.cmd");
    if launcher.exists() {
        std::fs::remove_file(&launcher)?;
    }
    println!("✅ 已移除飞书桥接自启动（配置与数据保留）");
    Ok(())
}

fn run_config(paths: &GqyPaths, key: &str, value: &str) -> Result<()> {
    let mut bridges = super::load(paths)?;
    let mut config = bridges.feishu.clone().unwrap_or(FeishuConfig {
        app_id: String::new(),
        app_secret: String::new(),
        bin: super::node_bin(),
        enabled: true,
    });
    match key {
        "app_id" => config.app_id = value.to_string(),
        "app_secret" => config.app_secret = value.to_string(),
        "bin" => config.bin = value.to_string(),
        "enabled" => {
            config.enabled = value == "true" || value == "1" || value == "yes";
        }
        _ => bail!("未知配置项 {key}（支持：app_id / app_secret / bin / enabled）"),
    }
    bridges.feishu = Some(config.clone());
    super::save(paths, &bridges)?;
    println!("feishu.{key} = {value}");
    Ok(())
}
