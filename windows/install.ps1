# 希尔娅 Windows 安装脚本
# 用法：解压发行包后，在包目录里以管理员身份运行：
#   powershell -ExecutionPolicy Bypass -File install.ps1
# 把 hilia.exe / hilia-tray.exe 安装到 C:\hilia，创建开始菜单快捷方式，可选开机自启。

$ErrorActionPreference = "Stop"

$Version = "0.6.0"
$InstallDir = "C:\hilia"
$StartMenuDir = Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs\希尔娅"

Write-Host "希尔娅 v$Version Windows 安装程序" -ForegroundColor Cyan
Write-Host "开发者：2101497063@qq.com" -ForegroundColor DarkGray
Write-Host ""

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$hiliaExe = Join-Path $here "hilia.exe"
$trayExe = Join-Path $here "hilia-tray.exe"

if (-not (Test-Path $hiliaExe)) {
    Write-Error "未找到 hilia.exe，请把 install.ps1 与 hilia.exe 放在同一目录。"
    exit 1
}

# 需要管理员权限写 C:\
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Warning "建议以管理员身份运行本脚本（将安装到 C:\hilia）。按任意键继续，或 Ctrl+C 取消..."
    Read-Host
}

Write-Host "[1/4] 复制程序文件到 $InstallDir ..."
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Copy-Item $hiliaExe $InstallDir -Force
if (Test-Path $trayExe) { Copy-Item $trayExe $InstallDir -Force }

# 随包资源（scripts / kb / memes / communication）
foreach ($dirName in @("scripts", "kb", "memes", "communication")) {
    $src = Join-Path $here $dirName
    if (Test-Path $src) {
        Copy-Item $src (Join-Path $InstallDir $dirName) -Recurse -Force
    }
}

Write-Host "[2/4] 创建开始菜单快捷方式 ..."
New-Item -ItemType Directory -Force -Path $StartMenuDir | Out-Null
$shell = New-Object -ComObject WScript.Shell
$lnkTray = $shell.CreateShortcut((Join-Path $StartMenuDir "希尔娅托盘.lnk"))
$lnkTray.TargetPath = (Join-Path $InstallDir "hilia-tray.exe")
$lnkTray.WorkingDirectory = $InstallDir
$lnkTray.Description = "希尔娅 —— 班级学分管理 AI 助理"
$lnkTray.Save()
$lnkCli = $shell.CreateShortcut((Join-Path $StartMenuDir "希尔娅终端.lnk"))
$lnkCli.TargetPath = (Join-Path $InstallDir "hilia.exe")
$lnkCli.WorkingDirectory = $InstallDir
$lnkCli.Description = "希尔娅 命令行（PowerShell 里直接输入 hilia 对话）"
$lnkCli.Save()

Write-Host "[3/4] 初始化配置与学分数据库 ..."
$env:HILIA_HOME = Join-Path $env:LOCALAPPDATA "hilia"
& (Join-Path $InstallDir "hilia.exe") init

Write-Host "[4/4] 完成！" -ForegroundColor Green
Write-Host ""
Write-Host "  启动托盘：$InstallDir\hilia-tray.exe（开始菜单 → 希尔娅托盘）"
Write-Host "  命令行：  PowerShell 里输入 hilia（或 hilia.exe 全路径）"
Write-Host "  面板：    托盘菜单 → 打开面板（浏览器打开 127.0.0.1:4096）"
Write-Host "  数据目录：$env:LOCALAPPDATA\hilia"
Write-Host "  开发者：  2101497063@qq.com"
Write-Host ""
$answer = Read-Host "是否现在启动托盘程序？(y/N)"
if ($answer -eq "y" -or $answer -eq "Y") {
    Start-Process (Join-Path $InstallDir "hilia-tray.exe")
}
