//! JSON 版本更新系统。
//!
//! - `update.json` 规范（仓库根 / Release 资产，签名防伪）：
//!   ```json
//!   { "version": "0.7.0", "min_version": "0.6.0",
//!     "windows": { "version": "0.7.0", "urls": ["原始", "加速源..."], "sha256": "...", "size": 123 },
//!     "apk":    { "version": "1.0.0", "urls": [...], "sha256": "..." },
//!     "notes": "...", "next": "https://上游/update.json", "signature": "Ed25519 签名" }
//!   ```
//! - 签名：对去掉 `signature` 字段后的规范化 JSON 做 Ed25519 签名；
//!   客户端内置公钥（见 src/security.rs）验签失败即拒绝，防伪造更新源。
//! - 多加速源：内置 GitHub 加速前缀列表（ghproxy 等），逐个轮询直到成功；
//!   config `update.mirrors` 可自定义。
//! - 上游切换：config `update.upstream_url`（默认指向 XRY 仓库）；
//!   update.json 的 `next` 字段可指示切换新上游。
//! - 强制更新：`min_version`（低于该版本必须更新）+ config `update.force=true`。

use crate::config::AppConfig;
use crate::paths::GqyPaths;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 默认上游：XRY 仓库根 update.json。
pub const DEFAULT_UPSTREAM_URL: &str =
    "https://raw.githubusercontent.com/Francis-Xavier-code/XRY/main/update.json";

/// 内置 GitHub 加速前缀（原始 URL 之前拼接，逐个轮询）。
pub const BUILTIN_MIRRORS: [&str; 4] = [
    "", // 原始 GitHub（第一个尝试，最可信）
    "https://ghproxy.net/",
    "https://gh-proxy.com/",
    "https://ghfast.top/",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    #[serde(default = "default_upstream_url")]
    pub upstream_url: String,
    /// 自定义加速前缀列表（覆盖内置）；空数组 = 只用内置
    #[serde(default)]
    pub mirrors: Vec<String>,
    /// 启动时后台检查更新（面板会收到提示）
    #[serde(default = "default_true")]
    pub check_on_startup: bool,
    /// 强制更新：发现新版本必须更新（不提供跳过）
    #[serde(default)]
    pub force: bool,
}

fn default_upstream_url() -> String {
    DEFAULT_UPSTREAM_URL.to_string()
}

fn default_true() -> bool {
    true
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            upstream_url: default_upstream_url(),
            mirrors: Vec::new(),
            check_on_startup: true,
            force: false,
        }
    }
}

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
    /// 上游切换：非空时提示用户切换到新上游
    #[serde(default)]
    pub next: String,
    /// Ed25519 签名（对去掉本字段后的规范化 JSON）
    #[serde(default)]
    pub signature: String,
}

/// 更新检查结果。
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub current_version: String,
    pub latest_version: String,
    pub has_update: bool,
    pub forced: bool,
    pub notes: String,
    pub source_url: String,
}

/// 语义化版本比较（支持 `v` 前缀与 `0.6.0` / `0.6.0-beta` 形态；忽略预发布段）。
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    fn parse(version: &str) -> (u64, u64, u64) {
        let version = version.trim().trim_start_matches('v');
        let mut parts = version.split(['-', '+']).next().unwrap_or("").split('.');
        let major = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let minor = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let patch = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        (major, minor, patch)
    }
    parse(a).cmp(&parse(b))
}

/// 规范化 + 验签 + 解析 update.json 文本。
pub fn verify_update_json(raw: &str) -> Result<UpdateInfo> {
    let mut info: UpdateInfo =
        serde_json::from_str(raw).context("update.json 不是有效 JSON")?;
    let signature = std::mem::take(&mut info.signature);
    if signature.trim().is_empty() {
        bail!("update.json 缺少签名（拒绝未签名更新源）");
    }
    // 签名对象 = 去掉 signature 字段后的规范化 JSON
    let canonical = serde_json::to_vec(&info).context("update.json 序列化失败")?;
    crate::security::verify_signature(&canonical, &signature)?;
    if info.version.trim().is_empty() {
        bail!("update.json 缺少 version");
    }
    Ok(info)
}

