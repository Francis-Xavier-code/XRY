//! 语音能力（本地、零 API 成本）：
//! - TTS：macOS 用自带 `say`，Windows 用 PowerShell + System.Speech（零依赖）
//! - STT：macOS 用 speech-tool.swift（SFSpeechRecognizer 本地离线识别），
//!   Windows 用 PowerShell + System.Speech.Recognition（依赖系统语音包）
//!
//! 工具：`speak`（读一段文字）、`listen_audio`（识别音频文件）
//! CLI：`hilia tts "文字"`、`hilia stt 音频文件`

use crate::paths::GqyPaths;
use anyhow::{bail, Context, Result};
use std::process::Command;

/// 文字转语音：默认直接播放，可指定输出文件。
/// - macOS：`say`（.aiff/.m4a）
/// - Windows：PowerShell + System.Speech（播放或输出 .wav）
pub fn speak(text: &str, voice: Option<&str>, output: Option<&str>) -> Result<()> {
    let text = text.trim();
    if text.is_empty() {
        bail!("text is required");
    }
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("say");
        if let Some(voice) = voice.filter(|v| !v.trim().is_empty()) {
            command.arg("-v").arg(voice);
        }
        if let Some(output) = output {
            command.arg("-o").arg(output);
        }
        let status = command
            .arg(text)
            .status()
            .with_context(|| "failed to run `say`; TTS requires macOS")?;
        if !status.success() {
            bail!("say exited with status {status}");
        }
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        speak_windows(text, voice, output)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (voice, output);
        bail!("TTS is only supported on macOS and Windows")
    }
}

/// Windows TTS：PowerShell + System.Speech（系统自带，无需额外安装）。
#[cfg(target_os = "windows")]
fn speak_windows(text: &str, voice: Option<&str>, output: Option<&str>) -> Result<()> {
    // 用单引号转义（PowerShell 单引号内双单引号表示一个单引号）
    let text_ps = format!("'{}'", text.replace('\'', "''"));
    let mut ps = String::from(
        "Add-Type -AssemblyName System.Speech; \
         $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
         $s.Rate = 0; ",
    );
    if let Some(v) = voice.filter(|v| !v.trim().is_empty()) {
        ps.push_str(&format!(
            "try {{ $s.SelectVoice('{}') }} catch {{}}; ",
            v.replace('\'', "''")
        ));
    }
    if let Some(output) = output {
        ps.push_str(&format!(
            "$s.SetOutputToWaveFile('{}'); ",
            output.replace('\'', "''")
        ));
    }
    ps.push_str(&format!("$s.Speak({text_ps}); $s.Dispose()"));

    let status = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
        .status()
        .with_context(|| {
            "failed to run PowerShell System.Speech; TTS requires Windows PowerShell"
        })?;
    if !status.success() {
        bail!("PowerShell TTS exited with status {status}");
    }
    Ok(())
}

/// 列出可用的系统语音。
/// - macOS：`say -v '?'`
/// - Windows：PowerShell 枚举 System.Speech 已安装语音
pub fn list_voices() -> Result<Vec<String>> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("say")
            .args(["-v", "?"])
            .output()
            .with_context(|| "failed to list voices")?;
        let text = String::from_utf8_lossy(&output.stdout);
        let voices = text
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .map(str::to_string)
            .collect();
        Ok(voices)
    }
    #[cfg(target_os = "windows")]
    {
        let ps = "Add-Type -AssemblyName System.Speech; \
                  $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
                  $s.GetInstalledVoices() | ForEach-Object { $_.VoiceInfo.Name }; \
                  $s.Dispose()";
        let output = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", ps])
            .output()
            .with_context(|| "failed to list Windows voices")?;
        if !output.status.success() {
            bail!(
                "PowerShell voice listing failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let voices = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect();
        Ok(voices)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        bail!("TTS is only supported on macOS and Windows")
    }
}

/// 语音转文字：把本地音频文件识别成文字。
/// - macOS：speech-tool.swift（SFSpeechRecognizer 离线识别）
/// - Windows：PowerShell + System.Speech.Recognition（依赖系统语音识别包）
pub fn transcribe(paths: &GqyPaths, audio_path: &str, locale: Option<&str>) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        transcribe_macos(paths, audio_path, locale)
    }
    #[cfg(target_os = "windows")]
    {
        let _ = paths;
        transcribe_windows(audio_path, locale)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (paths, audio_path, locale);
        bail!("STT is only supported on macOS and Windows")
    }
}

