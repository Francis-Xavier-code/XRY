//! 付费订阅模块（预留能力）。
//!
//! 激活方式：
//! 1. **离线激活码**：开发者用私钥签发 `HILIA1.<payload_b64>.<sig_b64>`，
//!    客户端内置公钥验签 + 有效期检查（无需服务器，防伪靠签名）。
//! 2. **在线订阅（预留）**：`license.server` 配置 license 服务器地址，
//!    `LicenseClient` 骨架已就位，接入支付后可实现在线校验/续期。
//!
//! 门控框架：`has_feature(feature)`。当前定义的特性：
//! - `multi_device`：多设备配对（免费版限 1 台，pro 不限）
//! 免费版（free）可用全部基础功能；激活后按 plan 开放高级特性。

use crate::config::AppConfig;
use crate::paths::GqyPaths;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const CODE_PREFIX: &str = "HILIA1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivationPayload {
    pub plan: String,
    pub user: String,
    /// Unix 秒；0 = 永久
    pub expires_at: i64,
}

/// 从 config.jsonc 读取的授权状态。
#[derive(Debug, Clone, Default)]
pub struct LicenseState {
    pub status: String,
    pub plan: String,
    pub activation_code: String,
    pub activated_at: String,
    pub expires_at: String,
    pub server: String,
}

impl LicenseState {
    pub fn is_activated(&self) -> bool {
        self.status == "activated"
    }

    /// 是否已过期（永久激活 expires_at 为空）。
    pub fn is_expired(&self) -> bool {
        if self.status != "activated" || self.expires_at.is_empty() {
            return false;
        }
        DateTime::parse_from_rfc3339(&self.expires_at)
            .map(|time| time < Utc::now())
            .unwrap_or(false)
    }

    /// 功能门控：plan 为 pro 时开放全部特性。
    pub fn has_feature(&self, feature: &str) -> bool {
        if self.plan == "pro" {
            return true;
        }
        match feature {
            // 免费版仅限 1 台设备配对（见 M5 配对逻辑）
            "multi_device" => false,
            _ => true,
        }
    }

    pub fn summary(&self) -> String {
        if !self.is_activated() {
            return "未激活（免费版）".to_string();
        }
        let plan = if self.plan.is_empty() { "基础" } else { &self.plan };
        if self.expires_at.is_empty() {
            format!("已激活：{plan}（永久）")
        } else {
            format!("已激活：{plan}（至 {}）", self.expires_at)
        }
    }
}

/// 从配置加载授权状态。
pub fn load(config: &AppConfig) -> LicenseState {
    LicenseState {
        status: config.license.status.clone(),
        plan: config.license.plan.clone(),
        activation_code: config.license.activation_code.clone(),
        activated_at: config.license.activated_at.clone(),
        expires_at: config.license.expires_at.clone(),
        server: config.license.server.clone(),
    }
}

/// 解析激活码：`HILIA1.<payload_b64>.<sig_b64>` → payload；验签 + 有效期。
pub fn verify_activation_code(code: &str) -> Result<ActivationPayload> {
    let code = code.trim();
    let parts: Vec<&str> = code.split('.').collect();
    if parts.len() != 3 || parts[0] != CODE_PREFIX {
        bail!("激活码格式错误（应为 HILIA1.<数据>.<签名>）");
    }
    let payload_b64 = parts[1];
    let signature_b64 = parts[2];
    let payload_raw = crate::security::base64_decode(payload_b64)?;
    let payload_text = String::from_utf8(payload_raw.clone())
        .context("激活码数据不是有效 UTF-8")?;
    // 验签（对 payload 原文）
    crate::security::verify_signature(&payload_raw, signature_b64)?;
    let payload: ActivationPayload = serde_json::from_str(&payload_text)
        .context("激活码数据字段缺失或格式错误")?;
    // 有效期
    if payload.expires_at > 0 {
        let expires = DateTime::from_timestamp(payload.expires_at, 0)
            .context("激活码有效期超出范围")?;
        if expires < Utc::now() {
            bail!("激活码已过期（{}）", expires.to_rfc3339());
        }
    }
    if payload.plan.trim().is_empty() || payload.user.trim().is_empty() {
        bail!("激活码缺少 plan 或 user 字段");
    }
    Ok(payload)
}

/// 开发者签发激活码：`hilia keys sign-license '<plan>|<user>|<expires 或 0>'`。
/// 私钥从环境变量 HILIA_SIGN_KEY 读取。
pub fn make_activation_code(secret_b64: &str, plan: &str, user: &str, expires_at: i64) -> Result<String> {
    let payload = ActivationPayload {
        plan: plan.trim().to_string(),
        user: user.trim().to_string(),
        expires_at,
    };
    if payload.plan.is_empty() || payload.user.is_empty() {
        bail!("plan 与 user 不能为空");
    }
    let payload_raw = serde_json::to_vec(&payload)?;
    let payload_b64 = crate::security::base64_encode(&payload_raw);
    let signature = crate::security::sign_with_secret(&payload_raw, secret_b64)?;
    Ok(format!("{CODE_PREFIX}.{payload_b64}.{signature}"))
}

