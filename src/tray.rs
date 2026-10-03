use crate::{
    app::{Action, SharedState},
    wide,
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
const STARTUP_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
static WINDOW: AtomicIsize = AtomicIsize::new(0);
static FINISHED: AtomicBool = AtomicBool::new(false);
struct UI {
    shared: SharedState,
    tx: Sender<Action>,
    folder: PathBuf,
    icon: HICON,
    taskbar_message: u32,
    last_error: String,
}
thread_local! {static UI_STATE:RefCell<Option<UI>>=const{RefCell::new(None)};}
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
        "\"{}\" --data-dir \"{}\"",
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
                "Disable detection".into(),
                if state.disabled { MF_CHECKED } else { 0 },
            ),
            (4, "Refresh installed games".into(), 0),
            (5, "Open configuration".into(), 0),
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
            5 => {
                open_path(&ui.folder.join("config.json"));
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
            UI_STATE.with(|state| {
                if let Some(ui) = state.borrow_mut().as_mut() {
                    unsafe {
                        notification(hwnd, NIM_MODIFY, ui);
                    }
                    let text = ui
                        .shared
                        .lock()
                        .map(|s| s.message.clone())
                        .unwrap_or_default();
                    if text.starts_with("Needs attention:") && text != ui.last_error {
                        unsafe {
                            let mut data: NOTIFYICONDATAW = std::mem::zeroed();
                            data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                            data.hWnd = hwnd;
                            data.uID = 1;
                            data.uFlags = NIF_INFO;
                            data.dwInfoFlags = NIIF_WARNING;
                            let title: Vec<_> =
                                "GamePause needs attention".encode_utf16().collect();
                            data.szInfoTitle[..title.len()].copy_from_slice(&title);
                            let info: Vec<_> = text.encode_utf16().take(255).collect();
                            data.szInfo[..info.len()].copy_from_slice(&info);
                            Shell_NotifyIconW(NIM_MODIFY, &data);
                        }
                        ui.last_error = text;
                    } else if !text.starts_with("Needs attention:")
                        && !ui.shared.lock().map(|s| s.pending).unwrap_or(true)
                    {
                        ui.last_error.clear();
                    }
                }
            });
            0
        }
        CALLBACK => {
            if l as u32 == WM_RBUTTONUP || l as u32 == WM_LBUTTONUP {
                UI_STATE.with(|state| {
                    if let Some(ui) = state.borrow().as_ref() {
                        unsafe {
                            menu(hwnd, ui);
                        }
                    }
                });
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
            UI_STATE.with(|state| {
                if let Some(ui) = state.borrow().as_ref() {
                    unsafe {
                        notification(hwnd, NIM_DELETE, ui);
                        DestroyIcon(ui.icon);
                    }
                }
            });
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
                UI_STATE.with(|state| {
                    if let Some(ui) = state.borrow().as_ref() {
                        unsafe {
                            notification(hwnd, NIM_ADD, ui);
                        }
                    }
                });
                0
            } else {
                unsafe { DefWindowProcW(hwnd, message, w, l) }
            }
        }
    }
}
pub fn run(shared: SharedState, tx: Sender<Action>, folder: PathBuf) -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(null());
        let class = wide("GamePauseTrayWindow");
        let taskbar = wide("TaskbarCreated");
        let ui = UI {
            shared,
            tx,
            folder,
            icon: icon(),
            taskbar_message: RegisterWindowMessageW(taskbar.as_ptr()),
            last_error: String::new(),
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
        UI_STATE.with(|state| notification(window, NIM_ADD, state.borrow().as_ref().unwrap()));
        SetTimer(window, 1, 2000, None);
        let mut msg: MSG = std::mem::zeroed();
        loop {
            let status = GetMessageW(&mut msg, null_mut(), 0, 0);
            if status == 0 {
                break;
            }
            if status == -1 {
                bail_message()?;
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
    fn startup_keeps_custom_folder() {
        let command = startup_command(
            Path::new(r"C:\Program Files\GamePause\GamePause.exe"),
            Path::new(r"D:\AI Data\GamePause"),
        );
        assert_eq!(
            command,
            r#""C:\Program Files\GamePause\GamePause.exe" --data-dir "D:\AI Data\GamePause""#
        );
    }
}
