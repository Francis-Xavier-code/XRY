; 希尔娅 Hilia —— Windows 统一安装器（主体 hilia.exe + 菜单栏 hilia-tray.exe）
; 构建：makensis /DSTAGE_DIR=<打包目录> /DAPP_VERSION=<版本> windows/installer.nsi
; 注意：用户数据在 %LOCALAPPDATA%\hilia，卸载时保留

Unicode true

!ifndef APP_VERSION
!define APP_VERSION "0.6.0"
!endif
!ifndef STAGE_DIR
!define STAGE_DIR "stage"
!endif

!define APP_NAME "希尔娅 Hilia"
!define APP_BRAND "希尔娅"
!define PUBLISHER "2101497063@qq.com"
!define INSTALL_DIR "Hilia"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Hilia"

Name "${APP_NAME}"
OutFile "Hilia-Setup-${APP_VERSION}.exe"
InstallDir "$PROGRAMFILES64\${INSTALL_DIR}"
InstallDirRegKey HKLM "${UNINST_KEY}" "InstallLocation"
RequestExecutionLevel admin
SetCompressor /SOLID lzma

; 页面
Page directory
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

; 安装：主体 + 菜单栏（托盘）+ 资源 + 快捷方式 + 卸载信息
Section "希尔娅（主体 + 菜单栏）" SEC_MAIN
  SetOutPath "$INSTDIR"
  File /r "${STAGE_DIR}\*"

  ; 开始菜单
  CreateDirectory "$SMPROGRAMS\${APP_BRAND}"
  CreateShortcut "$SMPROGRAMS\${APP_BRAND}\${APP_BRAND}托盘.lnk" "$INSTDIR\hilia-tray.exe" "" "$INSTDIR\hilia-tray.exe"
  CreateShortcut "$SMPROGRAMS\${APP_BRAND}\${APP_BRAND}终端.lnk" "$INSTDIR\hilia.exe" "" "$INSTDIR\hilia.exe"
  CreateShortcut "$SMPROGRAMS\${APP_BRAND}\卸载${APP_BRAND}.lnk" "$INSTDIR\uninstall.exe" "" "$INSTDIR\uninstall.exe"

  ; 桌面快捷方式（托盘）
  CreateShortcut "$DESKTOP\${APP_BRAND}.lnk" "$INSTDIR\hilia-tray.exe" "" "$INSTDIR\hilia-tray.exe"

  ; 卸载信息
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKLM "${UNINST_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKLM "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKLM "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoRepair" 1
SectionEnd

; 可选：开机自启（默认勾选，启动托盘）
Section "开机启动希尔娅托盘" SEC_AUTOSTART
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "HiliaTray" '"$INSTDIR\hilia-tray.exe"'
SectionEnd

; 安装完成：询问是否立即启动
!include "LogicLib.nsh"
Function .onInstSuccess
  MessageBox MB_YESNO|MB_ICONQUESTION "希尔娅安装完成，是否立即启动菜单栏（托盘）？" IDNO noLaunch
    ExecShell "" "$INSTDIR\hilia-tray.exe"
  noLaunch:
FunctionEnd

; 卸载
Section "Uninstall"
  ; 停进程
  nsExec::Exec 'taskkill /IM hilia.exe /F'
  nsExec::Exec 'taskkill /IM hilia-tray.exe /F'

  ; 移除快捷方式与自启
  Delete "$SMPROGRAMS\${APP_BRAND}\${APP_BRAND}托盘.lnk"
  Delete "$SMPROGRAMS\${APP_BRAND}\${APP_BRAND}终端.lnk"
  Delete "$SMPROGRAMS\${APP_BRAND}\卸载${APP_BRAND}.lnk"
  RMDir "$SMPROGRAMS\${APP_BRAND}"
  Delete "$DESKTOP\${APP_BRAND}.lnk"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "HiliaTray"

  ; 卸载信息与程序文件（保留 %LOCALAPPDATA%\hilia 用户数据）
  DeleteRegKey HKLM "${UNINST_KEY}"
  RMDir /r "$INSTDIR"
SectionEnd