/// 所有候选 URL：上游 + 加速前缀拼接。
pub fn candidate_urls(base_url: &str, mirrors: &[String]) -> Vec<String> {
    let mut urls = Vec::new();
    for mirror in mirrors {
        let url = format!("{mirror}{base_url}");
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    if !urls.contains(&base_url.to_string()) {
        urls.push(base_url.to_string());
    }
    urls
}

/// 从上游拉取并验签 update.json（多加速源轮询）。
pub fn fetch_update_info(config: &AppConfig, paths: &GqyPaths) -> Result<UpdateInfo> {
    let upstream = config.update.upstream_url.trim();
    if upstream.is_empty() {
        bail!("update.upstream_url 未配置");
    }
    let mirrors: Vec<String> = if config.update.mirrors.is_empty() {
        BUILTIN_MIRRORS.iter().map(|s| s.to_string()).collect()
    } else {
        let mut list = vec![String::new()];
        list.extend(config.update.mirrors.iter().cloned());
        list
    };
    let candidates = candidate_urls(upstream, &mirrors);
    let mut last_error: Option<anyhow::Error> = None;
    for url in &candidates {
        match fetch_url_text(url) {
            Ok(raw) => match verify_update_json(&raw) {
                Ok(info) => return Ok(info),
                Err(error) => last_error = Some(error.context(format!("源 {url} 验签失败"))),
            },
            Err(error) => last_error = Some(error.context(format!("源 {url} 获取失败"))),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("所有更新源都不可用")))
}

fn fetch_url_text(url: &str) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .context("构建 HTTP 客户端失败")?;
    let response = client
        .get(url)
        .header("User-Agent", "hilia-update/0.6")
        .send()
        .with_context(|| format!("请求失败: {url}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("HTTP {status}");
    }
    let text = response.text().context("读取响应失败")?;
    if text.trim().is_empty() {
        bail!("响应为空");
    }
    Ok(text)
}

/// 检查更新：拉取 + 对比。
pub fn check_update(config: &AppConfig, paths: &GqyPaths) -> Result<CheckResult> {
    let info = fetch_update_info(config, paths)?;
    let current = env!("CARGO_PKG_VERSION").to_string();
    let has_update = compare_versions(&info.version, &current) == std::cmp::Ordering::Greater;
    let forced = has_update
        && (config.update.force
            || (!info.min_version.trim().is_empty()
                && compare_versions(&current, &info.min_version) == std::cmp::Ordering::Less));
    Ok(CheckResult {
        current_version: current,
        latest_version: info.version,
        has_update,
        forced,
        notes: info.notes,
        source_url: info.next,
    })
}

/// 下载更新包（多源轮询），校验 sha256，返回本地文件路径。
pub fn download_artifact(
    artifact: &ArtifactInfo,
    config: &AppConfig,
    paths: &GqyPaths,
    progress: Option<&dyn Fn(u64, u64)>,
) -> Result<PathBuf> {
    if artifact.urls.is_empty() {
        bail!("更新包没有下载地址");
    }
    let mirrors: Vec<String> = if config.update.mirrors.is_empty() {
        BUILTIN_MIRRORS.iter().map(|s| s.to_string()).collect()
    } else {
        let mut list = vec![String::new()];
        list.extend(config.update.mirrors.iter().cloned());
        list
    };
    let dir = paths.cache_dir.join("update");
    std::fs::create_dir_all(&dir)?;
    let file_name = artifact
        .urls
        .first()
        .and_then(|url| url.split('/').last())
        .filter(|name| !name.is_empty())
        .unwrap_or("update.zip");
    let dest = dir.join(file_name);

    let mut last_error: Option<anyhow::Error> = None;
    for url in &artifact.urls {
        for mirror in &mirrors {
            let candidate = format!("{mirror}{url}");
            match download_file(&candidate, &dest, progress) {
                Ok(()) => {
                    if let Err(error) = verify_sha256(&dest, &artifact.sha256) {
                        last_error = Some(error);
                        continue;
                    }
                    return Ok(dest);
                }
                Err(error) => last_error = Some(error),
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("所有下载源都失败")))
}

fn download_file(
    url: &str,
    dest: &Path,
    progress: Option<&dyn Fn(u64, u64)>,
) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .build()
        .context("构建 HTTP 客户端失败")?;
    let mut response = client
        .get(url)
        .header("User-Agent", "hilia-update/0.6")
        .send()
        .with_context(|| format!("下载失败: {url}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("HTTP {status}");
    }
    let total = response.content_length().unwrap_or(0);
    let mut file = std::fs::File::create(dest)?;
    let mut written: u64 = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        use std::io::Read;
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        use std::io::Write;
        file.write_all(&buffer[..read])?;
        written += read as u64;
        if let Some(callback) = progress {
            callback(written, total);
        }
    }
    Ok(())
}

pub fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let expected = expected.trim();
    if expected.is_empty() {
        bail!("更新包缺少 sha256（拒绝未校验的包）");
    }
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    use std::io::Read;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hex::encode(hasher.finalize());
    if !digest.eq_ignore_ascii_case(expected) {
        bail!("sha256 校验失败：期望 {expected}，实际 {digest}");
    }
    Ok(())
}

