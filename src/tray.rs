use crate::{
    app::{Action, SharedState},
    control::{Activity, CoreCommand},
    dashboard,
    ui_commands::Command,
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
    notifications: crate::notifications::Queue,
    clock: std::time::Instant,
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
pub fn open_path(path: &Path) -> Result<()> {
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
        anyhow::bail!(
            "Could not open this folder: {display} (shell code {})",
            result as isize
        );
    }
    Ok(())
}
pub fn request_startup(state: &SharedState, enabled: bool, folder: &Path) {
    if !crate::ui_commands::allowed(state, Command::Startup) {
        return;
    }
    use crate::commands::Outcome;
    if startup_enabled() == enabled {
        crate::app::local_result(
            state,
            Outcome::NoChange,
            "Windows startup preference unchanged.",
        );
        return;
    }
    match set_startup(enabled, folder) {
        Ok(()) => crate::app::local_result(
            state,
            Outcome::Completed,
            if enabled {
                "Start with Windows enabled."
            } else {
                "Start with Windows disabled."
            },
        ),
        Err(error) => crate::app::local_result(
            state,
            Outcome::Failed,
            format!("Could not save Windows startup preference: {error:#}"),
        ),
    }
}
pub fn request_folder(state: &SharedState, folder: &Path) {
    if !crate::ui_commands::allowed(state, Command::OpenFolder) {
        return;
    }
    match open_path(folder) {
        Ok(()) => crate::app::local_result(
            state,
            crate::commands::Outcome::Completed,
            "Logs and status folder opened.",
        ),
        Err(error) => crate::app::local_result(
            state,
            crate::commands::Outcome::Failed,
            format!("Could not open logs folder: {error:#}"),
        ),
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
/// The tray glyph's solid fill color per state. Pure + unit-testable: this is
/// the "state -> icon variant" mapping P1-6 wants asserted without Win32.
/// `Idle` is the app's brand color; `Paused` reads "standby"; `Attention` is
/// a warning red that pops against the dark tray.
pub fn icon_tint(kind: StateKind) -> [u8; 3] {
    match kind {
        StateKind::Idle => [220, 168, 72],
        StateKind::Paused => [56, 132, 255],
        StateKind::Attention => [230, 62, 62],
    }
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
        let (message, activity) = ui
            .shared
            .lock()
            .map(|s| (s.message.clone(), s.activity))
            .unwrap_or_else(|_| ("GamePause".into(), Activity::Unknown));
        let kind = activity_kind(activity);
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
        Activity::PartialFailure
        | Activity::DetectionUnavailable
        | Activity::Recovery
        | Activity::Unavailable => StateKind::Attention,
        _ => StateKind::Idle,
    }
}
/// Toast severity. Pure data so the mapping is unit-testable (P1-5 acceptance).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    /// Pause-success or restore-success — the "good" system sound.
    Success,
    /// Any failure — the warning system sound.
    Error,
}
/// Titles and severity for typed completion/failure events. The bounded queue
/// supplies factual completion text or the current failure message.
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
/// Sound-only mode uses this system sound. Visual notifications use Windows sound.
pub fn beep_code(kind: StateKind) -> u32 {
    match kind {
        StateKind::Attention => MB_ICONASTERISK,
        // Pause-success and restore-success are both "good" → the OK sound.
        StateKind::Idle | StateKind::Paused => MB_OK,
    }
}
fn notification_flags(kind: StateKind, sound: bool) -> u32 {
    (if kind == StateKind::Attention {
        NIIF_ERROR
    } else {
        NIIF_INFO
    }) | NIIF_RESPECT_QUIET_TIME
        | if sound { 0 } else { NIIF_NOSOUND }
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
struct NativeNotifications {
    hwnd: HWND,
}
fn notification_kind(kind: crate::notifications::Kind) -> StateKind {
    match kind {
        crate::notifications::Kind::Paused => StateKind::Paused,
        crate::notifications::Kind::Restored | crate::notifications::Kind::Ask => StateKind::Idle,
        crate::notifications::Kind::Failure => StateKind::Attention,
    }
}
impl crate::notifications::Sink for NativeNotifications {
    fn toast(&mut self, event: &crate::notifications::Event, sound: bool) -> bool {
        unsafe { state_toast(self.hwnd, &event.text, notification_kind(event.kind), sound) }
    }
    fn sound(&mut self, event: &crate::notifications::Event) -> bool {
        unsafe { MessageBeep(beep_code(notification_kind(event.kind))) != 0 }
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
                crate::restore_dialog::request(hwnd, &ui.shared, &ui.tx);
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
                let snapshot = ui.shared.lock().map(|s| s.clone()).ok();
                if let Some(snapshot) = snapshot {
                    let failure = activity_kind(snapshot.activity) == StateKind::Attention
                        || snapshot.message.starts_with("Needs attention:");
                    let ask = (!snapshot.ask_prompt.is_empty()).then(|| {
                        format!(
                            "{} is running. AI is still running: open GamePause to pause it for this game.",
                            snapshot.ask_prompt.join(", ")
                        )
                    });
                    let delivery = UI_STATE.with(|state| {
                        let mut state = state.borrow_mut();
                        let current = state.as_mut()?;
                        current.notifications.poll(
                            current.clock.elapsed(),
                            crate::notifications::Input {
                                pause: snapshot.pause_completions,
                                restore: snapshot.restore_completions,
                                activity: snapshot.activity,
                                pending: snapshot.pending,
                                freed: snapshot.freed_bytes,
                                ask: ask.as_deref(),
                                failure: failure.then_some(snapshot.message.as_str()),
                            },
                            snapshot.config.notifications_enabled,
                            snapshot.config.sound_enabled,
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
mod tests {
    #[test]
    fn manual_hold_has_one_resume_action_and_no_restore_or_duplicate_resume() {
        let state = crate::app::Shared {
            active_mode: true,
            discovery_ready: true,
            detection_ok: true,
            manual_pause: true,
            pending: true,
            activity: Activity::ManualHold,
            ..Default::default()
        };
        let items = command_items(&state);
        assert_eq!(items.iter().filter(|item| item.1 == "Resume AI").count(), 1);
        assert!(!items.iter().any(|item| item.1.contains("Restore")));
        assert_eq!(
            items
                .iter()
                .find(|item| item.0 == Command::Pause as usize)
                .unwrap()
                .2,
            MF_GRAYED
        );
        assert_eq!(
            items
                .iter()
                .find(|item| item.0 == Command::Resume as usize)
                .unwrap()
                .2,
            0
        );
    }

    #[test]
    fn menu_is_the_same_quick_controls_with_or_without_advanced() {
        let mut state = crate::app::Shared::default();
        let basic = crate::dashboard::shared_command_ids(false);
        for advanced in [false, true] {
            state.config.advanced_settings_visible = advanced;
            let items = super::command_items(&state);
            let ids = items
                .iter()
                .filter_map(|item| crate::ui_commands::Command::from_id(item.0 as i32))
                .collect::<Vec<_>>();
            assert_eq!(
                ids,
                [
                    crate::ui_commands::Command::Automation,
                    crate::ui_commands::Command::Pause,
                    crate::ui_commands::Command::Resume,
                    crate::ui_commands::Command::OpenDashboard,
                    crate::ui_commands::Command::Quit,
                ]
            );
            // Every tray action has a dashboard counterpart that needs no Advanced toggle.
            assert!(ids.iter().all(|command| {
                *command == crate::ui_commands::Command::OpenDashboard
                    || basic.contains(&(*command as i32))
            }));
            // No informational rows hide among the commands.
            assert!(
                items
                    .iter()
                    .all(|item| item.0 != 0 || item.2 == MF_SEPARATOR)
            );
        }
    }
    use super::*;
    #[test]
    fn tray_uses_confirmed_activity_not_pending_recovery() {
        assert_eq!(activity_kind(Activity::Paused), StateKind::Paused);
        assert_eq!(activity_kind(Activity::ManualHold), StateKind::Paused);
        assert_eq!(activity_kind(Activity::Countdown), StateKind::Paused);
        assert_eq!(activity_kind(Activity::Recovery), StateKind::Attention);
        assert_eq!(
            activity_kind(Activity::PartialFailure),
            StateKind::Attention
        );
        assert_eq!(activity_kind(Activity::Unloading), StateKind::Idle);
        assert_eq!(activity_kind(Activity::Capturing), StateKind::Idle);
        assert_eq!(activity_kind(Activity::Watching), StateKind::Idle);
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
        // Each tint supplies three DIB color bytes; the attention color is a warning red
        // (high R, low G/B) so it pops against the dark tray.
        assert!(attention[0] > 180 && attention[1] < 120 && attention[2] < 120);
        // DIB pixels are BGR: paused uses orange with a dominant red byte.
        assert!(paused[2] > paused[0] && paused[2] > paused[1]);
    }
    #[test]
    fn timer_can_reenter_while_menu_context_is_alive() {
        use crate::app::Shared;
        use std::sync::{Arc, Mutex, mpsc};
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Mutex::new(Shared {
            commands: Default::default(),
            activity: Activity::PartialFailure,
            restore_offer: None,
            coexistence: false,
            restore_feedback: None,
            detection_ok: false,
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
            verify_report: None,
            verifying: false,
            pause_completions: 0,
            restore_completions: 0,
            provider_statuses: vec![],
            doctor_report: None,
            doctor_pending: false,
            lm_missing: false,
            freed_bytes: 0,
            ask_prompt: vec![],
            power: Default::default(),
        }));
        {
            let mut state = shared.lock().unwrap();
            state.config.notifications_enabled = false;
            state.config.sound_enabled = false;
        }
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
                notifications: crate::notifications::Queue::default(),
                clock: std::time::Instant::now(),

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
        shared.lock().unwrap().detection_ok = true;
        assert_eq!(
            unsafe { window_proc_inner(null_mut(), WM_POWERBROADCAST, 4, 0) },
            1
        );
        assert!(shared.lock().unwrap().power.snapshot().suspended);
        assert!(!shared.lock().unwrap().detection_ok);
        assert!(matches!(rx.try_recv(), Ok(Action::PowerChanged)));
        assert_eq!(
            unsafe { window_proc_inner(null_mut(), WM_POWERBROADCAST, 18, 0) },
            1
        );
        assert!(!shared.lock().unwrap().power.snapshot().suspended);
        let generation = shared.lock().unwrap().power.snapshot().generation;
        assert!(matches!(rx.try_recv(), Ok(Action::PowerChanged)));
        unsafe {
            window_proc_inner(null_mut(), WM_POWERBROADCAST, 7, 0);
        }
        assert_eq!(
            shared.lock().unwrap().power.snapshot().generation,
            generation
        );
        assert!(
            rx.try_recv().is_err(),
            "user-interaction resume must not reset grace a second time"
        );
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
            commands: Default::default(),
            activity: Activity::Observation,
            restore_offer: None,
            coexistence: false,
            restore_feedback: None,
            detection_ok: false,
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
            verify_report: None,
            verifying: false,
            pause_completions: 0,
            restore_completions: 0,
            provider_statuses: vec![],
            doctor_report: None,
            doctor_pending: false,
            lm_missing: false,
            freed_bytes: 0,
            ask_prompt: vec![],
            power: Default::default(),
        }));
        let (tx, _rx) = mpsc::channel();
        {
            let mut state = shared.lock().unwrap();
            state.config.notifications_enabled = false;
            state.config.sound_enabled = false;
        }
        let driver_state = shared.clone();
        let driver = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while WINDOW.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let hwnd = WINDOW.load(Ordering::Relaxed) as HWND;
            assert!(!hwnd.is_null(), "test window did not initialize");
            unsafe {
                SendMessageW(hwnd, WM_POWERBROADCAST, 4, 0);
            }
            assert!(driver_state.lock().unwrap().power.snapshot().suspended);
            unsafe {
                SendMessageW(hwnd, WM_POWERBROADCAST, 18, 0);
            }
            let resumed = driver_state.lock().unwrap().power.snapshot();
            assert!(!resumed.suspended);
            unsafe {
                SendMessageW(hwnd, WM_POWERBROADCAST, 7, 0);
            }
            assert_eq!(driver_state.lock().unwrap().power.snapshot(), resumed);
            let mut counts = Vec::new();
            for iteration in 0..3 {
                let before = MENU_TIMER_TICKS.load(Ordering::Relaxed);
                unsafe {
                    PostMessageW(hwnd, CALLBACK, 0, WM_RBUTTONUP as LPARAM);
                }
                driver_state
                    .lock()
                    .unwrap()
                    .config
                    .advanced_settings_visible = iteration % 2 == 0;
                driver_state.lock().unwrap().config.appearance = if iteration % 2 == 0 {
                    crate::config::Appearance::Dark
                } else {
                    crate::config::Appearance::Light
                };
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
    #[test]
    fn notifications_follow_completions_not_countdowns_or_retry_states() {
        assert_eq!(
            notification_flags(StateKind::Paused, false) & NIIF_NOSOUND,
            NIIF_NOSOUND
        );
        assert_eq!(notification_flags(StateKind::Idle, true) & NIIF_NOSOUND, 0);
        assert_ne!(
            notification_flags(StateKind::Attention, true) & NIIF_ERROR,
            0
        );
        for kind in [StateKind::Paused, StateKind::Idle, StateKind::Attention] {
            assert_ne!(notification_flags(kind, true) & NIIF_RESPECT_QUIET_TIME, 0);
        }
    }
}
