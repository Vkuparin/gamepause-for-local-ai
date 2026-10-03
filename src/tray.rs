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
    Graphics::{
        Dwm::*,
        Gdi::{CreateBitmap, DeleteObject},
    },
    System::Diagnostics::Debug::MessageBeep,
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
/// The three state-colored tray icons, precomputed once so the timer path
/// never calls GDI. `Idle` is the base glyph; `Paused` and `Attention` are
/// tinted variants (P1-6).
#[derive(Clone)]
struct Icons {
    idle: HICON,
    paused: HICON,
    attention: HICON,
}
impl Icons {
    fn for_kind(&self, kind: StateKind) -> HICON {
        match kind {
            StateKind::Idle => self.idle,
            StateKind::Paused => self.paused,
            StateKind::Attention => self.attention,
        }
    }
    unsafe fn destroy(&self) {
        unsafe {
            if !self.idle.is_null() {
                DestroyIcon(self.idle);
            }
            if !self.paused.is_null() {
                DestroyIcon(self.paused);
            }
            if !self.attention.is_null() {
                DestroyIcon(self.attention);
            }
        }
    }
}
#[derive(Clone)]
struct UI {
    shared: SharedState,
    tx: Sender<Action>,
    folder: PathBuf,
    icons: Icons,
    taskbar_message: u32,
    last_error: String,
    last_kind: StateKind,
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
/// Pure: classify a `ShellExecuteW` return value (an `HINSTANCE`,
/// `*mut c_void`). Per the Win32 docs a successful call returns a handle whose
/// value is greater than 32; `0` and the range `1..=32` are documented failure
/// codes (S_OK, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ...). This is the
/// testable core of "open_path surfaces a visible error on failure" (P1-7).
#[must_use]
pub fn shell_execute_failed(return_value: *mut core::ffi::c_void) -> bool {
    return_value as isize <= 32
}

/// Open a folder in the default file manager. A `ShellExecuteW` failure is
/// surfaced as a visible error instead of being silently swallowed (P1-7).
pub fn open_path(path: &Path) {
    let operation = wide("open");
    let display = path.to_string_lossy();
    let path_w = wide(&display);
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            operation.as_ptr(),
            path_w.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    if shell_execute_failed(result) {
        error(&format!("Could not open this folder: {display}"));
    }
}
/// The tray glyph's solid fill color per state. Pure + unit-testable: this is
/// the "state -> icon variant" mapping P1-6 wants asserted without Win32.
/// `Idle` is the app's brand color; `Paused` reads "standby"; `Attention` is
/// a warning red that pops against the dark tray.
pub fn icon_tint(kind: StateKind) -> [u8; 3] {
    match kind {
        StateKind::Idle => [117, 94, 21],
        StateKind::Paused => [56, 132, 255],
        StateKind::Attention => [230, 62, 62],
    }
}
/// Read the OS app-color preference. `true` = dark mode. The key/value is the
/// documented `HKCU\...\Themes\Personalize\ColorsUseLightTheme` DWORD; a missing
/// value or read error is treated as "light" (the Windows default) so the app
/// never mis-colors a window it can't read.
pub fn system_is_dark() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_value::<u32, _>("ColorsUseLightTheme"))
        .map(|v| v == 0)
        .unwrap_or(false)
}
/// Apply the OS theme to `hwnd`: a dark caption bar when the system is in dark
/// mode, a light one otherwise. The DWM attribute is set per-window and is the
/// only way to theme a non-DWM window's caption without a full visual-styles
/// revamp. No-op when DWM is unavailable (older OS / accessibility).
///
/// # Safety
/// `hwnd` must be a valid `HWND` obtained from a successful window-creation
/// call. Passing `null()` is a documented no-op (DWM ignores it) but the
/// caller is responsible for not leaking an unowned handle.
pub unsafe fn apply_theme(hwnd: HWND) {
    unsafe {
        let use_dark: u32 = if system_is_dark() { 1 } else { 0 };
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            &use_dark as *const u32 as *const core::ffi::c_void,
            size_of::<u32>() as u32,
        );
    }
}
/// Draw the pause-bar glyph in `color` and return an HICON. The bars stay
/// white so the state is carried by the background tint alone.
unsafe fn icon(color: [u8; 3]) -> HICON {
    let mut pixels = vec![0u8; 32 * 32 * 4];
    for y in 0..32 {
        for x in 0..32 {
            let offset = (y * 32 + x) * 4;
            if (4..28).contains(&x) && (4..28).contains(&y) {
                pixels[offset..offset + 4].copy_from_slice(&[color[0], color[1], color[2], 255]);
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
        let (message, manual_pause) = ui
            .shared
            .lock()
            .map(|s| (s.message.clone(), s.manual_pause))
            .unwrap_or_else(|_| ("GamePause".into(), false));
        let kind = state_kind(&message, manual_pause);
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = 1;
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = CALLBACK;
        data.hIcon = ui.icons.for_kind(kind);
        let text: Vec<_> = format!("GamePause: {message}")
            .encode_utf16()
            .take(127)
            .collect();
        data.szTip[..text.len()].copy_from_slice(&text);
        Shell_NotifyIconW(operation, &data);
    }
}
/// The engine's user-facing state, classified from the current status message
/// plus the manual-pause flag. Single source of truth for the toast (P1-5) and
/// the state-colored tray icon (P1-6).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StateKind {
    /// Healthy / idle — the base icon. A successful restore lands here ("AI restored").
    Idle,
    /// AI is paused for gaming (automatic) or manually paused.
    Paused,
    /// Something needs human attention (a pause/restore failure, or repeated
    /// transient errors).
    Attention,
}
pub fn state_kind(message: &str, manual_pause: bool) -> StateKind {
    // Attention wins: a restore failure or repeated transient error must be
    // surfaced even if a pause message is also present.
    if message.starts_with("Restore failed") || message.starts_with("Needs attention") {
        return StateKind::Attention;
    }
    if message.starts_with("AI paused") || manual_pause {
        return StateKind::Paused;
    }
    StateKind::Idle
}
/// Toast severity. Pure data so the mapping is unit-testable (P1-5 acceptance).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    /// Pause-success or restore-success — the "good" system sound.
    Success,
    /// Any failure — the warning system sound.
    Error,
}
/// The toast title + severity for a given state (the "mapping table" the plan
/// wants asserted). The body is always the engine's own message, so the user
/// sees exactly what the engine said.
pub struct ToastSpec {
    pub title: &'static str,
    pub severity: Severity,
}
pub fn toast_spec(kind: StateKind) -> ToastSpec {
    match kind {
        StateKind::Attention => ToastSpec {
            title: "GamePause needs attention",
            severity: Severity::Error,
        },
        StateKind::Paused => ToastSpec {
            title: "GamePause",
            severity: Severity::Success,
        },
        StateKind::Idle => ToastSpec {
            title: "GamePause",
            severity: Severity::Success,
        },
    }
}
/// The system sound for a state. The "tiny indirection" the plan asks for:
/// tests assert this mapping instead of calling a live `MessageBeep`.
pub fn beep_code(kind: StateKind) -> u32 {
    match kind {
        StateKind::Attention => MB_ICONASTERISK,
        // Pause-success and restore-success are both "good" → the OK sound.
        StateKind::Idle | StateKind::Paused => MB_OK,
    }
}
unsafe fn state_toast(hwnd: HWND, message: &str, kind: StateKind) {
    unsafe {
        let spec = toast_spec(kind);
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = 1;
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = if spec.severity == Severity::Error {
            NIIF_ERROR
        } else {
            NIIF_INFO
        };
        let title: Vec<_> = spec.title.encode_utf16().collect();
        data.szInfoTitle[..title.len()].copy_from_slice(&title);
        let info: Vec<_> = message.encode_utf16().take(255).collect();
        data.szInfo[..info.len()].copy_from_slice(&info);
        Shell_NotifyIconW(NIM_MODIFY, &data);
        MessageBeep(beep_code(kind));
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
        // P1-7: gate "Restore AI now" on discovery readiness (mirror the
        // dashboard) and surface feedback instead of the worker no-opping
        // silently. The worker keeps its own gate as the safety net.
        let action = match id {
            1 => Some(Action::Pause),
            2 => {
                let (enabled, reason) = {
                    let s = ui.shared.lock().unwrap();
                    crate::dashboard::restore_gate(s.discovery_ready, s.active_mode, s.pending)
                };
                if enabled {
                    Some(Action::Restore)
                } else {
                    error(reason);
                    None
                }
            }
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
                let manual_pause = ui.shared.lock().map(|s| s.manual_pause).unwrap_or(false);
                // Keep the tray tooltip's "current attention" line in sync with
                // the state. This display bookkeeping is asserted by
                // `timer_can_reenter_while_menu_context_is_alive`: set the line
                // when a "Needs attention" message changes, clear it once a
                // healthy message arrives with no pending work, otherwise leave
                // it untouched.
                UI_STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    let Some(current) = state.as_mut() else {
                        return;
                    };
                    if text.starts_with("Needs attention:") && text != current.last_error {
                        current.last_error = text.clone();
                    } else if !text.starts_with("Needs attention:") && !pending {
                        current.last_error.clear();
                    }
                });
                // Edge-triggered toast + system sound on a state *transition*
                // (pause-success, restore-success, restore/pause-failure), not on
                // every tick — a held state toasts exactly once. The mapping and
                // the `MessageBeep` code live in `state_toast` so the table is
                // unit-testable (P1-5).
                let kind = state_kind(&text, manual_pause);
                UI_STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    let Some(current) = state.as_mut() else {
                        return;
                    };
                    if kind != current.last_kind {
                        current.last_kind = kind;
                        unsafe {
                            state_toast(hwnd, &text, kind);
                        }
                    }
                });
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
                    ui.icons.destroy();
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
            icons: Icons {
                idle: icon(icon_tint(StateKind::Idle)),
                paused: icon(icon_tint(StateKind::Paused)),
                attention: icon(icon_tint(StateKind::Attention)),
            },
            taskbar_message: RegisterWindowMessageW(taskbar.as_ptr()),
            last_error: String::new(),
            last_kind: StateKind::Idle,
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
        // DWM attribute is per-window; safe to set immediately after creation.
        apply_theme(window);
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
    fn state_kind_maps_engine_messages_to_states() {
        // Pause-success moment.
        assert_eq!(state_kind("AI paused for gaming", false), StateKind::Paused);
        assert_eq!(state_kind("AI paused for gaming", true), StateKind::Paused);
        // Restore-success moment: a successful restore lands idle.
        assert_eq!(state_kind("AI restored", false), StateKind::Idle);
        assert_eq!(state_kind("AI available", false), StateKind::Idle);
        // Restore-failure and pause-failure both need attention.
        assert_eq!(
            state_kind("Restore failed — AI not restored: embed", false),
            StateKind::Attention
        );
        assert_eq!(
            state_kind("Needs attention: embed", false),
            StateKind::Attention
        );
        // A failure beats a concurrent pause flag: attention wins.
        assert_eq!(
            state_kind("Needs attention: embed", true),
            StateKind::Attention
        );
        // Manual pause with no failure message.
        assert_eq!(state_kind("AI available", true), StateKind::Paused);
    }
    #[test]
    fn toast_spec_returns_right_title_and_severity_per_state() {
        // Pause-success and restore-success → the "good" sound, plain title.
        let paused = toast_spec(StateKind::Paused);
        assert_eq!(paused.title, "GamePause");
        assert_eq!(paused.severity, Severity::Success);
        let idle = toast_spec(StateKind::Idle);
        assert_eq!(idle.title, "GamePause");
        assert_eq!(idle.severity, Severity::Success);
        // Restore/pause-failure → error style, attention title.
        let attention = toast_spec(StateKind::Attention);
        assert_eq!(attention.title, "GamePause needs attention");
        assert_eq!(attention.severity, Severity::Error);
    }
    #[test]
    fn beep_code_maps_success_to_ok_and_failure_to_warning() {
        assert_eq!(beep_code(StateKind::Idle), MB_OK);
        assert_eq!(beep_code(StateKind::Paused), MB_OK);
        assert_eq!(beep_code(StateKind::Attention), MB_ICONASTERISK);
    }
    #[test]
    fn system_is_dark_reads_a_consistent_binary_preference() {
        // The registry is the single source of truth; two reads agree.
        let a = super::system_is_dark();
        let b = super::system_is_dark();
        assert_eq!(a, b, "two consecutive reads of the OS theme must agree");
    }
    #[test]
    fn apply_theme_does_not_panic_on_null_hwnd() {
        // DwmSetWindowAttribute on a null HWND is a documented no-op; the
        // production call site is the same code path, so it must not panic.
        unsafe { super::apply_theme(std::ptr::null_mut()) };
    }
    #[test]
    fn icon_tint_maps_state_to_distinct_colors() {
        // The three states must be visually distinct (P1-6 acceptance: the
        // state→icon mapping is asserted without Win32).
        let idle = icon_tint(StateKind::Idle);
        let paused = icon_tint(StateKind::Paused);
        let attention = icon_tint(StateKind::Attention);
        assert_ne!(idle, paused, "idle and paused icons must differ");
        assert_ne!(idle, attention, "idle and attention icons must differ");
        assert_ne!(paused, attention, "paused and attention icons must differ");
        // Each tint is a 3-byte RGB; the attention color is a warning red
        // (high R, low G/B) so it pops against the dark tray.
        assert!(attention[0] > 180 && attention[1] < 120 && attention[2] < 120);
        // Paused reads as a calm blue (B dominant).
        assert!(paused[2] > paused[0] && paused[2] > paused[1]);
    }
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
                icons: Icons {
                    idle: null_mut(),
                    paused: null_mut(),
                    attention: null_mut(),
                },
                taskbar_message: WM_APP + 9,
                last_error: String::new(),
                last_kind: StateKind::Idle,
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

    #[test]
    fn open_path_failure_is_classified_as_a_visible_error() {
        // ShellExecuteW returns an HINSTANCE > 32 on success; 0 and 1..=32 are
        // documented failure codes. The classifier is the testable core of
        // "open_path surfaces a visible error on failure" (P1-7).
        let v = |n: isize| n as *mut core::ffi::c_void;
        assert!(
            super::shell_execute_failed(v(0)),
            "0 is the generic failure code"
        );
        assert!(
            super::shell_execute_failed(v(2)),
            "ERROR_FILE_NOT_FOUND (2)"
        );
        assert!(
            super::shell_execute_failed(v(3)),
            "ERROR_PATH_NOT_FOUND (3)"
        );
        assert!(
            super::shell_execute_failed(v(32)),
            "32 is still in the failure range"
        );
        assert!(
            !super::shell_execute_failed(v(33)),
            "33 is the first success value"
        );
        assert!(
            !super::shell_execute_failed(v(0x0040_0000)),
            "a real HINSTANCE (a pointer-sized handle) is a success"
        );
    }
}
