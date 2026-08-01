//! 希尔娅 Windows 系统托盘应用
//!
//! - 右键菜单：打开面板 / 迷你对话 / 打开配置 / 立即备份 / 打开主目录 /
//!   开机自启 / 开发者信息 / 退出
//! - 全局快捷键：Alt+G 迷你对话，Alt+H 面板
//! - 后台确保 `hilia web --no-open` 服务在跑（127.0.0.1:4096），面板用浏览器打开
//! - 单实例：重复启动时只打开面板
//!
//! 只支持 Windows（由 GitHub Actions windows-latest 构建）。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager,
};
use std::process::{Child, Command};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};

const WEB_PORT: u16 = 4096;
const PANEL_URL: &str = "http://127.0.0.1:4096";
const MINI_URL: &str = "http://127.0.0.1:4096/mini";
const DEV_CONTACT: &str = "2101497063@qq.com";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const AUTOSTART_VALUE: &str = "HiliaTray";

enum TrayEvent {
    Menu(tray_icon::menu::MenuId),
    Hotkey,
}

struct TrayApp {
    menu: Menu,
    menu_open_panel: MenuItem,
    menu_mini: MenuItem,
    menu_config: MenuItem,
    menu_backup: MenuItem,
    menu_check_update: MenuItem,
    menu_home: MenuItem,
    menu_autostart: CheckMenuItem,
    menu_quit: MenuItem,
    autostart_checked: bool,
    web_child: Option<Child>,
    _tray: Option<TrayIcon>,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn hilia_exe() -> std::path::PathBuf {
    // 1. 与托盘同目录的 hilia.exe（随包分发）
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("hilia.exe");
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    // 2. 标准安装目录
    let standard = std::path::PathBuf::from(r"C:\hilia\hilia.exe");
    if standard.is_file() {
        return standard;
    }
    // 3. PATH 查找
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("hilia.exe");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    std::path::PathBuf::from("hilia.exe")
}

fn web_health_ok() -> bool {
    let Ok(output) = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "try {{ (Invoke-WebRequest -UseBasicParsing -Uri 'http://127.0.0.1:{WEB_PORT}/api/health' -TimeoutSec 2).StatusCode -eq 200 }} catch {{ $false }}"
            ),
        ])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout).trim() == "True"
}

/// 确保面板服务在跑：health 不通则启动 `hilia web --no-open`（随包同目录）。
fn ensure_web_server(app: &mut TrayApp) {
    if web_health_ok() || app.web_child.is_some() {
        return;
    }
    match Command::new(hilia_exe()).args(["web", "--no-open"]).spawn() {
        Ok(child) => app.web_child = Some(child),
        Err(error) => {
            let _ = Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    &format!(
                        "Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.MessageBox]::Show('启动面板服务失败：{error}', '希尔娅')"
                    ),
                ])
                .spawn();
        }
    }
}

fn open_url(url: &str) {
    let _ = Command::new("cmd").args(["/C", "start", "", url]).spawn();
}

fn open_panel() {
    open_url(PANEL_URL);
}

impl TrayApp {
    fn new() -> Self {
        let menu = Menu::new();
        let title = MenuItem::new("希尔娅 v0.6.0", false, None);
        let dev = MenuItem::new(format!("开发者：{DEV_CONTACT}"), false, None);
        let open_panel = MenuItem::new("打开面板", true, None);
        let mini = MenuItem::new("迷你对话 Alt+G", true, None);
        let config = MenuItem::new("打开配置", true, None);
        let backup = MenuItem::new("立即备份", true, None);
        let check_update = MenuItem::new("检查更新", true, None);
        let home = MenuItem::new("打开主目录", true, None);
        let autostart = CheckMenuItem::new("开机自启", true, is_autostart_enabled(), None);
        let quit = MenuItem::new("退出希尔娅", true, None);

        menu.append_items(&[
            &title,
            &dev,
            &PredefinedMenuItem::separator(),
            &open_panel,
            &mini,
            &config,
            &backup,
            &check_update,
            &home,
            &PredefinedMenuItem::separator(),
            &autostart,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .expect("build tray menu");

        let autostart_checked = is_autostart_enabled();
        let mut app = Self {
            menu,
            menu_open_panel: open_panel,
            menu_mini: mini,
            menu_config: config,
            menu_backup: backup,
            menu_check_update: check_update,
            menu_home: home,
            menu_autostart: autostart,
            menu_quit: quit,
            autostart_checked,
            web_child: None,
            _tray: None,
        };
        app._tray = app.build_tray();
        app
    }

    fn build_tray(&self) -> Option<TrayIcon> {
        let icon = Icon::from_rgba(build_icon_rgba(), 32, 32).ok()?;
        TrayIconBuilder::new()
            .with_tooltip("希尔娅 —— 班级学分管理 AI 助理")
            .with_icon(icon)
            .with_menu(Box::new(self.menu.clone()))
            .build()
            .ok()
    }

    fn on_event(&mut self, event: TrayEvent) {
        match event {
            TrayEvent::Menu(id) => {
                let key = id.0.as_str();
                if key == self.menu_open_panel.id().0.as_str() {
                    ensure_web_server(self);
                    open_panel();
                } else if key == self.menu_mini.id().0.as_str() {
                    ensure_web_server(self);
                    open_url(MINI_URL);
                } else if key == self.menu_config.id().0.as_str() {
                    open_config_file();
                } else if key == self.menu_backup.id().0.as_str() {
                    let _ = Command::new(hilia_exe()).args(["backup", "now"]).spawn();
                } else if key == self.menu_check_update.id().0.as_str() {
                    check_for_update();
                } else if key == self.menu_home.id().0.as_str() {
                    open_home_dir();
                } else if key == self.menu_autostart.id().0.as_str() {
                    self.autostart_checked = !self.autostart_checked;
                    set_autostart(self.autostart_checked);
                    let _ = self.menu_autostart.set_checked(self.autostart_checked);
                } else if key == self.menu_quit.id().0.as_str() {
                    std::process::exit(0);
                }
            }
            TrayEvent::Hotkey => {
                ensure_web_server(self);
                open_panel();
            }
        }
    }
}

fn build_icon_rgba() -> Vec<u8> {
    // 32x32 简单图标：蓝紫色圆（生成式，避免额外资源文件）
    let size = 32;
    let mut rgba = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let index = (y * size + x) * 4;
            let dx = x as f32 - 15.5;
            let dy = y as f32 - 15.5;
            let distance = (dx * dx + dy * dy).sqrt();
            if distance <= 14.0 {
                rgba[index] = 106;
                rgba[index + 1] = 168;
                rgba[index + 2] = 254;
                rgba[index + 3] = 255;
            }
        }
    }
    rgba
}

