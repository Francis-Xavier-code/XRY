#!powershell
# 显示名称：进程资源占用排行
# 描述：查看当前占用最高的进程（内存/CPU 累计时间），排查卡顿或资源异常。Windows 版基于 Get-Process。
# Description: List top resource-consuming processes (memory / CPU time) to diagnose slowness. Windows version based on Get-Process.
# 参数（stdin JSON，可选）：top 显示条数，默认 10

$ErrorActionPreference = "SilentlyContinue"

$inputJson = [Console]::In.ReadToEnd()
$args = @{}
if ($inputJson -and $inputJson.Trim() -ne "") {
    try { $args = $inputJson | ConvertFrom-Json } catch {}
}
$top = 10
if ($args.top) { try { $top = [int]$args.top } catch {} }
if ($top -lt 1 -or $top -gt 50) { $top = 10 }

$procs = Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -ne "" }

Write-Output "按内存占用（前 $top 名）："
Write-Output ("{0,-28} {1,10} {2,12}" -f "进程", "内存(MB)", "CPU秒")
$memTop = $procs | Sort-Object WorkingSet64 -Descending | Select-Object -First $top
foreach ($p in $memTop) {
    $mb = [math]::Round($p.WorkingSet64 / 1MB, 1)
    $cpu = if ($p.CPU) { [math]::Round($p.CPU, 0) } else { 0 }
    Write-Output ("{0,-28} {1,10} {2,12}" -f $p.ProcessName, $mb, $cpu)
}

Write-Output ""
Write-Output "按 CPU 累计时间（前 $top 名）："
$cpuTop = $procs | Where-Object { $_.CPU -gt 0 } | Sort-Object CPU -Descending | Select-Object -First $top
if ($cpuTop) {
    foreach ($p in $cpuTop) {
        $mb = [math]::Round($p.WorkingSet64 / 1MB, 1)
        $cpu = [math]::Round($p.CPU, 0)
        Write-Output ("{0,-28} {1,10} {2,12}" -f $p.ProcessName, $mb, $cpu)
    }
} else {
    Write-Output "暂无数据。"
}
