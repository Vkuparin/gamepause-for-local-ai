use crate::{
    app::{Action, SharedState},
    dashboard, wide,
};
use anyhow::{Context, Result};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicBool, AtomicIsize, Ordering},
        mpsc::Sender,
    },
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    Graphics::Gdi::{CreateBitmap, DeleteObject},
    System::LibraryLoader::GetModuleHandleW,
    UI::{Shell::*, WindowsAndMessaging::*},
};
use winreg::{RegKey, enums::*};

const CALLBACK: u32 = WM_APP + 1;
const SHOW_DASHBOARD: u32 = WM_APP + 2;
const STARTUP_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
static WINDOW: AtomicIsize = AtomicIsize::new(0);
static FINISHED: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static MENU_TIMER_TICKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static MENU_OPENINGS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[derive(Clone)]
struct UI {
    shared: SharedState,
    tx: Sender<Action>,
    folder: PathBuf,
    icon: HICON,
    taskbar_message: u32,
    last_error: String,
    menu_open: bool,
}
thread_local! {static UI_STATE:RefCell<Option<UI>>=const{RefCell::new(None)};}

// Win32 menu tracking and shell calls can dispatch messages synchronously.
// Never retain a RefCell borrow across those calls: nested timers need this state.
fn ui_snapshot() -> Option<UI> {
    UI_STATE.with(|state| state.borrow().clone())
}
struct MenuSession {
    ui: UI,
}
impl Drop for MenuSession {
    fn drop(&mut self) {
        UI_STATE.with(|state| {
            if let Some(ui) = state.borrow_mut().as_mut() {
                ui.menu_open = false;
            }
        });
    }
}
fn begin_menu() -> Option<MenuSession> {
    UI_STATE.with(|state| {
        let mut state = state.borrow_mut();
        let ui = state.as_mut()?;
        if ui.menu_open {
            return None;
        }
        ui.menu_open = true;
        #[cfg(test)]
        MENU_OPENINGS.fetch_add(1, Ordering::Relaxed);
        Some(MenuSession { ui: ui.clone() })
    })
}
fn window_class(folder: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    folder.to_string_lossy().to_lowercase().hash(&mut hasher);
    format!("GamePauseTrayWindow-{:x}", hasher.finish())
}
pub fn show_existing(folder: &Path) -> bool {
    unsafe {
        let window = FindWindowW(wide(&window_class(folder)).as_ptr(), null());
        if window.is_null() {
            return false;
        }
        PostMessageW(window, SHOW_DASHBOARD, 0, 0) != 0
    }
}
pub fn request_exit() {
    FINISHED.store(true, Ordering::Relaxed);
    let window = WINDOW.load(Ordering::Relaxed);
    if window != 0 {
        unsafe {
            PostMessageW(window as HWND, WM_CLOSE, 0, 0);
        }
    }
}
pub fn error(message: &str) {
    let text = wide(message);
    let title = wide("GamePause needs attention");
    unsafe {
        MessageBoxW(
            null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
pub fn startup_enabled() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(STARTUP_KEY)
        .and_then(|k| k.get_value::<String, _>("GamePause"))
        .is_ok()
}
pub fn startup_command(executable: &Path, folder: &Path) -> String {
    format!(
        "\"{}\" --background --data-dir \"{}\"",
        executable.display(),
        folder.display()
    )
}
pub fn set_startup(enabled: bool, folder: &Path) -> Result<()> {
    let (root, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(STARTUP_KEY)?;
    if enabled {
        root.set_value(
            "GamePause",
            &startup_command(&std::env::current_exe()?, folder),
        )?;
    } else {
        match root.delete_value("GamePause") {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
pub fn open_path(path: &Path) {
    let operation = wide("open");
    let path = wide(&path.to_string_lossy());
    unsafe {
        ShellExecuteW(
            null_mut(),
            operation.as_ptr(),
            path.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        );
    }
}
unsafe fn icon() -> HICON {
    let mut pixels = vec![0u8; 32 * 32 * 4];
    for y in 0..32 {
        for x in 0..32 {
            let offset = (y * 32 + x) * 4;
            let cyan = (4..28).contains(&x) && (4..28).contains(&y);
            if cyan {
                pixels[offset..offset + 4].copy_from_slice(&[117, 94, 21, 255]);
            }
            if (9..23).contains(&y) && ((10..14).contains(&x) || (18..22).contains(&x)) {
                pixels[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    unsafe {
        let color = CreateBitmap(32, 32, 1, 32, pixels.as_ptr().cast());
        let mask = CreateBitmap(32, 32, 1, 1, [0u8; 128].as_ptr().cast());
        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let result = CreateIconIndirect(&info);
        DeleteObject(color);
        DeleteObject(mask);
        result
    }
}
unsafe fn notification(hwnd: HWND, operation: u32, ui: &UI) {
    unsafe {
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = 1;
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = CALLBACK;
        data.hIcon = ui.icon;
        let message = ui
            .shared
            .lock()
            .map(|s| s.message.clone())
            .unwrap_or_else(|_| "GamePause".into());
        let text: Vec<_> = format!("GamePause: {message}")
            .encode_utf16()
            .take(127)
            .collect();
        data.szTip[..text.len()].copy_from_slice(&text);
        Shell_NotifyIconW(operation, &data);
    }
}
unsafe fn menu(hwnd: HWND, ui: &UI) {
    unsafe {
        let menu = CreatePopupMenu();
        let state = ui.shared.lock().unwrap();
        let items = [
            (0, state.message.clone(), MF_GRAYED),
            (9, "Open GamePause".into(), 0),
            (
                1,
                "Pause AI manually".into(),
                if !state.active_mode {
                    MF_GRAYED
                } else if state.manual_pause {
                    MF_CHECKED
                } else {
                    0
                },
            ),
            (
                2,
                "Restore AI now".into(),
                if state.active_mode && state.pending {
                    0
                } else {
                    MF_GRAYED
                },
            ),
            (
                3,
                "Automatically pause AI while gaming".into(),
                if state.config.automation_enabled {
                    MF_CHECKED
                } else {
                    0
                },
            ),
            (4, "Refresh installed games".into(), 0),
            (5, "Settings and games".into(), 0),
            (6, "Open logs and status folder".into(), 0),
            (
                7,
                "Start with Windows".into(),
                if startup_enabled() { MF_CHECKED } else { 0 },
            ),
            (8, "Quit (keeps recovery state)".into(), 0),
        ];
        drop(state);
        for (id, label, flags) in items {
            let text = wide(&label);
            AppendMenuW(menu, MF_STRING | flags, id, text.as_ptr());
        }
        let mut point: POINT = std::mem::zeroed();
        GetCursorPos(&mut point);
        SetForegroundWindow(hwnd);
        let id = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            0,
            hwnd,
            null(),
        );
        DestroyMenu(menu);
        let action = match id {
            1 => Some(Action::Pause),
            2 => Some(Action::Restore),
            3 => Some(Action::Disable),
            4 => Some(Action::Refresh),
            8 => Some(Action::Quit),
            5 | 9 => {
                dashboard::show(ui.shared.clone(), ui.tx.clone(), ui.folder.clone());
                None
            }
            6 => {
                open_path(&ui.folder);
                None
            }
            7 => {
                if let Err(e) = set_startup(!startup_enabled(), &ui.folder) {
                    error(&format!("Startup setup failed: {e:#}"));
                }
                None
            }
            _ => None,
        };
        if let Some(action) = action {
            let _ = ui.tx.send(action);
        }
        PostMessageW(hwnd, WM_NULL, 0, 0);
    }
}
unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_TIMER => {
            if FINISHED.load(Ordering::Relaxed) {
                unsafe {
                    DestroyWindow(hwnd);
                }
                return 0;
            }
            if let Some(ui) = ui_snapshot() {
                #[cfg(test)]
                if ui.menu_open {
                    MENU_TIMER_TICKS.fetch_add(1, Ordering::Relaxed);
                }
                unsafe {
                    notification(hwnd, NIM_MODIFY, &ui);
                }
                let text = ui
                    .shared
                    .lock()
                    .map(|s| s.message.clone())
                    .unwrap_or_default();
                let pending = ui.shared.lock().map(|s| s.pending).unwrap_or(true);
                let show_error = UI_STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    let Some(current) = state.as_mut() else {
                        return false;
                    };
                    if text.starts_with("Needs attention:") && text != current.last_error {
                        current.last_error = text.clone();
                        true
                    } else {
                        if !text.starts_with("Needs attention:") && !pending {
                            current.last_error.clear();
                        }
                        false
                    }
                });
                if show_error {
                    unsafe {
                        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
                        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                        data.hWnd = hwnd;
                        data.uID = 1;
                        data.uFlags = NIF_INFO;
                        data.dwInfoFlags = NIIF_WARNING;
                        let title: Vec<_> = "GamePause needs attention".encode_utf16().collect();
                        data.szInfoTitle[..title.len()].copy_from_slice(&title);
                        let info: Vec<_> = text.encode_utf16().take(255).collect();
                        data.szInfo[..info.len()].copy_from_slice(&info);
                        Shell_NotifyIconW(NIM_MODIFY, &data);
                    }
                }
            }
            0
        }
        SHOW_DASHBOARD => {
            if let Some(ui) = ui_snapshot() {
                dashboard::show(ui.shared, ui.tx, ui.folder);
            }
            0
        }
        CALLBACK => {
            if l as u32 == WM_LBUTTONUP {
                if let Some(ui) = ui_snapshot() {
                    dashboard::show(ui.shared, ui.tx, ui.folder);
                }
                return 0;
            }
            if l as u32 == WM_RBUTTONUP
                && let Some(session) = begin_menu()
            {
                unsafe {
                    menu(hwnd, &session.ui);
                }
            }
            0
        }
        WM_CLOSE => {
            UI_STATE.with(|state| {
                if let Some(ui) = state.borrow().as_ref() {
                    let _ = ui.tx.send(Action::Quit);
                }
            });
            unsafe {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_DESTROY => {
            dashboard::close();
            if let Some(ui) = ui_snapshot() {
                unsafe {
                    notification(hwnd, NIM_DELETE, &ui);
                    DestroyIcon(ui.icon);
                }
            }
            WINDOW.store(0, Ordering::Relaxed);
            unsafe {
                PostQuitMessage(0);
            }
            0
        }
        _ => {
            let explorer = UI_STATE.with(|state| {
                state
                    .borrow()
                    .as_ref()
                    .is_some_and(|ui| message == ui.taskbar_message)
            });
            if explorer {
                if let Some(ui) = ui_snapshot() {
                    unsafe {
                        notification(hwnd, NIM_ADD, &ui);
                    }
                }
                0
            } else {
                unsafe { DefWindowProcW(hwnd, message, w, l) }
            }
        }
    }
}
pub fn run(shared: SharedState, tx: Sender<Action>, folder: PathBuf, show: bool) -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(null());
        let class = wide(&window_class(&folder));
        let taskbar = wide("TaskbarCreated");
        let ui = UI {
            shared,
            tx,
            folder,
            icon: icon(),
            taskbar_message: RegisterWindowMessageW(taskbar.as_ptr()),
            last_error: String::new(),
            menu_open: false,
        };
        UI_STATE.with(|state| *state.borrow_mut() = Some(ui));
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let window = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if window.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        WINDOW.store(window as isize, Ordering::Relaxed);
        if let Some(ui) = ui_snapshot() {
            notification(window, NIM_ADD, &ui);
        }
        SetTimer(window, 1, 2000, None);
        if show {
            PostMessageW(window, SHOW_DASHBOARD, 0, 0);
        }
        let mut msg: MSG = std::mem::zeroed();
        loop {
            let status = GetMessageW(&mut msg, null_mut(), 0, 0);
            if status == 0 {
                break;
            }
            if status == -1 {
                bail_message()?;
            }
            if dashboard::is_dialog_message(&msg) {
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        UI_STATE.with(|state| state.borrow_mut().take());
    }
    Ok(())
}
fn bail_message() -> Result<()> {
    Err(std::io::Error::last_os_error()).context("Windows message loop failed")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timer_can_reenter_while_menu_context_is_alive() {
        use crate::app::Shared;
        use std::sync::{Arc, Mutex, mpsc};
        let (tx, _) = mpsc::channel();
        let shared = Arc::new(Mutex::new(Shared {
            message: "Needs attention: simulated failure".into(),
            disabled: false,
            manual_pause: false,
            pending: true,
            active_mode: false,
            config: crate::config::Config::default(),
            games: vec![],
            active_games: vec![],
            running_apps: vec![],
            discovery_errors: Default::default(),
            settings_error: String::new(),
            revision: 0,
            discovery_ready: false,
        }));
        UI_STATE.with(|state| {
            *state.borrow_mut() = Some(UI {
                shared: shared.clone(),
                tx,
                folder: PathBuf::new(),
                icon: null_mut(),
                taskbar_message: WM_APP + 9,
                last_error: String::new(),
                menu_open: false,
            })
        });
        let session = begin_menu().expect("first menu opens");
        assert!(
            begin_menu().is_none(),
            "nested clicks cannot start another menu"
        );
        // TrackPopupMenu invokes this callback while its caller is still active.
        unsafe {
            window_proc(null_mut(), WM_TIMER, 1, 0);
        }
        assert_eq!(
            ui_snapshot().unwrap().last_error,
            "Needs attention: simulated failure"
        );
        shared.lock().unwrap().message = "AI available".into();
        shared.lock().unwrap().pending = false;
        unsafe {
            window_proc(null_mut(), WM_TIMER, 1, 0);
        }
        assert!(ui_snapshot().unwrap().last_error.is_empty());
        drop(session);
        assert!(begin_menu().is_some(), "menu opens again after dismissal");
        UI_STATE.with(|state| state.borrow_mut().take());
    }
    #[test]
    #[ignore = "requires an interactive Windows desktop; opens only the test application's menus"]
    fn native_popup_survives_repeated_timer_reentry() {
        use crate::app::Shared;
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::{Duration, Instant};
        FINISHED.store(false, Ordering::Relaxed);
        MENU_TIMER_TICKS.store(0, Ordering::Relaxed);
        MENU_OPENINGS.store(0, Ordering::Relaxed);
        let shared = Arc::new(Mutex::new(Shared {
            message: "Tray regression test (observation only)".into(),
            disabled: false,
            manual_pause: false,
            pending: false,
            active_mode: false,
            config: crate::config::Config::default(),
            games: vec![],
            active_games: vec![],
            running_apps: vec![],
            discovery_errors: Default::default(),
            settings_error: String::new(),
            revision: 0,
            discovery_ready: false,
        }));
        let (tx, _rx) = mpsc::channel();
        let driver = std::thread::spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while WINDOW.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let hwnd = WINDOW.load(Ordering::Relaxed) as HWND;
            assert!(!hwnd.is_null(), "test window did not initialize");
            let mut counts = Vec::new();
            for _ in 0..3 {
                let before = MENU_TIMER_TICKS.load(Ordering::Relaxed);
                unsafe {
                    PostMessageW(hwnd, CALLBACK, 0, WM_RBUTTONUP as LPARAM);
                }
                let deadline = Instant::now() + Duration::from_secs(10);
                while MENU_TIMER_TICKS.load(Ordering::Relaxed) < before + 2
                    && Instant::now() < deadline
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                counts.push(MENU_TIMER_TICKS.load(Ordering::Relaxed) - before);
                unsafe {
                    PostMessageW(hwnd, WM_CANCELMODE, 0, 0);
                }
                std::thread::sleep(Duration::from_millis(300));
            }
            request_exit();
            counts
        });
        run(shared, tx, PathBuf::from("."), false).unwrap();
        let counts = driver.join().unwrap();
        assert_eq!(
            MENU_OPENINGS.load(Ordering::Relaxed),
            3,
            "three distinct menus must open"
        );
        assert!(
            counts.iter().all(|ticks| *ticks >= 2),
            "each real popup must survive at least two timer callbacks: {counts:?}"
        );
        FINISHED.store(false, Ordering::Relaxed);
    }
    #[test]
    fn startup_keeps_custom_folder() {
        let command = startup_command(
            Path::new(r"C:\Program Files\GamePause\GamePause.exe"),
            Path::new(r"D:\AI Data\GamePause"),
        );
        assert_eq!(
            command,
            r#""C:\Program Files\GamePause\GamePause.exe" --background --data-dir "D:\AI Data\GamePause""#
        );
    }
}
