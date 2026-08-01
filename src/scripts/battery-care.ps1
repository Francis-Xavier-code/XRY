#!powershell
# 显示名称：笔记本电池管理
# 描述：查询电池信息（剩余电量、充电状态、续航），生成电池健康报告。Windows 无通用充电阈值命令行接口，设置充电上限需用厂商工具（联想智能引擎 / MyASUS / 华硕管家 等）。
# Description: Query laptop battery info (charge level, charging state, runtime), generate battery health report. Windows has no generic charge-limit CLI; vendor tools (Lenovo Vantage / MyASUS) are required for charge limits.
# 参数（stdin JSON，可选 action）：status 查询电池状态（默认）；report 生成 powercfg 电池报告；set 设置充电阈值（仅提示厂商工具路径）

$ErrorActionPreference = "SilentlyContinue"

$inputJson = [Console]::In.ReadToEnd()
$args = @{}
if ($inputJson -and $inputJson.Trim() -ne "") {
    try { $args = $inputJson | ConvertFrom-Json } catch {}
}
$action = ""
if ($args.action) { $action = [string]$args.action }

function Get-BatteryStatus {
    $battery = Get-CimInstance Win32_Battery
    if (-not $battery) {
        Write-Output "未检测到电池（可能为台式机，或电池驱动未就绪）。"
        return
    }
    $charge = $battery.EstimatedChargeRemaining
    $status = switch ($battery.BatteryStatus) {
        1 { "放电中" }
        2 { "充电中 / 交流供电" }
        3 { "放电中（低电量）" }
        4 { "电量耗尽" }
        5 { "满电" }
        6 { "充电中" }
        default { "未知状态 ($($battery.BatteryStatus))" }
    }
    $runtimeMin = $battery.EstimatedRunTime
    $runtime = if ($runtimeMin -gt 0 -and $runtimeMin -lt 0xFFFF) {
        "$runtimeMin 分钟（约 $([math]::Round($runtimeMin / 60, 1)) 小时）"
    } else {
        "未知（充电中或电量充足）"
    }
    Write-Output "电池状态：$status"
    Write-Output "剩余电量：$charge%"
    Write-Output "预计续航：$runtime"
}

function New-BatteryReport {
    $out = Join-Path $env:USERPROFILE "battery-report.html"
    powercfg /batteryreport /output $out | Out-Null
    if (Test-Path $out) {
        Write-Output "电池健康报告已生成：$out"
        Write-Output "打开方法：在资源管理器双击，或运行 start $out"
        Write-Output "报告含设计容量 / 充满容量 / 循环次数 / 最近使用记录，可直接查看电池健康度。"
    } else {
        Write-Output "生成电池报告失败（powercfg /batteryreport 未成功）。"
    }
}

function Set-ChargeLimitHint {
    Write-Output "Windows 没有通用的充电阈值命令行接口。"
    Write-Output "如果你的笔记本支持充电上限，请使用厂商工具："
    Write-Output "  - 联想：联想智能引擎 / Lenovo Vantage（电池养护模式）"
    Write-Output "  - 华硕：MyASUS（电池健康充电）"
    Write-Output "  - 戴尔：Dell Power Manager"
    Write-Output "  - 惠普：HP 支持助手 / BIOS 设置"
    Write-Output "或在 BIOS/UEFI 的电池设置中开启充电上限（如 80%）。"
}

switch ($action) {
    "report" { Get-BatteryStatus; Write-Output ""; New-BatteryReport }
    "set" { Set-ChargeLimitHint }
    default { Get-BatteryStatus }
}
