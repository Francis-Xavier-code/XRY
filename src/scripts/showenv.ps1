#!powershell
# 显示名称：环境变量查看
# 描述：查看当前进程的环境变量与 PATH 路径，排查命令找不到 / 环境配置问题。Windows 版。
# Description: Show environment variables and PATH entries of the current process. Windows version.
# 参数（stdin JSON，可选）：name 只显示指定变量（支持模糊匹配）；path 为 true 时只显示 PATH

$ErrorActionPreference = "SilentlyContinue"

$inputJson = [Console]::In.ReadToEnd()
$args = @{}
if ($inputJson -and $inputJson.Trim() -ne "") {
    try { $args = $inputJson | ConvertFrom-Json } catch {}
}
$filter = ""
if ($args.name) { $filter = [string]$args.name }
$onlyPath = $false
if ($args.path) { $onlyPath = $true }

if ($onlyPath) {
    Write-Output "PATH 路径（每条一行）："
    $env:PATH -split ';' | ForEach-Object { if ($_.Trim() -ne "") { Write-Output $_ } }
    exit 0
}

$vars = [System.Environment]::GetEnvironmentVariables()
$keys = $vars.Keys | Sort-Object

if ($filter -ne "") {
    $keys = $keys | Where-Object { $_ -like "*$filter*" }
    Write-Output "匹配「$filter」的环境变量："
}

foreach ($key in $keys) {
    $value = [string]$vars[$key]
    if ($value.Length -gt 300) { $value = $value.Substring(0, 300) + "…" }
    if ($key -eq "PATH") {
        Write-Output "$key ="
        $value -split ';' | ForEach-Object { if ($_.Trim() -ne "") { Write-Output "    $_" } }
    } else {
        Write-Output "$key = $value"
    }
}

if ($filter -ne "" -and $keys.Count -eq 0) {
    Write-Output "（无匹配项）"
}
