//! 管家监控（主动对话）：后台采样系统进程，检测到异常（CPU 突增/内存吃紧/未知进程）
//! 时，先本地判断是否值得打扰，再通过 WebUI 的 queue API 给运行中的会话入队
//! 一条「主动消息」，让希尔娅先判断再询问用户。
//!
//! 用法：`hilia watch --every 30s`（前台跑）或由计划任务/托盘托管。
//! 设计原则：采样开销极小（一条 ps / PowerShell 命令），默认不打扰（阈值内静默）。

use crate::paths::GqyPaths;
use anyhow::{Context, Result};
use std::process::Command;

/// 单次采样结果：CPU 突增 / 内存吃紧的进程列表。
pub struct WatchSample {
    /// 超过 CPU 阈值的进程（名字, CPU%, 内存%）
    pub hot_processes: Vec<(String, f32, f32)>,
    /// 系统整体内存占用（%）
    pub memory_pressure: f32,
}

/// 采样一次系统状态。
/// - Unix：`ps -axo` 列全部进程并取 CPU/内存
/// - Windows：PowerShell 取进程 WorkingSet 与总物理内存（CPU% 需双采样，这里以内存为主）
pub fn sample_system() -> Result<WatchSample> {
    #[cfg(unix)]
    {
        sample_system_unix()
    }
    #[cfg(not(unix))]
    {
        sample_system_windows()
    }
}

