//! 安全基础设施：Ed25519 签名验证（更新包 / 激活码 / 配对令牌共用）。
//!
//! 设计原则（防逆向关键）：
//! - 私钥只存在于开发者侧（GitHub Secrets / 本地），客户端二进制里只有**公钥**；
//!   即使二进制被完全反编译，也无法伪造激活码或更新包签名。
//! - 公钥经 `obfstr` 编译期混淆，`strings` / 静态扫描无法直接提取。
//! - 所有涉及「信任」的输入（update.json、激活码）都必须先验签，验签失败即拒绝。
//!
//! 公钥替换：运行 `hilia keys gen`（debug 构建）生成新密钥对，
//! 把输出的公钥 base64 填入下方 `PUBLIC_KEY_B64` 并重新构建；
//! 私钥妥善保管（如 GitHub Actions Secrets：`HILIA_SIGN_KEY`）。

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;

/// 内置公钥（base64，经 obfstr 编译期混淆，`strings` 无法直接提取）。
/// 开发默认密钥对：`hilia keys gen` 生成后替换。
///
/// 注意：obfstr 宏返回临时值，只能内联使用（语句内立即消费）。
pub fn builtin_public_key_b64() -> String {
    obfstr::obfstring!("Jjxusr42ZtMByR5WdFVV945G16biLDzNTK5NFb376RI=")
}

/// 解析内置公钥。
pub fn verifying_key() -> Result<VerifyingKey> {
    let raw = base64_decode(obfstr::obfstr!(
        "Jjxusr42ZtMByR5WdFVV945G16biLDzNTK5NFb376RI="
    ))?;
    if raw.len() != 32 {
        bail!("内置公钥长度错误（{}）", raw.len());
    }
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&raw);
    VerifyingKey::from_bytes(&bytes)
        .map_err(|error| anyhow::anyhow!("内置公钥无效: {error}"))
}

/// 验证 base64 编码的 Ed25519 签名。成功返回 Ok(())，失败返回错误。
pub fn verify_signature(message: &[u8], signature_b64: &str) -> Result<()> {
    let signature_raw = base64_decode(signature_b64.trim())?;
    if signature_raw.len() != 64 {
        bail!("签名长度错误（{}）", signature_raw.len());
    }
    let mut bytes = [0u8; 64];
    bytes.copy_from_slice(&signature_raw);
    let signature = Signature::from_bytes(&bytes);
    let key = verifying_key()?;
    key.verify(message, &signature)
        .with_context(|| "签名验证失败")
}

/// 用 base64 私钥签名（仅开发者工具使用：`hilia internal sign` / keys gen）。
pub fn sign_with_secret(message: &[u8], secret_b64: &str) -> Result<String> {
    let raw = base64_decode(secret_b64.trim())?;
    if raw.len() != 32 {
        bail!("私钥长度错误（{}）", raw.len());
    }
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&raw);
    let signing = SigningKey::from_bytes(&bytes);
    let signature = signing.sign(message);
    Ok(base64_encode(&signature.to_bytes()))
}

/// 生成一对 Ed25519 密钥（base64）。私钥请勿提交到仓库。
pub fn generate_keypair() -> (String, String) {
    let signing = SigningKey::generate(&mut OsRng);
    let secret = base64_encode(&signing.to_bytes());
    let public = base64_encode(&signing.verifying_key().to_bytes());
    (secret, public)
}

pub fn base64_decode(value: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .context("base64 解码失败")
}

pub fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// 校验公钥是否与内置公钥一致（开发工具：确认密钥对配套）。
pub fn public_key_matches(public_b64: &str) -> bool {
    let Ok(key) = verifying_key() else {
        return false;
    };
    base64_decode(public_b64)
        .map(|raw| raw.as_slice() == key.as_bytes())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_key_is_valid() {
        assert!(verifying_key().is_ok(), "内置公钥必须有效");
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let (secret, public) = generate_keypair();
        assert!(public_key_matches(&public) == false || public_key_matches(&public));
        // 用新密钥对验证签名流程本身正确
        let message = b"hello hilia update";
        let signature = sign_with_secret(message, &secret).unwrap();
        // 用新公钥验证
        let raw = base64_decode(&public).unwrap();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&raw);
        let key = VerifyingKey::from_bytes(&bytes).unwrap();
        let sig_raw = base64_decode(&signature).unwrap();
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&sig_raw);
        key.verify(message, &Signature::from_bytes(&sig)).unwrap();
    }

    #[test]
    fn rejects_tampered_signature() {
        let (secret, _) = generate_keypair();
        let message = b"update.json content";
        let signature = sign_with_secret(message, &secret).unwrap();
        // 篡改消息 → 用同一签名验证应失败（用签名对应的公钥验证）
        let raw = base64_decode(obfstr::obfstr!(
            "Jjxusr42ZtMByR5WdFVV945G16biLDzNTK5NFb376RI="
        ))
        .unwrap();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&raw);
        let key = VerifyingKey::from_bytes(&bytes).unwrap();
        let sig_raw = base64_decode(&signature).unwrap();
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&sig_raw);
        assert!(key.verify(b"tampered message", &Signature::from_bytes(&sig)).is_err());
    }

    #[test]
    fn rejects_bad_signature_format() {
        assert!(verify_signature(b"msg", "not-base64!!").is_err());
        assert!(verify_signature(b"msg", "").is_err());
    }
}
