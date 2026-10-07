//! The notification-area icon, its menu and timer. The window callback, the
//! menu-session lifetime and the timer order stay together in this file; the
//! children hold the icon resources, notification delivery and shell commands.

mod icons;
mod notifications;
mod shell;

pub use icons::icon_tint;
pub use notifications::{Severity, ToastSpec, beep_code, toast_spec};
pub use shell::{
    open_path, request_folder, request_startup, set_startup, shell_execute_failed, startup_command,
    startup_enabled,
};

use crate::{
    app::{Action, SharedState},
    control::{Activity, CoreCommand},
    dashboard,
    ui_commands::Command,
    wide,
};
use anyhow::{Context, Result};
use icons::{Icons, icon};
use notifications::{NativeNotifications, notification_flags};
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
    Graphics::Dwm::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Input::KeyboardAndMouse::{MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey},
        Shell::*,
        WindowsAndMessaging::*,
    },
};
use winreg::{RegKey, enums::*};

const CALLBACK: u32 = WM_APP + 1;
const HOTKEY_ID: i32 = 1;
thread_local! {
    /// Shortcut text last applied to the tray window, registered or refused.
    static HOTKEY: RefCell<String> = const { RefCell::new(String::new()) };
}
/// Keep the registered system shortcut in step with saved settings. Runs on
/// the tray UI thread; no borrow or lock spans the Win32 calls.
fn sync_hotkey(hwnd: HWND, shared: &crate::app::SharedState) {
    let wanted = shared
        .lock()
        .map(|s| s.config.pause_hotkey.clone())
        .unwrap_or_default();
    if HOTKEY.with(|current| *current.borrow() == wanted) {
        return;
    }
    HOTKEY.with(|current| current.borrow_mut().clone_from(&wanted));
    unsafe {
        UnregisterHotKey(hwnd, HOTKEY_ID);
    }
    if let Ok(Some((modifiers, key))) = crate::config::parse_hotkey(&wanted)
        && unsafe { RegisterHotKey(hwnd, HOTKEY_ID, modifiers | MOD_NOREPEAT, key) } == 0
    {
        crate::app::local_result(
            shared,
            crate::commands::Outcome::Failed,
            format!("The shortcut {wanted} is in use by another program; choose a different one."),
        );
    }
}
const SHOW_DASHBOARD: u32 = WM_APP + 2;
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
    icons: Icons,
    taskbar_message: u32,
    /// Icon state and tooltip last sent to the shell; unchanged ticks send nothing.
    shown: Option<(StateKind, String)>,
    /// Consecutive timer ticks that found game detection unavailable.
    detection_down: u32,
    notifications: crate::notifications::Queue,
    clock: std::time::Instant,
    menu_open: bool,
}
/// Detection must stay down this many two-second ticks before it is announced.
/// A wake from sleep or a start-up scan recovers well inside it.
const DETECTION_DOWN_TICKS: u32 = 10;
/// Whether the current state is a failure worth a notification: an operation
/// that did not finish, saved AI still waiting after a restart, or detection
/// that stays broken. The brief hold after Windows resumes is not.
pub(crate) fn failure_due(activity: Activity, message: &str, detection_down: u32) -> bool {
    if message.starts_with("Needs attention") {
        return true;
    }
    match activity {
        Activity::PartialFailure => true,
        Activity::Recovery => !message.starts_with("Windows resumed"),
        Activity::DetectionUnavailable => detection_down >= DETECTION_DOWN_TICKS,
        _ => false,
    }
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
/// A plain result box for a one-shot command run without a console.
pub fn info(message: &str) {
    let text = wide(message);
    let title = wide("GamePause for Local AI");
    unsafe {
        MessageBoxW(
            null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

/// The tray carries quick controls only. Tools and settings stay in the dashboard.
fn command_items(state: &crate::app::Shared) -> Vec<(usize, String, u32)> {
    let availability = state.controls().availability();
    vec![
        (
            Command::Automation as usize,
            "Pause AI automatically while gaming".into(),
            if state.config.automation_enabled {
                MF_CHECKED
            } else {
                0
            },
        ),
        (
            Command::Pause as usize,
            "Pause AI".into(),
            if availability.pause { 0 } else { MF_GRAYED },
        ),
        (
            Command::Resume as usize,
            crate::restore_dialog::resume_label(
                !state.active_games.is_empty(),
                state.coexistence,
                state.pending,
            )
            .into(),
            if availability.restore || availability.resume {
                0
            } else {
                MF_GRAYED
            },
        ),
        (0, String::new(), MF_SEPARATOR),
        (Command::OpenDashboard as usize, "Open GamePause".into(), 0),
        (Command::Quit as usize, "Quit".into(), 0),
    ]
}
/// Read the OS app-color preference. `true` = dark mode. The key/value is the
/// documented `HKCU\...\Themes\Personalize\AppsUseLightTheme` DWORD; a missing
/// value or read error is treated as "light" (the Windows default) so the app
/// never mis-colors a window it can't read.
pub fn system_is_dark() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_value::<u32, _>("AppsUseLightTheme"))
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
        let choice = ui_snapshot()
            .and_then(|ui| ui.shared.lock().ok().map(|s| s.config.appearance))
            .unwrap_or_default();
        apply_theme_mode(hwnd, crate::theme::effective_dark(choice));
    }
}
pub(crate) unsafe fn apply_theme_mode(hwnd: HWND, dark: bool) {
    unsafe {
        let use_dark = u32::from(dark);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            &use_dark as *const u32 as *const core::ffi::c_void,
            size_of::<u32>() as u32,
        );
    }
}
unsafe fn notification(hwnd: HWND, operation: u32, ui: &UI) {
    let (message, activity) = ui
        .shared
        .lock()
        .map(|s| (s.message.clone(), s.activity))
        .unwrap_or_else(|_| ("GamePause".into(), Activity::Unknown));
    unsafe { notify_icon(hwnd, operation, ui, activity_kind(activity), &message) }
}
unsafe fn notify_icon(hwnd: HWND, operation: u32, ui: &UI, kind: StateKind, message: &str) {
    unsafe {
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
    UI_STATE.with(|state| {
        if let Some(current) = state.borrow_mut().as_mut() {
            current.shown = (operation != NIM_DELETE).then(|| (kind, message.to_owned()));
        }
    });
}
/// What one timer tick reads from shared state.
struct Seen {
    message: String,
    activity: Activity,
    pending: bool,
    pause: u64,
    restore: u64,
    freed: u64,
    ask_prompt: Vec<String>,
    suggestion: Option<String>,
    visual: bool,
    sound: bool,
}
/// Tray appearance follows typed worker evidence. Recovery alone is not a pause.
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
fn activity_kind(activity: Activity) -> StateKind {
    match activity {
        Activity::Paused | Activity::ManualHold | Activity::Countdown => StateKind::Paused,
        Activity::PartialFailure | Activity::DetectionUnavailable | Activity::Recovery => {
            StateKind::Attention
        }
        _ => StateKind::Idle,
    }
}
unsafe fn state_toast(hwnd: HWND, message: &str, kind: StateKind, sound: bool) -> bool {
    unsafe {
        let spec = toast_spec(kind);
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = 1;
        // Completion state can become stale while another application's toast is visible.
        data.uFlags = NIF_INFO | NIF_REALTIME;
        data.dwInfoFlags = notification_flags(kind, sound);
        let title: Vec<_> = spec.title.encode_utf16().collect();
        data.szInfoTitle[..title.len()].copy_from_slice(&title);
        let info: Vec<_> = message.encode_utf16().take(255).collect();
        data.szInfo[..info.len()].copy_from_slice(&info);
        Shell_NotifyIconW(NIM_MODIFY, &data) != 0
    }
}
unsafe fn menu(hwnd: HWND, ui: &UI) {
    unsafe {
        let state = ui.shared.lock().unwrap();
        let mut items = crate::presentation::status(&state)
            .tray_lines()
            .map(|line| (0, line, MF_GRAYED))
            .to_vec();
        items.push((0, String::new(), MF_SEPARATOR));
        items.extend(command_items(&state));
        let dark = crate::theme::effective_dark(state.config.appearance);
        let activity = state.activity;
        drop(state);
        let Some(menu) = crate::native_menu::create(hwnd, items, dark, activity) else {
            return;
        };
        let mut point: POINT = std::mem::zeroed();
        GetCursorPos(&mut point);
        SetForegroundWindow(hwnd);
        let id = TrackPopupMenu(
            menu.handle,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            0,
            hwnd,
            null(),
        );
        drop(menu);
        // P1-7: gate "Resume AI" on discovery readiness (mirror the
        // dashboard) and surface feedback instead of the worker no-opping
        // silently. The worker keeps its own gate as the safety net.
        let Some(command) = Command::from_id(id) else {
            return;
        };
        if !crate::ui_commands::allowed(&ui.shared, command) {
            return;
        }
        let action = match command {
            Command::Pause => {
                crate::app::request_core(&ui.shared, &ui.tx, CoreCommand::Pause);
                None
            }
            Command::Resume => {
                crate::restore_dialog::request(hwnd, &ui.shared, &ui.tx, &ui.folder);
                None
            }
            Command::Automation => {
                crate::app::request_action(
                    &ui.shared,
                    &ui.tx,
                    Action::Disable,
                    "Change automatic pausing",
                );
                None
            }
            Command::Quit => {
                crate::app::request_quit(&ui.shared, &ui.tx);
                None
            }
            Command::OpenDashboard => {
                dashboard::show(ui.shared.clone(), ui.tx.clone(), ui.folder.clone());
                None
            }
            // Dashboard-only commands; the tray menu never offers them.
            Command::Refresh
            | Command::Doctor
            | Command::Verify
            | Command::OpenFolder
            | Command::Startup => None,
        };
        if let Some(action) = action {
            let _ = ui.tx.send(action);
        }
        PostMessageW(hwnd, WM_NULL, 0, 0);
    }
}
unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        window_proc_inner(hwnd, message, w, l)
    }))
    .unwrap_or_else(|_| {
        crate::app::log(
            &crate::config::data_directory(),
            "Tray callback panic contained",
        );
        0
    })
}
unsafe fn window_proc_inner(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_MEASUREITEM if unsafe { crate::native_menu::measure(l) } => 1,
        WM_DRAWITEM if unsafe { crate::native_menu::draw(l) } => 1,
        WM_POWERBROADCAST => {
            if let Some(ui) = ui_snapshot() {
                let signal = ui.shared.lock().ok().map(|shared| shared.power.clone());
                if signal.is_some_and(|signal| signal.notify(w)) {
                    if let Ok(mut shared) = ui.shared.lock() {
                        shared.detection_ok = false;
                        shared.restore_offer = None;
                        shared.coexistence = false;
                        shared.provider_statuses.clear();
                        shared.activity = crate::control::Activity::DetectionUnavailable;
                        shared.message = "Power state changed; fresh game detection is required before AI control.".into();
                    }
                    let _ = ui.tx.send(Action::PowerChanged);
                }
            }
            1
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            unsafe {
                apply_theme(hwnd);
            }
            dashboard::theme_changed();
            0
        }
        WM_TIMER => {
            dashboard::refresh();
            if FINISHED.load(Ordering::Relaxed) {
                unsafe {
                    DestroyWindow(hwnd);
                }
                return 0;
            }
            if let Some(ui) = ui_snapshot() {
                sync_hotkey(hwnd, &ui.shared);
                #[cfg(test)]
                if ui.menu_open {
                    MENU_TIMER_TICKS.fetch_add(1, Ordering::Relaxed);
                }
                // One short lock copies only what this tick needs.
                let seen = ui.shared.lock().ok().map(|s| Seen {
                    message: s.message.clone(),
                    activity: s.activity,
                    pending: s.pending,
                    pause: s.pause_completions,
                    restore: s.restore_completions,
                    freed: s.freed_bytes,
                    ask_prompt: s.ask_prompt.clone(),
                    suggestion: s.suggestion.clone(),
                    visual: s.config.notifications_enabled,
                    sound: s.config.sound_enabled,
                });
                if let Some(snapshot) = seen {
                    let kind = activity_kind(snapshot.activity);
                    // The shell is told only when the icon or tooltip changes.
                    if ui
                        .shown
                        .as_ref()
                        .is_none_or(|(shown, text)| *shown != kind || *text != snapshot.message)
                    {
                        unsafe {
                            notify_icon(hwnd, NIM_MODIFY, &ui, kind, &snapshot.message);
                        }
                    }
                    let detection_down = UI_STATE.with(|state| {
                        let mut state = state.borrow_mut();
                        let Some(current) = state.as_mut() else {
                            return 0;
                        };
                        current.detection_down =
                            if snapshot.activity == Activity::DetectionUnavailable {
                                current.detection_down.saturating_add(1)
                            } else {
                                0
                            };
                        current.detection_down
                    });
                    let failure = failure_due(snapshot.activity, &snapshot.message, detection_down);
                    let ask = if !snapshot.ask_prompt.is_empty() {
                        Some(format!(
                            "{} is running. AI is still running: open GamePause to pause it for this game.",
                            snapshot.ask_prompt.join(", ")
                        ))
                    } else {
                        snapshot.suggestion.as_deref().map(|path| {
                            format!(
                                "{} looks like a game GamePause does not know. Open GamePause to add it.",
                                crate::process_session::file_name(path)
                            )
                        })
                    };
                    let delivery = UI_STATE.with(|state| {
                        let mut state = state.borrow_mut();
                        let current = state.as_mut()?;
                        current.notifications.poll(
                            current.clock.elapsed(),
                            crate::notifications::Input {
                                pause: snapshot.pause,
                                restore: snapshot.restore,
                                activity: snapshot.activity,
                                pending: snapshot.pending,
                                freed: snapshot.freed,
                                ask: ask.as_deref(),
                                failure: failure.then_some(snapshot.message.as_str()),
                            },
                            snapshot.visual,
                            snapshot.sound,
                        )
                    });
                    // Release every state borrow before shell calls that can reenter.
                    if let Some(delivery) = delivery
                        && !crate::notifications::deliver(
                            &delivery,
                            &mut NativeNotifications { hwnd },
                        )
                    {
                        crate::app::log(
                            &ui.folder,
                            "Windows notification delivery failed; persistent dashboard/tray state is retained.",
                        );
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
        WM_HOTKEY if w == HOTKEY_ID as usize => {
            // One toggle: pause when that is allowed, otherwise resume through
            // the same guarded path as the tray's Resume AI.
            if let Some(ui) = ui_snapshot() {
                let pause = ui
                    .shared
                    .lock()
                    .map(|s| s.controls().availability().allows(CoreCommand::Pause))
                    .unwrap_or(false);
                if pause {
                    crate::app::request_core(&ui.shared, &ui.tx, CoreCommand::Pause);
                } else if crate::ui_commands::allowed(&ui.shared, Command::Resume) {
                    crate::restore_dialog::request(hwnd, &ui.shared, &ui.tx, &ui.folder);
                }
            }
            0
        }
        WM_DESTROY => {
            unsafe {
                UnregisterHotKey(hwnd, HOTKEY_ID);
            }
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
            shown: None,
            detection_down: 0,
            notifications: crate::notifications::Queue::default(),
            clock: std::time::Instant::now(),

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
mod tests;