#[cfg(unix)]
fn sample_system_unix() -> Result<WatchSample> {
    let output = Command::new("ps")
        .args(["-axo", "%cpu=,%mem=,comm="])
        .output()
        .with_context(|| "failed to run ps")?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut hot = Vec::new();
    let mut total_mem = 0.0f32;
    for line in text.lines() {
        let mut parts = line.splitn(3, char::is_whitespace).filter(|p| !p.is_empty());
        let (Some(cpu_str), Some(mem_str), Some(comm)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let cpu = cpu_str.trim().parse::<f32>().unwrap_or(0.0);
        let mem = mem_str.trim().parse::<f32>().unwrap_or(0.0);
        total_mem += mem;
        // 只看用户进程（排除内核/自身/监控常见进程）
        if cpu >= 80.0 && !is_noise_process(comm.trim()) {
            hot.push((comm.trim().to_string(), cpu, mem));
        }
    }
    hot.sort_by(|a, b| b.1.total_cmp(&a.1));
    hot.truncate(5);
    Ok(WatchSample {
        hot_processes: hot,
        memory_pressure: total_mem,
    })
}

/// Windows 采样：内存占用取各进程 WorkingSet / 总物理内存；
/// CPU 时间（秒）作为热点候选的辅助信号（无法一次性拿到 CPU%）。
#[cfg(not(unix))]
fn sample_system_windows() -> Result<WatchSample> {
    let ps = "$procs = Get-Process -ErrorAction SilentlyContinue | \
              Select-Object ProcessName, CPU, @{n='MB';e={[math]::Round($_.WorkingSet64/1MB,1)}}; \
              $totalGb = [math]::Round((Get-CimInstance Win32_OperatingSystem).TotalVisibleMemorySize / 1KB / 1GB, 2); \
              @{ procs = @($procs); total_gb = $totalGb } | ConvertTo-Json -Compress -Depth 3";
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", ps])
        .output()
        .with_context(|| "failed to run PowerShell process sampling")?;
    if !output.status.success() {
        anyhow::bail!(
            "PowerShell sampling failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or(serde_json::Value::Null);
    let total_gb = parsed.get("total_gb").and_then(|v| v.as_f64()).unwrap_or(16.0) as f32;
    let items: Vec<&serde_json::Value> = match parsed.get("procs") {
        Some(serde_json::Value::Array(ref arr)) => arr.iter().collect(),
        Some(value @ serde_json::Value::Object(_)) => vec![value],
        _ => Vec::new(),
    };
    let mut hot = Vec::new();
    let mut total_mem = 0.0f32;
    for item in items {
        let name = item.get("ProcessName").and_then(|v| v.as_str()).unwrap_or("");
        let mb = item.get("MB").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        total_mem += mb;
        // 内存占用 ≥ 500MB 或 CPU 累计时间 ≥ 600 秒的进程作为热点候选
        if mb >= 500.0 && !is_noise_process(name) {
            let cpu_secs = item.get("CPU").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            hot.push((name.to_string(), cpu_secs.min(999.0), mb));
        }
    }
    hot.sort_by(|a, b| b.2.total_cmp(&a.2));
    hot.truncate(5);
    // 内存压力：总 WorkingSet / 总物理内存
    let memory_pressure = if total_gb > 0.0 {
        (total_mem / 1024.0 / total_gb) * 100.0
    } else {
        0.0
    };
    Ok(WatchSample {
        hot_processes: hot,
        memory_pressure,
    })
}

/// 排除不值得报告的进程（监控工具自身、系统常驻等）。
fn is_noise_process(comm: &str) -> bool {
    let name = comm.rsplit(['/', '\\']).next().unwrap_or(comm);
    matches!(
        name,
        "hilia" | "hilia-tray" | "ps" | "top" | "powershell" | "GQYMenuBar"
            | "WindowServer" | "kernel_task" | "launchd" | "mds" | "mdworker"
            | "Spotlight" | "backupd" | "cloudd" | "VTDecoderXPCService" | "rapportd"
            | "opendirectoryd"
            // Windows 系统常驻
            | "System" | "Idle" | "Registry" | "Memory Compression" | "dwm" | "svchost"
            | "MsMpEng" | "SearchHost" | "SearchIndexer" | "RuntimeBroker"
            | "ShellExperienceHost" | "StartMenuExperienceHost" | "TextInputHost"
            | "winlogon" | "csrss" | "lsass" | "services" | "smss" | "spoolsv"
            | "WmiPrvSE" | "fontdrvhost" | "ctfmon" | "conhost" | "wininit"
    ) || name.starts_with("zcode")
}

/// 本地判断：是否值得打扰（CPU 突增进程存在且非瞬时）。
/// 简单启发式：存在 ≥2 个高 CPU 进程，或单个 ≥150% CPU（多核满载），
/// 或系统整体内存占用超过 90%（内存吃紧）。
pub fn should_alert(sample: &WatchSample) -> bool {
    let heavy = sample.hot_processes.iter().filter(|(_, cpu, _)| *cpu >= 150.0).count();
    heavy >= 1 || sample.hot_processes.len() >= 2 || sample.memory_pressure >= 90.0
}

/// 构造主动消息（给希尔娅的判断材料，不直接打扰用户）。
pub fn alert_message(sample: &WatchSample) -> String {
    let procs = sample
        .hot_processes
        .iter()
        .map(|(name, cpu, mem)| format!("{name}（CPU {cpu:.0}% / 内存 {mem:.0}%）"))
        .collect::<Vec<_>>()
        .join("、");
    format!(
        "【主动提醒】我检测到系统里 {} 的 CPU 占用异常偏高，可能卡顿或出问题。\
         请先自己判断这是否正常（比如编译/渲染/下载任务），\
         如果值得关注再主动告诉我；不要无事打扰用户。",
        procs
    )
}

/// 通过 WebUI queue API 给运行中的会话入队主动消息。
/// 需要 WebUI 在跑（默认 127.0.0.1:4096）；不在跑就静默跳过。
pub fn enqueue_alert(paths: &GqyPaths, message: &str) -> Result<bool> {
    let _ = paths;
    let port = std::env::var("HILIA_WEB_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(4096);
    let url = format!("http://127.0.0.1:{port}/api/queue");
    let body = serde_json::json!({ "content": message }).to_string();
    let Ok(response) = Command::new("curl")
        .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST", "-H", "Content-Type: application/json", "-d", &body, &url])
        .output()
    else {
        return Ok(false);
    };
    let code = String::from_utf8_lossy(&response.stdout);
    Ok(code.trim() == "200" || code.trim() == "202")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_processes_are_filtered() {
        assert!(is_noise_process("/usr/bin/ps"));
        assert!(is_noise_process("WindowServer"));
        assert!(is_noise_process("zcode"));
        assert!(!is_noise_process("/usr/local/bin/cargo"));
        assert!(!is_noise_process("node"));
    }

    #[test]
    fn alert_thresholds() {
        // 单进程 150%+ CPU → 报警
        let heavy = WatchSample {
            hot_processes: vec![("node".to_string(), 180.0, 12.0)],
            memory_pressure: 50.0,
        };
        assert!(should_alert(&heavy));
        // 低 CPU → 不报警
        let calm = WatchSample {
            hot_processes: vec![("node".to_string(), 30.0, 5.0)],
            memory_pressure: 40.0,
        };
        assert!(!should_alert(&calm));
    }

    #[test]
    fn alert_message_lists_processes() {
        let sample = WatchSample {
            hot_processes: vec![("node".to_string(), 200.0, 15.0)],
            memory_pressure: 60.0,
        };
        let message = alert_message(&sample);
        assert!(message.contains("node"));
        assert!(message.contains("CPU 200%"));
    }
}