fn data_home() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Users\Public"))
        .join("hilia")
}

/// 检查更新：运行 `hilia update check` 并把结果用消息框展示。
fn check_for_update() {
    let exe = hilia_exe();
    let ps = format!(
        "Add-Type -AssemblyName System.Windows.Forms;          $out = & '{exe}' update check 2>&1 | Out-String;          [System.Windows.Forms.MessageBox]::Show($out, '希尔娅 更新检查')"
    );
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
        .spawn();
}

fn open_config_file() {
    let config = data_home().join("config").join("config.jsonc");
    let _ = Command::new("cmd")
        .args(["/C", "start", "", config.to_str().unwrap_or_default()])
        .spawn();
}

fn open_home_dir() {
    let _ = Command::new("cmd")
        .args(["/C", "start", "", data_home().to_str().unwrap_or_default()])
        .spawn();
}

// ─────────────────────────── 开机自启（注册表 Run） ───────────────────────────

fn is_autostart_enabled() -> bool {
    unsafe {
        use windows_sys::Win32::System::Registry::*;
        let mut key = std::ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        ) != 0
        {
            return false;
        }
        let mut buffer = [0u16; 1024];
        let mut size = (buffer.len() * 2) as u32;
        let result = RegQueryValueExW(
            key,
            wide(AUTOSTART_VALUE).as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            buffer.as_mut_ptr() as *mut u8,
            &mut size,
        );
        RegCloseKey(key);
        result == 0
    }
}

fn set_autostart(enabled: bool) {
    unsafe {
        use windows_sys::Win32::System::Registry::*;
        let mut key = std::ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            0,
            KEY_SET_VALUE | KEY_QUERY_VALUE,
            &mut key,
        ) != 0
        {
            return;
        }
        if enabled {
            let exe = std::env::current_exe()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let value = wide(&format!("\"{exe}\""));
            RegSetValueExW(
                key,
                wide(AUTOSTART_VALUE).as_ptr(),
                0,
                REG_SZ,
                value.as_ptr() as *const u8,
                (value.len() * 2) as u32,
            );
        } else {
            RegDeleteValueW(key, wide(AUTOSTART_VALUE).as_ptr());
        }
        RegCloseKey(key);
    }
}

// ─────────────────────────── 单实例 ───────────────────────────

fn try_lock_single_instance() -> bool {
    unsafe {
        use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
        use windows_sys::Win32::System::Threading::CreateMutexW;
        let mutex = CreateMutexW(std::ptr::null(), false, wide("Local\\hilia-tray-mutex").as_ptr());
        if mutex.is_null() {
            return true;
        }
        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

// ─────────────────────────── 入口 ───────────────────────────

impl ApplicationHandler<TrayEvent> for TrayApp {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: TrayEvent) {
        self.on_event(event);
    }
}

fn main() {
    if !try_lock_single_instance() {
        // 已有实例在跑：直接打开面板并退出
        open_panel();
        std::process::exit(0);
    }

    let event_loop = EventLoop::<TrayEvent>::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();

    // 托盘菜单事件 → 事件循环
    MenuEvent::set_event_handler(Some(Box::new(move |event| {
        let _ = proxy.send_event(TrayEvent::Menu(event.id.clone()));
    })));

    // 全局快捷键 Alt+G（迷你）/ Alt+H（面板）
    let hotkey_manager = GlobalHotKeyManager::new().expect("create hotkey manager");
    let _ = hotkey_manager.register(HotKey::new(Some(Modifiers::ALT), Code::KeyH).expect("alt+h"));
    let _ = hotkey_manager.register(HotKey::new(Some(Modifiers::ALT), Code::KeyG).expect("alt+g"));
    let hotkey_proxy = event_loop.create_proxy();
    GlobalHotKeyEvent::set_event_handler(Some(Box::new(move |_event| {
        let _ = hotkey_proxy.send_event(TrayEvent::Hotkey);
    })));

    let mut app = TrayApp::new();
    event_loop.run_app(&mut app).expect("run tray event loop");
}