/// 激活：验签通过后写入 config.jsonc 的 license 段。
pub fn activate(paths: &GqyPaths, code: &str) -> Result<ActivationPayload> {
    let payload = verify_activation_code(code)?;
    let mut config = AppConfig::load(paths)?;
    let expires = if payload.expires_at > 0 {
        DateTime::from_timestamp(payload.expires_at, 0)
            .map(|time| time.to_rfc3339())
            .unwrap_or_default()
    } else {
        String::new()
    };
    config.license.status = "activated".to_string();
    config.license.plan = payload.plan.clone();
    config.license.activation_code = code.trim().to_string();
    config.license.activated_at = Utc::now().to_rfc3339();
    config.license.expires_at = expires;
    config.save(paths)?;
    Ok(payload)
}

/// 清除激活状态（不删除配置其他内容）。
pub fn deactivate(paths: &GqyPaths) -> Result<()> {
    let mut config = AppConfig::load(paths)?;
    config.license.status = "free".to_string();
    config.license.plan.clear();
    config.license.activation_code.clear();
    config.license.activated_at.clear();
    config.license.expires_at.clear();
    config.save(paths)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config() -> (tempfile::TempDir, GqyPaths) {
        let temp = tempfile::tempdir().unwrap();
        // 测试用隔离主目录：直接用 HILIA_HOME 环境变量指向临时目录
        let paths = crate::paths::GqyPaths::new().unwrap();
        let _ = &paths;
        unsafe { std::env::set_var("HILIA_HOME", temp.path()) };
        let paths = crate::paths::GqyPaths::new().unwrap();
        AppConfig::init_files(&paths).unwrap();
        (temp, paths)
    }

    /// 私钥从环境变量读取（GitHub Secrets 注入 CI；本地跑测试时手动设）。
    /// 仓库中不存放任何私钥。未设置时相关测试跳过。
    fn dev_secret() -> Option<String> {
        std::env::var("HILIA_SIGN_KEY").ok().filter(|v| !v.trim().is_empty())
    }

    #[test]
    fn activation_code_round_trip() {
        let Some(secret) = dev_secret() else {
            eprintln!("跳过：未设置 HILIA_SIGN_KEY");
            return;
        };
        let code = make_activation_code(&secret, "pro", "张三", 0).unwrap();
        let payload = verify_activation_code(&code).unwrap();
        assert_eq!(payload.plan, "pro");
        assert_eq!(payload.user, "张三");
        assert_eq!(payload.expires_at, 0);
    }

    #[test]
    fn rejects_tampered_code() {
        let Some(secret) = dev_secret() else {
            eprintln!("跳过：未设置 HILIA_SIGN_KEY");
            return;
        };
        let code = make_activation_code(&secret, "pro", "张三", 0).unwrap();
        let mut parts: Vec<String> = code.split('.').map(str::to_string).collect();
        // 篡改 payload
        let payload = ActivationPayload { plan: "pro".into(), user: "李四".into(), expires_at: 0 };
        parts[1] = crate::security::base64_encode(&serde_json::to_vec(&payload).unwrap());
        let tampered = parts.join(".");
        assert!(verify_activation_code(&tampered).is_err());
    }

    #[test]
    fn rejects_expired_code() {
        let Some(secret) = dev_secret() else {
            eprintln!("跳过：未设置 HILIA_SIGN_KEY");
            return;
        };
        let past = (Utc::now() - chrono::Duration::days(1)).timestamp();
        let code = make_activation_code(&secret, "pro", "张三", past).unwrap();
        assert!(verify_activation_code(&code).is_err());
    }

    #[test]
    fn activate_persists_and_gates_features() {
        let Some(secret) = dev_secret() else {
            eprintln!("跳过：未设置 HILIA_SIGN_KEY");
            return;
        };
        let (_temp, paths) = temp_config();
        let code = make_activation_code(&secret, "pro", "辅导员", 0).unwrap();
        let payload = activate(&paths, &code).unwrap();
        assert_eq!(payload.plan, "pro");

        let config = AppConfig::load(&paths).unwrap();
        let state = load(&config);
        assert!(state.is_activated());
        assert!(!state.is_expired());
        assert!(state.has_feature("multi_device"));
        assert_eq!(state.summary(), "已激活：pro（永久）");

        deactivate(&paths).unwrap();
        let config = AppConfig::load(&paths).unwrap();
        let state = load(&config);
        assert!(!state.is_activated());
        assert!(!state.has_feature("multi_device"));
    }
}