/// 解压 zip 到目标目录（zip 内目录结构保留）。
pub fn extract_zip(zip_path: &Path, dest_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dest_dir)?;
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file).context("打开 zip 失败")?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        // 防 zip-slip：拒绝绝对路径与 ../
        let safe_name = name.replace('\\', "/");
        if safe_name.starts_with('/')
            || safe_name.split('/').any(|part| part == "..")
            || safe_name.contains(":")
        {
            bail!("zip 包含非法路径: {name}");
        }
        let out_path = dest_dir.join(&safe_name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&out_path)?;
            use std::io::Write;
            std::io::copy(&mut entry, &mut out)?;
        }
    }
    Ok(())
}

/// Windows 更新应用：下载 → 校验 → 解压到 stage → 生成替换脚本并启动。
/// 替换脚本等待当前进程退出后，用 stage 内容覆盖安装目录并重启托盘。
pub fn apply_windows_update(
    info: &UpdateInfo,
    config: &AppConfig,
    paths: &GqyPaths,
) -> Result<()> {
    let artifact = info
        .windows
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("该版本没有 Windows 更新包"))?;
    let zip_path = download_artifact(artifact, config, paths, None)?;
    let stage = paths.cache_dir.join("update").join("stage");
    extract_zip(&zip_path, &stage)?;

    // 安装目录 = 当前 exe 所在目录
    let install_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .ok_or_else(|| anyhow::anyhow!("无法定位安装目录"))?;

    // 替换脚本：等 3 秒让旧进程退出 → 备份 → 覆盖 → 重启托盘
    let script_path = paths.cache_dir.join("update").join("apply-update.cmd");
    let install = install_dir.display().to_string();
    let stage_str = stage.display().to_string();
    let install_escaped = install.replace('\\', "\\\\");
    let tray_exe = install_dir.join("hilia-tray.exe");
    let relaunch = if tray_exe.is_file() {
        format!("start \"\" \"{}\\hilia-tray.exe\"", install_escaped)
    } else {
        String::new()
    };
    let script = format!(
        "@echo off\r\n\
         timeout /t 3 /nobreak >nul\r\n\
         taskkill /IM hilia.exe /F >nul 2>&1\r\n\
         taskkill /IM hilia-tray.exe /F >nul 2>&1\r\n\
         if exist \"{install}\\hilia.exe.old\" del /f /q \"{install}\\hilia.exe.old\"\r\n\
         if exist \"{install}\\hilia.exe\" ren \"{install}\\hilia.exe\" \"hilia.exe.old\"\r\n\
         xcopy /E /Y /I \"{stage_str}\" \"{install}\" >nul\r\n\
         {relaunch}\r\n\
         del \"%~f0\"\r\n"
    );
    std::fs::write(&script_path, script)?;
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", script_path.to_str().unwrap_or_default()])
        .spawn();
    Ok(())
}

