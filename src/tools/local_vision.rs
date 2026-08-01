//! 本地视觉工具：离线图片分析（OCR 为主）。
//!
//! 不消耗任何模型 API 额度——opencode/DeepSeek 等模型超额时，
//! 希尔娅仍然可以"看"图片（读文字、识别画面内容）。
//! - macOS：Apple Vision（swift + Vision 框架）
//! - Windows：Windows.Media.Ocr（PowerShell WinRT，需系统安装 OCR 语言包）
//! 免费、离线、无隐私外泄。

use super::{ToolRegistry, ToolSpec};
use crate::i18n::agent_text as t;
use crate::paths::GqyPaths;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;

/// 定位本地视觉脚本：安装版 = share/hilia/scripts；源码 = <repo>/src/scripts。
fn vision_tool_path(paths: &GqyPaths) -> Result<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["vision-ocr.ps1"]
    } else {
        &["vision-tool.swift"]
    };
    let mut candidates = Vec::new();
    for name in names {
        candidates.push(paths.share_dir.join("scripts").join(name));
        candidates.push(paths.share_dir.join("src/scripts").join(name));
    }
    for candidate in &candidates {
        if candidate.is_file() {
            return Ok(candidate.clone());
        }
    }
    anyhow::bail!(
        "找不到本地视觉工具 {}（预期位置：{}）",
        names[0],
        candidates[0].display()
    )
}

pub fn register(registry: &mut ToolRegistry, paths: GqyPaths) {
    registry.register(ToolSpec::new(
        "analyze_image_local",
        t(
            "Analyze a local image offline (OCR text; macOS Apple Vision also adds classification and object detection; Windows OCR only). Free, no API quota. Use when the vision model is rate-limited or unavailable.",
            "本地离线分析图片：OCR 文字（macOS Apple Vision 另有分类/对象检测，Windows 仅 OCR）。免费不耗 API 额度。视觉模型限流或不可用时看图。",
        ),
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": t("Local image file path.", "本地图片文件路径。") }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        move |args| {
            let paths = paths.clone();
            async move { analyze_image_local(args, &paths).await }
        },
    ));
}

async fn analyze_image_local(args: Value, paths: &GqyPaths) -> Result<String> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("path is required"))?
        .to_string();
    let script = vision_tool_path(paths)?;
    let display_path = path.clone();

    #[cfg(target_os = "windows")]
    let parsed: Value = {
        let output = tokio::task::spawn_blocking(move || {
            Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                    script.to_str().unwrap_or_default(),
                    &path,
                ])
                .output()
                .context("running vision-ocr.ps1 (Windows OCR)")
        })
        .await??;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!(
                "本地视觉分析失败（{}）：{}（Windows OCR 需在 设置 → 时间和语言 → 语言 → 中文 → 选项 中安装「光学字符识别」）",
                display_path,
                stderr.chars().take(200).collect::<String>()
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str(stdout.trim())
            .unwrap_or_else(|_| json!({ "raw": stdout.trim().to_string() }))
    };

    #[cfg(not(target_os = "windows"))]
    let parsed: Value = {
        let output = tokio::task::spawn_blocking(move || {
            Command::new("swift")
                .arg(&script)
                .arg(&path)
                .arg("all")
                .output()
                .context("running vision-tool (swift + Vision)")
        })
        .await??;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!(
                "本地视觉分析失败（{}）：{}",
                display_path,
                stderr.chars().take(200).collect::<String>()
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str(stdout.trim())
            .unwrap_or_else(|_| json!({ "raw": stdout.trim().to_string() }))
    };

    let ocr = parsed
        .get("ocr")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let labels = parsed
        .get("labels")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let objects = parsed
        .get("objects")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();

    let mut sections = Vec::new();
    if !labels.is_empty() {
        sections.push(format!(
            "{}：{}",
            t("Scene labels", "画面识别"),
            labels.join("、")
        ));
    }
    if !ocr.is_empty() {
        sections.push(format!(
            "{}：\n{}",
            t("Text in image", "图片中的文字"),
            ocr.join("\n")
        ));
    }
    if !objects.is_empty() {
        sections.push(format!(
            "{}：{}",
            t("Detected objects", "检测到的对象"),
            objects.join("、")
        ));
    }
    if sections.is_empty() {
        return Ok(t(
            "Local analysis found nothing notable in this image.",
            "本地分析未从图片中识别出明显内容。",
        )
        .to_string());
    }
    Ok(sections.join("\n\n"))
}