/// macOS 离线 STT：speech-tool.swift。
#[cfg(target_os = "macos")]
fn transcribe_macos(paths: &GqyPaths, audio_path: &str, locale: Option<&str>) -> Result<String> {
    let tool = speech_tool_path(paths);
    if !tool.is_file() {
        bail!("speech-tool.swift not found at {}", tool.display());
    }
    let locale = locale.unwrap_or("zh-Hans");
    let output = Command::new("swift")
        .arg(&tool)
        .arg(audio_path)
        .arg(locale)
        .output()
        .with_context(|| "failed to run speech-tool.swift (requires macOS + swift)")?;
    if !output.status.success() {
        bail!(
            "speech-tool failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .with_context(|| format!("speech-tool returned invalid JSON: {stdout}"))?;
    if parsed.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let error = parsed
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown error");
        bail!("speech recognition failed: {error}");
    }
    parsed
        .get("text")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("speech-tool returned no text"))
}

/// Windows STT：PowerShell + System.Speech.Recognition 识别 .wav。
/// 注意：识别中文需要系统安装「中文(简体)语音识别」语言包。
#[cfg(target_os = "windows")]
fn transcribe_windows(audio_path: &str, locale: Option<&str>) -> Result<String> {
    let locale = locale.unwrap_or("zh-CN");
    // locale（如 zh-CN / en-US）会去掉连字符后的部分作为 culture 前缀校验
    let ps = format!(
        "Add-Type -AssemblyName System.Speech; \
         $rec = New-Object System.Speech.Recognition.SpeechRecognitionEngine('{locale}'); \
         $rec.SetInputToWaveFile('{path}'); \
         $result = $rec.Recognize(); \
         $rec.Dispose(); \
         if ($result) {{ $result.Text }} else {{ Write-Output '' }}",
        locale = locale.replace('\'', "''"),
        path = audio_path.replace('\'', "''"),
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
        .output()
        .with_context(|| "failed to run PowerShell STT (System.Speech)")?;
    if !output.status.success() {
        bail!(
            "Windows STT failed: {}（请确认已安装对应语言的语音识别包：设置 → 时间和语言 → 语言 → 添加语音）",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    if text.is_empty() {
        bail!("未识别出语音内容（音频应为 16kHz 单声道 WAV，且系统需安装对应语音识别语言包）");
    }
    Ok(text.to_string())
}

/// 定位 speech-tool.swift：安装版 = share/hilia/scripts；源码 = <repo>/src/scripts。
#[cfg(target_os = "macos")]
fn speech_tool_path(paths: &GqyPaths) -> std::path::PathBuf {
    let candidates = [
        paths.share_dir.join("scripts/speech-tool.swift"),
        paths.share_dir.join("src/scripts/speech-tool.swift"),
    ];
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| paths.share_dir.join("scripts/speech-tool.swift"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn tts_generates_audio_file() {
        let out = std::env::temp_dir().join(format!("hilia-tts-test-{}.aiff", std::process::id()));
        let _ = std::fs::remove_file(&out);
        speak("test", None, Some(out.to_str().unwrap())).unwrap();
        assert!(out.is_file(), "say should produce an audio file");
        let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        assert!(size > 1000, "audio file should have content, got {size} bytes");
        let _ = std::fs::remove_file(&out);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn lists_system_voices() {
        let voices = list_voices().unwrap();
        assert!(!voices.is_empty(), "macOS should have voices");
    }

    #[test]
    fn rejects_empty_text() {
        assert!(speak("", None, None).is_err());
    }
}
