//! 希尔娅 update.json 签名工具（CI 工作流使用）。
//!
//! 用法：
//!   update-signer sign <update.json> [<私钥base64>]
//! 私钥也可通过环境变量 HILIA_SIGN_KEY 提供（参数优先）。
//!
//! 签名规范（必须与客户端 src/update.rs 完全一致）：
//!   1. 解析为 UpdateInfo 结构（字段顺序固定）
//!   2. 把 `signature` 字段置空字符串
//!   3. 按结构体字段顺序 serde_json 序列化
//!   4. 对序列化结果做 Ed25519 签名，base64 写回 `signature` 字段

use base64::{engine::general_purpose, Engine as _};
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use std::process::ExitCode;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactInfo {
    #[serde(default)]
    pub version: String,
    pub urls: Vec<String>,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub version: String,
    #[serde(default)]
    pub min_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<ArtifactInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apk: Option<ArtifactInfo>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub next: String,
    #[serde(default)]
    pub signature: String,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("用法: update-signer sign <update.json> [<私钥base64>]");
        return ExitCode::from(2);
    }
    let file = &args[2];
    let secret = args
        .get(3)
        .cloned()
        .or_else(|| std::env::var("HILIA_SIGN_KEY").ok())
        .unwrap_or_default();
    if secret.trim().is_empty() {
        eprintln!("错误：缺少私钥（参数或环境变量 HILIA_SIGN_KEY）");
        return ExitCode::from(2);
    }

    match run(file, &secret) {
        Ok(()) => {
            println!("已签名并写回 {file}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("错误：{error}");
            ExitCode::FAILURE
        }
    }
}

fn run(file: &str, secret_b64: &str) -> Result<(), String> {
    let raw = std::fs::read_to_string(file).map_err(|e| format!("读取 {file} 失败：{e}"))?;
    let mut info: UpdateInfo =
        serde_json::from_str(&raw).map_err(|e| format!("解析 JSON 失败：{e}"))?;
    if !info.signature.trim().is_empty() {
        return Err("update.json 已包含签名，请先移除 signature 字段".to_string());
    }
    // 与客户端一致：signature 置空串后按结构体顺序序列化
    info.signature.clear();
    let canonical =
        serde_json::to_vec(&info).map_err(|e| format!("序列化失败：{e}"))?;

    let secret_raw = general_purpose::STANDARD
        .decode(secret_b64.trim())
        .map_err(|e| format!("私钥 base64 解码失败：{e}"))?;
    if secret_raw.len() != 32 {
        return Err(format!("私钥长度错误（{}）", secret_raw.len()));
    }
    let mut secret_bytes = [0u8; 32];
    secret_bytes.copy_from_slice(&secret_raw);
    let signing = SigningKey::from_bytes(&secret_bytes);
    let signature = signing.sign(&canonical);
    let signature_b64 = general_purpose::STANDARD.encode(signature.to_bytes());

    info.signature = signature_b64;
    let out =
        serde_json::to_string_pretty(&info).map_err(|e| format!("输出序列化失败：{e}"))?;
    std::fs::write(file, out + "\n").map_err(|e| format!("写入 {file} 失败：{e}"))?;
    Ok(())
}
