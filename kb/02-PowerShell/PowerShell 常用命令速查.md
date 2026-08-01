# PowerShell 常用命令速查

## 打开方式

- Win + X → 终端 / 终端(管理员)
- 开始菜单搜索 powershell；Shift + 右键文件夹 → 在终端中打开

## 文件与目录

```powershell
Get-ChildItem            # ls：列出文件
Set-Location C:\hilia    # cd：切换目录
New-Item -ItemType Directory -Path C:\test   # 建目录
Copy-Item a.txt b.txt    # 复制
Move-Item a.txt C:\test  # 移动
Remove-Item a.txt        # 删除（慎重）
Get-Content file.txt     # 查看文本文件
```

## 进程与系统

```powershell
Get-Process              # 进程列表（按内存排序：| Sort WorkingSet64 -Descending）
Stop-Process -Name notepad   # 结束进程
Get-CimInstance Win32_Battery # 电池信息
Get-CimInstance Win32_OperatingSystem  # 系统信息
```

## 网络

```powershell
Test-Connection baidu.com      # ping
Get-NetIPAddress               # 本机 IP
Invoke-WebRequest https://...  # 请求网页/接口
netstat -ano | findstr :4096   # 查端口占用
```

## 环境变量与 PATH

```powershell
$env:HILIA_HOME        # 读环境变量
[System.Environment]::SetEnvironmentVariable("HILIA_HOME", "C:\...", "User")  # 永久设置
$env:PATH -split ';'   # 查看 PATH 每一项
```

## 管理员权限

- 右键开始菜单 → 终端(管理员)
- 脚本提权：`Start-Process powershell -Verb RunAs`
- 当前是否管理员：`([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)`

## 执行策略

- 运行本地脚本被拦截时：`powershell -ExecutionPolicy Bypass -File script.ps1`
- 或当前会话 `Set-ExecutionPolicy -Scope Process Bypass`