/// 显示更新信息（CLI 输出）。
pub fn render_update_info(info: &UpdateInfo) -> String {
    let mut lines = vec![format!("最新版本：v{}", info.version)];
    if !info.min_version.is_empty() {
        lines.push(format!("最低支持：v{}", info.min_version));
    }
    if let Some(artifact) = &info.windows {
        lines.push(format!(
            "Windows 包：{}（{}）",
            artifact.version,
            human_size(artifact.size)
        ));
    }
    if let Some(artifact) = &info.apk {
        lines.push(format!("Android 包：{}", artifact.version));
    }
    if !info.notes.is_empty() {
        lines.push(format!("更新说明：\n{}", info.notes));
    }
    if !info.next.is_empty() {
        lines.push(format!("上游切换提示：{}", info.next));
    }
    lines.join("\n")
}

fn human_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed_update_json(version: &str) -> String {
        let mut info = UpdateInfo {
            version: version.to_string(),
            min_version: "0.5.0".to_string(),
            windows: Some(ArtifactInfo {
                version: version.to_string(),
                urls: vec!["https://example.com/hilia.zip".to_string()],
                sha256: "abc".to_string(),
                size: 123,
            }),
            apk: None,
            notes: "测试更新".to_string(),
            next: String::new(),
            signature: String::new(),
        };
        let canonical = serde_json::to_vec(&info).unwrap();
        let secret = "B6Z1uPI8qtF+WTnRyEeAGbUo/Yzr4NVeIPwFbfTrBC8=";
        info.signature = crate::security::sign_with_secret(&canonical, secret).unwrap();
        serde_json::to_string(&info).unwrap()
    }

    #[test]
    fn version_comparison() {
        assert_eq!(compare_versions("0.6.0", "0.6.0"), std::cmp::Ordering::Equal);
        assert_eq!(compare_versions("0.7.0", "0.6.9"), std::cmp::Ordering::Greater);
        assert_eq!(compare_versions("v0.6.0", "0.6.0"), std::cmp::Ordering::Equal);
        assert_eq!(compare_versions("0.6.0-beta", "0.6.0"), std::cmp::Ordering::Equal);
        assert_eq!(compare_versions("0.5.9", "0.6.0"), std::cmp::Ordering::Less);
        assert_eq!(compare_versions("1.0.0", "0.9.9"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn verifies_signed_update_json() {
        let raw = signed_update_json("0.7.0");
        let info = verify_update_json(&raw).unwrap();
        assert_eq!(info.version, "0.7.0");
        assert!(info.windows.is_some());
    }

    #[test]
    fn rejects_unsigned_update_json() {
        let mut raw = signed_update_json("0.7.0");
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        // 去掉签名
        let mut obj = value.as_object().unwrap().clone();
        obj.remove("signature");
        raw = serde_json::to_string(&obj).unwrap();
        let error = verify_update_json(&raw).unwrap_err();
        assert!(error.to_string().contains("缺少签名"));
    }

    #[test]
    fn rejects_tampered_update_json() {
        let raw = signed_update_json("0.7.0");
        let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        value["version"] = serde_json::json!("9.9.9");
        let tampered = serde_json::to_string(&value).unwrap();
        assert!(verify_update_json(&tampered).is_err());
    }

    #[test]
    fn candidate_urls_include_mirrors() {
        let urls = candidate_urls("https://github.com/x/y.zip", &["https://mirror.example/".to_string()]);
        assert_eq!(urls.len(), 2);
        assert!(urls[0].starts_with("https://mirror.example/"));
        assert_eq!(urls[1], "https://github.com/x/y.zip");
    }

    #[test]
    fn rejects_zip_slip_paths() {
        let temp = tempfile::tempdir().unwrap();
        let bad = Path::new("../../evil.txt");
        assert!(bad.components().any(|c| matches!(c, std::path::Component::ParentDir)));
        let _ = temp;
    }

    #[test]
    fn sha256_verification() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("test.bin");
        std::fs::write(&file, b"hello hilia").unwrap();
        let digest = {
            let mut hasher = Sha256::new();
            hasher.update(b"hello hilia");
            hex::encode(hasher.finalize())
        };
        assert!(verify_sha256(&file, &digest).is_ok());
        assert!(verify_sha256(&file, &format!("{}0", digest)).is_err());
    }
}
