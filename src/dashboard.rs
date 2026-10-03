//! Native, on-demand dashboard. Win32 calls never retain state borrows.
use crate::{
    app::{Action, Shared, SharedState},
    config::{Config, ExtraGame},
    discovery::canonical,
    tray, wide,
};
use std::{
    cell::RefCell,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{Controls::Dialogs::*, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

const STATUS: i32 = 100;
static RUNNING_REQUESTED: AtomicBool = AtomicBool::new(false);
pub fn needs_running_apps() -> bool {
    RUNNING_REQUESTED.load(Ordering::Relaxed)
}
const AUTO: i32 = 101;
const STARTUP: i32 = 102;
const GAMES: i32 = 103;
const RUNNING: i32 = 104;
const IGNORED: i32 = 105;
const REFRESH: i32 = 106;
const ADD: i32 = 107;
const SEARCH: i32 = 108;
const LIST: i32 = 109;
const DETAILS: i32 = 110;
const TOGGLE: i32 = 111;
const REMOVE: i32 = 112;
const RESTORE: i32 = 113;
const PAUSE: i32 = 114;
const DELAY: i32 = 115;
const ADDRESS: i32 = 116;
const SAVE: i32 = 117;
const CLI: i32 = 118;
const FEEDBACK: i32 = 119;
const SETTINGS: i32 = 120;
const DELAY_LABEL: i32 = 121;
const ADDRESS_LABEL: i32 = 122;
const TITLE: i32 = 130;
const SEARCH_LABEL: i32 = 131;
/// Scale a 96-DPI design coordinate to the target DPI.
#[must_use]
pub fn scale(value: i32, dpi: i32) -> i32 {
    value * dpi / 96
}

/// Dashboard height for the settings-panel visibility state, at the target DPI.
#[must_use]
pub fn window_height(settings_visible: bool, dpi: i32) -> i32 {
    scale(if settings_visible { 748 } else { 708 }, dpi)
}

/// Feedback-line geometry `(x, y, w, h)` for the settings-panel visibility state.
#[must_use]
pub fn feedback_position(settings_visible: bool, dpi: i32) -> (i32, i32, i32, i32) {
    (
        scale(24, dpi),
        scale(if settings_visible { 640 } else { 600 }, dpi),
        scale(836, dpi),
        scale(58, dpi),
    )
}

/// Pure: the sys-color index a `WM_CTLCOLOR*` control paints its background with.
/// Statics sit on the window background; buttons on the button face. Theme-correct
/// in both light and dark because both are `GetSysColor`-backed.
#[must_use]
pub fn ctlcolor_index(message: u32) -> SYS_COLOR_INDEX {
    if message == WM_CTLCOLORSTATIC {
        COLOR_WINDOW
    } else {
        COLOR_BTNFACE
    }
}

/// Base (96-DPI) geometry of one control; the single source used to relayout on DPI change.
#[derive(Clone, Copy)]
struct Layout {
    id: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// Every dashboard control's base geometry, in 96-DPI design units.
const LAYOUT: [Layout; 25] = [
    Layout {
        id: TITLE,
        x: 24,
        y: 16,
        w: 600,
        h: 26,
    },
    Layout {
        id: STATUS,
        x: 24,
        y: 52,
        w: 836,
        h: 54,
    },
    Layout {
        id: AUTO,
        x: 24,
        y: 112,
        w: 430,
        h: 28,
    },
    Layout {
        id: STARTUP,
        x: 478,
        y: 112,
        w: 382,
        h: 28,
    },
    Layout {
        id: GAMES,
        x: 24,
        y: 154,
        w: 105,
        h: 30,
    },
    Layout {
        id: RUNNING,
        x: 138,
        y: 154,
        w: 135,
        h: 30,
    },
    Layout {
        id: IGNORED,
        x: 282,
        y: 154,
        w: 110,
        h: 30,
    },
    Layout {
        id: SETTINGS,
        x: 402,
        y: 154,
        w: 130,
        h: 30,
    },
    Layout {
        id: REFRESH,
        x: 614,
        y: 154,
        w: 116,
        h: 30,
    },
    Layout {
        id: ADD,
        x: 740,
        y: 154,
        w: 120,
        h: 30,
    },
    Layout {
        id: SEARCH_LABEL,
        x: 24,
        y: 198,
        w: 62,
        h: 25,
    },
    Layout {
        id: SEARCH,
        x: 90,
        y: 194,
        w: 770,
        h: 28,
    },
    Layout {
        id: LIST,
        x: 24,
        y: 234,
        w: 836,
        h: 220,
    },
    Layout {
        id: DETAILS,
        x: 24,
        y: 466,
        w: 836,
        h: 76,
    },
    Layout {
        id: TOGGLE,
        x: 24,
        y: 554,
        w: 205,
        h: 32,
    },
    Layout {
        id: REMOVE,
        x: 239,
        y: 554,
        w: 193,
        h: 32,
    },
    Layout {
        id: PAUSE,
        x: 442,
        y: 554,
        w: 252,
        h: 32,
    },
    Layout {
        id: RESTORE,
        x: 704,
        y: 554,
        w: 156,
        h: 32,
    },
    Layout {
        id: DELAY_LABEL,
        x: 24,
        y: 600,
        w: 180,
        h: 25,
    },
    Layout {
        id: DELAY,
        x: 206,
        y: 596,
        w: 65,
        h: 28,
    },
    Layout {
        id: ADDRESS_LABEL,
        x: 292,
        y: 600,
        w: 80,
        h: 25,
    },
    Layout {
        id: ADDRESS,
        x: 376,
        y: 596,
        w: 218,
        h: 28,
    },
    Layout {
        id: SAVE,
        x: 604,
        y: 596,
        w: 128,
        h: 28,
    },
    Layout {
        id: CLI,
        x: 742,
        y: 596,
        w: 118,
        h: 28,
    },
    Layout {
        id: FEEDBACK,
        x: 24,
        y: 600,
        w: 836,
        h: 58,
    },
];

/// Settings-row control ids, toggled together and repositioned by `relayout`.
const SETTINGS_ROW: [i32; 6] = [DELAY, ADDRESS, SAVE, CLI, DELAY_LABEL, ADDRESS_LABEL];

fn is_settings_row(id: i32) -> bool {
    SETTINGS_ROW.contains(&id)
}

/// Show/hide the settings row and place the feedback line for the given visibility.
/// Shared by the Settings toggle and DPI-change relayout so both use one code path.
fn apply_settings_visibility(hwnd: HWND, dpi: i32, visible: bool) {
    unsafe {
        for id in SETTINGS_ROW {
            let child = GetDlgItem(hwnd, id);
            if !child.is_null() {
                ShowWindow(child, if visible { SW_SHOW } else { SW_HIDE });
            }
        }
        for e in LAYOUT {
            if !is_settings_row(e.id) {
                continue;
            }
            let child = GetDlgItem(hwnd, e.id);
            if !child.is_null() {
                MoveWindow(
                    child,
                    scale(e.x, dpi),
                    scale(e.y, dpi),
                    scale(e.w, dpi),
                    scale(e.h, dpi),
                    1,
                );
            }
        }
        let (fx, fy, fw, fh) = feedback_position(visible, dpi);
        let child = GetDlgItem(hwnd, FEEDBACK);
        if !child.is_null() {
            MoveWindow(child, fx, fy, fw, fh, 1);
        }
    }
}

/// Relayout every control for the target DPI, preserving the top-left corner and the
/// current settings-panel visibility. Driven by `WM_DPICHANGED` so moving the window
/// between monitors re-scales the controls instead of bitmap-stretching them.
fn relayout(hwnd: HWND, dpi: i32, settings_visible: bool) {
    unsafe {
        let mut rect: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut rect);
        MoveWindow(
            hwnd,
            rect.left,
            rect.top,
            scale(900, dpi),
            window_height(settings_visible, dpi),
            1,
        );
        for e in LAYOUT {
            if e.id == FEEDBACK || is_settings_row(e.id) {
                continue; // handled by apply_settings_visibility
            }
            let child = GetDlgItem(hwnd, e.id);
            if child.is_null() {
                continue;
            }
            MoveWindow(
                child,
                scale(e.x, dpi),
                scale(e.y, dpi),
                scale(e.w, dpi),
                scale(e.h, dpi),
                1,
            );
        }
        apply_settings_visibility(hwnd, dpi, settings_visible);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Games,
    Running,
    Ignored,
}
#[derive(Clone, PartialEq)]
struct Row {
    label: String,
    path: String,
    name: String,
    custom: bool,
    ignored: bool,
}
#[derive(Clone)]
struct WindowState {
    hwnd: HWND,
    shared: SharedState,
    tx: Sender<Action>,
    folder: PathBuf,
    page: Page,
    rows: Vec<Row>,
    last_status: String,
    revision: u64,
    font: HFONT,
    settings_visible: bool,
}
thread_local! {static STATE:RefCell<Option<WindowState>>=const {RefCell::new(None)};}
fn snapshot() -> Option<WindowState> {
    STATE.with(|s| s.borrow().clone())
}
fn text(hwnd: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(hwnd);
        let mut buffer = vec![0u16; n as usize + 1];
        let count = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
        String::from_utf16_lossy(&buffer[..count as usize])
    }
}
fn set(hwnd: HWND, value: &str) {
    unsafe {
        SetWindowTextW(hwnd, wide(value).as_ptr());
    }
}
fn checked(hwnd: HWND) -> bool {
    unsafe { SendMessageW(hwnd, BM_GETCHECK, 0, 0) == 1 }
}
fn selection(state: &WindowState) -> Option<Row> {
    unsafe {
        let i = SendMessageW(GetDlgItem(state.hwnd, LIST), LB_GETCURSEL, 0, 0);
        state.rows.get(i as usize).cloned()
    }
}
fn send_settings(state: &WindowState, config: Config) {
    if let Ok(mut shared) = state.shared.lock() {
        shared.config = config.clone();
        shared.settings_error = "Saving settings...".into();
    }
    let _ = state.tx.send(Action::Settings(Box::new(config)));
}
fn rows(shared: &Shared, page: Page, query: &str) -> Vec<Row> {
    let ignored = |path: &str| {
        shared
            .config
            .ignored_games
            .iter()
            .chain(&shared.config.excluded_paths)
            .any(|p| canonical(p) == canonical(path))
    };
    let mut rows = match page {
        Page::Games => shared
            .games
            .iter()
            .map(|g| {
                let off = ignored(&g.path);
                let running = shared
                    .active_games
                    .iter()
                    .any(|a| canonical(&a.path) == canonical(&g.path));
                Row {
                    label: format!(
                        "{}  |  {}  |  {}{}",
                        g.name,
                        g.launcher,
                        if off {
                            "Automatic pausing off"
                        } else {
                            "Automatic pausing on"
                        },
                        if running { "  |  Running" } else { "" }
                    ),
                    path: g.path.clone(),
                    name: g.name.clone(),
                    custom: g.launcher == "Custom",
                    ignored: off,
                }
            })
            .collect::<Vec<_>>(),
        Page::Running => shared
            .running_apps
            .iter()
            .map(|a| Row {
                label: a.name.clone(),
                path: a.path.clone(),
                name: a.name.trim_end_matches(".exe").into(),
                custom: false,
                ignored: ignored(&a.path),
            })
            .collect(),
        Page::Ignored => shared
            .config
            .ignored_games
            .iter()
            .chain(&shared.config.excluded_paths)
            .map(|path| Row {
                label: shared
                    .games
                    .iter()
                    .find(|g| canonical(&g.path) == canonical(path))
                    .map(|g| g.name.clone())
                    .unwrap_or_else(|| path.rsplit(['\\', '/']).next().unwrap_or(path).into()),
                path: path.clone(),
                name: String::new(),
                custom: false,
                ignored: true,
            })
            .collect(),
    };
    let query = query.to_lowercase();
    rows.retain(|row| {
        row.label.to_lowercase().contains(&query) || row.path.to_lowercase().contains(&query)
    });
    rows.sort_by_key(|row| row.label.to_lowercase());
    rows.dedup_by(|a, b| canonical(&a.path) == canonical(&b.path));
    rows
}
pub fn refresh() {
    let Some(state) = snapshot() else { return };
    let Ok(shared) = state.shared.lock().map(|s| s.clone()) else {
        return;
    };
    let query = text(unsafe { GetDlgItem(state.hwnd, SEARCH) });
    let updated = rows(&shared, state.page, &query);
    let active = shared
        .active_games
        .iter()
        .map(|g| g.game.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let status = format!(
        "{}\r\n{} games found across your launchers{}",
        shared.message,
        shared.games.len(),
        if active.is_empty() {
            String::new()
        } else {
            format!("  |  Running: {active}")
        }
    );
    if state.last_status != status {
        set(unsafe { GetDlgItem(state.hwnd, STATUS) }, &status);
    }
    unsafe {
        SendMessageW(
            GetDlgItem(state.hwnd, AUTO),
            BM_SETCHECK,
            usize::from(shared.config.automation_enabled),
            0,
        );
        SendMessageW(
            GetDlgItem(state.hwnd, STARTUP),
            BM_SETCHECK,
            usize::from(tray::startup_enabled()),
            0,
        );
        EnableWindow(GetDlgItem(state.hwnd, PAUSE), i32::from(shared.active_mode));
        EnableWindow(
            GetDlgItem(state.hwnd, RESTORE),
            i32::from(shared.active_mode && shared.pending && shared.discovery_ready),
        );
    }
    if updated != state.rows {
        let selected = selection(&state).map(|r| r.path);
        let list = unsafe { GetDlgItem(state.hwnd, LIST) };
        unsafe {
            SendMessageW(list, WM_SETREDRAW, 0, 0);
            SendMessageW(list, LB_RESETCONTENT, 0, 0);
            for row in &updated {
                SendMessageW(list, LB_ADDSTRING, 0, wide(&row.label).as_ptr() as isize);
            }
            if let Some(path) = selected
                && let Some(i) = updated.iter().position(|r| r.path == path)
            {
                SendMessageW(list, LB_SETCURSEL, i, 0);
            }
            SendMessageW(list, WM_SETREDRAW, 1, 0);
            InvalidateRect(list, null(), 1);
        }
    }
    // Do not overwrite an edit the user is typing while timers update status.
    if state.revision != shared.revision
        && unsafe { GetFocus() } != unsafe { GetDlgItem(state.hwnd, DELAY) }
        && unsafe { GetFocus() } != unsafe { GetDlgItem(state.hwnd, ADDRESS) }
    {
        set(
            unsafe { GetDlgItem(state.hwnd, DELAY) },
            &format!("{}", shared.config.restore_delay_seconds),
        );
        set(
            unsafe { GetDlgItem(state.hwnd, ADDRESS) },
            &shared.config.api_host,
        );
    }
    let errors = shared
        .discovery_errors
        .iter()
        .map(|(launcher, error)| format!("{launcher}: {error}"))
        .collect::<Vec<_>>()
        .join("; ");
    let feedback = if !shared.settings_error.is_empty() {
        shared.settings_error.clone()
    } else if !errors.is_empty() {
        format!("Some discovery needs attention: {errors}")
    } else {
        "Games refresh automatically. New recognized games are enabled without setup.".into()
    };
    set(unsafe { GetDlgItem(state.hwnd, FEEDBACK) }, &feedback);
    STATE.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.rows = updated;
            s.last_status = status;
            s.revision = shared.revision;
        }
    });
    update_selection();
}
fn update_selection() {
    let Some(state) = snapshot() else { return };
    let row = selection(&state);
    let detail=row.as_ref().map(|r|format!("{}\r\n{}\r\n{}",r.label,r.path,match state.page{Page::Games=>"Recognized from launcher metadata or your saved game. Launch normally to pause AI.",Page::Running=>"Choose Add selected as game if discovery missed this game. Helpers are excluded automatically.",Page::Ignored=>"This entry does not start automatic pausing. Enable it to recognize it again."})).unwrap_or_else(||match state.page{Page::Games=>"Select a game to see how it is recognized. No per-game setup is required.".into(),Page::Running=>"Running applications are shown only while this page is open. Select a missing game to add it.".into(),Page::Ignored=>"Ignored games and applications appear here. You can enable them again at any time.".into()});
    set(unsafe { GetDlgItem(state.hwnd, DETAILS) }, &detail);
    set(
        unsafe { GetDlgItem(state.hwnd, TOGGLE) },
        match state.page {
            Page::Running => "Add selected as game",
            Page::Ignored => "Enable selected",
            Page::Games => {
                if row.as_ref().is_some_and(|r| r.ignored) {
                    "Enable selected"
                } else {
                    "Ignore selected"
                }
            }
        },
    );
    unsafe {
        EnableWindow(GetDlgItem(state.hwnd, TOGGLE), i32::from(row.is_some()));
        EnableWindow(
            GetDlgItem(state.hwnd, REMOVE),
            i32::from(row.is_some_and(|r| r.custom)),
        );
    }
}
fn browse(hwnd: HWND) -> Option<String> {
    unsafe {
        let mut buffer = vec![0u16; 32768];
        let filter = wide("Windows executable (*.exe)\0*.exe\0\0");
        let mut dialog: OPENFILENAMEW = std::mem::zeroed();
        dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        dialog.hwndOwner = hwnd;
        dialog.lpstrFilter = filter.as_ptr();
        dialog.lpstrFile = buffer.as_mut_ptr();
        dialog.nMaxFile = buffer.len() as u32;
        dialog.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
        if GetOpenFileNameW(&mut dialog) == 0 {
            return None;
        }
        let n = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..n]))
    }
}
fn add_game(config: &mut Config, path: String, name: String) {
    config
        .excluded_paths
        .retain(|p| canonical(p) != canonical(&path));
    config
        .ignored_games
        .retain(|p| canonical(p) != canonical(&path));
    if !config
        .extra_games
        .iter()
        .any(|g| canonical(&g.path) == canonical(&path))
    {
        config.extra_games.push(ExtraGame { name, path });
    }
}
fn command(id: i32, notification: u32) {
    let Some(state) = snapshot() else { return };
    if id == SEARCH && notification == EN_CHANGE {
        refresh();
        return;
    }
    if id == LIST && notification == LBN_SELCHANGE {
        update_selection();
        return;
    }
    if notification != BN_CLICKED {
        return;
    }
    let Ok(mut config) = state.shared.lock().map(|s| s.config.clone()) else {
        return;
    };
    match id {
        AUTO => {
            config.automation_enabled = checked(unsafe { GetDlgItem(state.hwnd, AUTO) });
            send_settings(&state, config);
        }
        STARTUP => {
            if let Err(e) = tray::set_startup(
                checked(unsafe { GetDlgItem(state.hwnd, STARTUP) }),
                &state.folder,
            ) {
                show_error(state.hwnd, &format!("Could not update startup: {e:#}"));
            }
        }
        GAMES | RUNNING | IGNORED => {
            RUNNING_REQUESTED.store(id == RUNNING, Ordering::Relaxed);
            STATE.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.page = match id {
                        RUNNING => Page::Running,
                        IGNORED => Page::Ignored,
                        _ => Page::Games,
                    };
                }
            });
            set(unsafe { GetDlgItem(state.hwnd, SEARCH) }, "");
            refresh();
        }
        SETTINGS => {
            let visible = !state.settings_visible;
            STATE.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.settings_visible = visible;
                }
            });
            unsafe {
                let dpi = GetDpiForWindow(state.hwnd) as i32;
                apply_settings_visibility(state.hwnd, dpi, visible);
                let mut rect: RECT = std::mem::zeroed();
                GetWindowRect(state.hwnd, &mut rect);
                SetWindowPos(
                    state.hwnd,
                    null_mut(),
                    0,
                    0,
                    rect.right - rect.left,
                    window_height(visible, dpi),
                    SWP_NOMOVE | SWP_NOZORDER,
                );
            }
        }
        REFRESH => {
            let _ = state.tx.send(Action::Refresh);
        }
        ADD => {
            if let Some(path) = browse(state.hwnd) {
                let name = std::path::Path::new(&path)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                add_game(&mut config, path, name);
                send_settings(&state, config);
            }
        }
        TOGGLE => {
            if let Some(row) = selection(&state) {
                match state.page {
                    Page::Running => add_game(&mut config, row.path, row.name),
                    _ => {
                        config
                            .ignored_games
                            .retain(|p| canonical(p) != canonical(&row.path));
                        config
                            .excluded_paths
                            .retain(|p| canonical(p) != canonical(&row.path));
                        if !row.ignored {
                            config.ignored_games.push(row.path);
                        }
                    }
                }
                send_settings(&state, config);
            }
        }
        REMOVE => {
            if let Some(row) = selection(&state) {
                config
                    .extra_games
                    .retain(|g| canonical(&g.path) != canonical(&row.path));
                send_settings(&state, config);
            }
        }
        RESTORE => {
            let _ = state.tx.send(Action::Restore);
        }
        PAUSE => {
            let _ = state.tx.send(Action::Pause);
        }
        SAVE => {
            let value = text(unsafe { GetDlgItem(state.hwnd, DELAY) });
            match value.parse::<f64>() {
                Ok(value) => config.restore_delay_seconds = value,
                Err(_) => {
                    show_error(state.hwnd, "Restore delay must be a number of seconds.");
                    return;
                }
            }
            config.api_host = text(unsafe { GetDlgItem(state.hwnd, ADDRESS) })
                .trim()
                .into();
            if let Err(e) = config.validate() {
                show_error(state.hwnd, &format!("Check these settings: {e:#}"));
                return;
            }
            send_settings(&state, config);
        }
        CLI => {
            if let Some(path) = browse(state.hwnd) {
                config.lms_path = path;
                send_settings(&state, config);
            }
        }
        _ => (),
    }
}
fn show_error(hwnd: HWND, message: &str) {
    unsafe {
        MessageBoxW(
            hwnd,
            wide(message).as_ptr(),
            wide("GamePause").as_ptr(),
            MB_OK | MB_ICONWARNING,
        );
    }
}
unsafe extern "system" fn procedure(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_COMMAND => {
            command((w & 0xffff) as i32, ((w >> 16) & 0xffff) as u32);
            0
        }
        WM_TIMER => {
            refresh();
            0
        }
        WM_DPICHANGED => {
            let dpi = (w >> 16) as i32;
            let settings_visible = snapshot().map(|s| s.settings_visible).unwrap_or(false);
            relayout(hwnd, dpi, settings_visible);
            0
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => unsafe {
            let idx = ctlcolor_index(message);
            SetBkColor(w as HDC, GetSysColor(idx));
            let text = if message == WM_CTLCOLORSTATIC {
                COLOR_WINDOWTEXT
            } else {
                COLOR_BTNTEXT
            };
            SetTextColor(w as HDC, GetSysColor(text));
            GetSysColorBrush(idx) as LRESULT
        },
        WM_CLOSE => {
            unsafe {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_DESTROY => {
            RUNNING_REQUESTED.store(false, Ordering::Relaxed);
            let old = STATE.with(|s| s.borrow_mut().take());
            if let Some(old) = old {
                unsafe {
                    DeleteObject(old.font);
                }
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, w, l) },
    }
}
pub fn is_dialog_message(msg: &MSG) -> bool {
    if let Some(state) = snapshot() {
        unsafe { IsDialogMessageW(state.hwnd, msg) != 0 }
    } else {
        false
    }
}
pub fn close() {
    if let Some(state) = snapshot() {
        unsafe {
            DestroyWindow(state.hwnd);
        }
    }
}
pub fn show(shared: SharedState, tx: Sender<Action>, folder: PathBuf) {
    unsafe {
        if let Some(state) = snapshot() {
            ShowWindow(state.hwnd, SW_RESTORE);
            SetForegroundWindow(state.hwnd);
            return;
        }
        let instance = GetModuleHandleW(null());
        let class = wide("GamePauseDashboard");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let dpi = GetDpiForSystem();
        let scale = |v: i32| v * dpi as i32 / 96;
        let width = scale(900);
        let height = scale(708);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("GamePause for LM Studio").as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            (GetSystemMetrics(SM_CXSCREEN) - width) / 2,
            (GetSystemMetrics(SM_CYSCREEN) - height) / 2,
            width,
            height,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            tray::error("Could not open the GamePause dashboard.");
            return;
        }
        tray::apply_theme(hwnd);
        let font = CreateFontW(
            -scale(16),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            0,
            0,
            CLEARTYPE_QUALITY as u32,
            0,
            wide("Segoe UI").as_ptr(),
        );
        let control =
            |class: &str, label: &str, id: i32, x: i32, y: i32, w: i32, h: i32, style: u32| {
                let child = CreateWindowExW(
                    if class == "EDIT" || class == "LISTBOX" {
                        WS_EX_CLIENTEDGE
                    } else {
                        0
                    },
                    wide(class).as_ptr(),
                    wide(label).as_ptr(),
                    WS_CHILD | WS_VISIBLE | style,
                    scale(x),
                    scale(y),
                    scale(w),
                    scale(h),
                    hwnd,
                    id as HMENU,
                    instance,
                    null(),
                );
                SendMessageW(child, WM_SETFONT, font as usize, 1);
                child
            };
        control("STATIC", "GamePause", TITLE, 24, 16, 600, 26, 0);
        control(
            "STATIC",
            "Starting automatic discovery...",
            STATUS,
            24,
            52,
            836,
            54,
            0,
        );
        control(
            "BUTTON",
            "Automatically pause AI while gaming",
            AUTO,
            24,
            112,
            430,
            28,
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
        );
        control(
            "BUTTON",
            "Start when I sign in to Windows",
            STARTUP,
            478,
            112,
            382,
            28,
            BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
        );
        control("BUTTON", "Games", GAMES, 24, 154, 105, 30, WS_TABSTOP);
        control(
            "BUTTON",
            "Running apps",
            RUNNING,
            138,
            154,
            135,
            30,
            WS_TABSTOP,
        );
        control("BUTTON", "Ignored", IGNORED, 282, 154, 110, 30, WS_TABSTOP);
        control(
            "BUTTON", "Settings", SETTINGS, 402, 154, 130, 30, WS_TABSTOP,
        );
        control(
            "BUTTON",
            "Refresh now",
            REFRESH,
            614,
            154,
            116,
            30,
            WS_TABSTOP,
        );
        control("BUTTON", "Add game...", ADD, 740, 154, 120, 30, WS_TABSTOP);
        control("STATIC", "Search", SEARCH_LABEL, 24, 198, 62, 25, 0);
        control(
            "EDIT",
            "",
            SEARCH,
            90,
            194,
            770,
            28,
            WS_TABSTOP | ES_AUTOHSCROLL as u32,
        );
        control(
            "LISTBOX",
            "",
            LIST,
            24,
            234,
            836,
            220,
            WS_TABSTOP | WS_VSCROLL | WS_HSCROLL | LBS_NOTIFY as u32 | LBS_NOINTEGRALHEIGHT as u32,
        );
        control(
            "EDIT",
            "",
            DETAILS,
            24,
            466,
            836,
            76,
            ES_MULTILINE as u32 | ES_READONLY as u32 | WS_VSCROLL,
        );
        control(
            "BUTTON",
            "Ignore selected",
            TOGGLE,
            24,
            554,
            205,
            32,
            WS_TABSTOP,
        );
        control(
            "BUTTON",
            "Remove custom game",
            REMOVE,
            239,
            554,
            193,
            32,
            WS_TABSTOP,
        );
        control(
            "BUTTON",
            "Pause / resume AI manually",
            PAUSE,
            442,
            554,
            252,
            32,
            WS_TABSTOP,
        );
        control(
            "BUTTON",
            "Restore AI now",
            RESTORE,
            704,
            554,
            156,
            32,
            WS_TABSTOP,
        );
        control(
            "STATIC",
            "Restore after (seconds)",
            DELAY_LABEL,
            24,
            600,
            180,
            25,
            0,
        );
        control(
            "EDIT",
            "30",
            DELAY,
            206,
            596,
            65,
            28,
            WS_TABSTOP | ES_AUTOHSCROLL as u32,
        );
        control("STATIC", "Local API", ADDRESS_LABEL, 292, 600, 80, 25, 0);
        control(
            "EDIT",
            "127.0.0.1:1234",
            ADDRESS,
            376,
            596,
            218,
            28,
            WS_TABSTOP | ES_AUTOHSCROLL as u32,
        );
        control(
            "BUTTON",
            "Save settings",
            SAVE,
            604,
            596,
            128,
            28,
            WS_TABSTOP,
        );
        control(
            "BUTTON",
            "Locate lms...",
            CLI,
            742,
            596,
            118,
            28,
            WS_TABSTOP,
        );
        control("STATIC", "", FEEDBACK, 24, 600, 836, 58, 0);
        for id in [DELAY, ADDRESS, SAVE, CLI, DELAY_LABEL, ADDRESS_LABEL] {
            ShowWindow(GetDlgItem(hwnd, id), SW_HIDE);
        }
        let revision = shared
            .lock()
            .map(|s| s.revision)
            .unwrap_or(0)
            .wrapping_sub(1);
        STATE.with(|s| {
            *s.borrow_mut() = Some(WindowState {
                hwnd,
                shared,
                tx,
                folder,
                page: Page::Games,
                rows: vec![],
                last_status: String::new(),
                revision,
                font,
                settings_visible: false,
            })
        });
        refresh();
        SetTimer(hwnd, 1, 1000, None);
        ShowWindow(hwnd, SW_SHOW);
        // A quiet startup can supply SW_HIDE in STARTUPINFO, overriding the first call.
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn game_rows_explain_running_ignored_and_search() {
        let mut shared = Shared::default();
        shared.games.push(crate::discovery::Game::new(
            "Steam",
            "292030",
            "The Witcher 3",
            r"D:\Games\Witcher",
        ));
        shared.active_games.push(crate::processes::ActiveGame {
            pid: 42,
            game: "The Witcher 3".into(),
            launcher: "Steam".into(),
            path: r"D:\Games\Witcher".into(),
        });
        shared.config.ignored_games.push(r"d:\games\witcher".into());
        let rows = rows(&shared, Page::Games, "WITCHER");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].label.contains("Running"));
        assert!(rows[0].label.contains("pausing off"));
        assert!(rows[0].ignored);
        assert_eq!(super::rows(&shared, Page::Ignored, "witcher").len(), 1);
        assert!(super::rows(&shared, Page::Games, "not installed").is_empty());
    }
    #[test]
    fn adding_executable_deduplicates_and_clears_ignore() {
        let mut config = Config::default();
        config.ignored_games.push(r"D:\Games\game.exe".into());
        add_game(&mut config, r"d:\games\GAME.exe".into(), "Game".into());
        add_game(&mut config, r"D:\Games\game.exe".into(), "Game".into());
        assert_eq!(config.extra_games.len(), 1);
        assert!(config.ignored_games.is_empty());
    }
    #[test]
    fn scale_follows_dpi_at_100_150_200() {
        assert_eq!(scale(100, 96), 100);
        assert_eq!(scale(100, 144), 150);
        assert_eq!(scale(100, 192), 200);
        assert_eq!(scale(960, 96), 960);
        assert_eq!(scale(960, 144), 1440);
        assert_eq!(scale(960, 192), 1920);
    }
    #[test]
    fn window_height_tracks_settings_visibility() {
        assert_eq!(window_height(false, 96), 708);
        assert_eq!(window_height(true, 96), 748);
        assert_eq!(window_height(false, 144), 1062);
        assert_eq!(window_height(true, 144), 1122);
    }
    #[test]
    fn feedback_line_sits_below_settings_row_when_expanded() {
        assert_eq!(feedback_position(false, 96), (24, 600, 836, 58));
        assert_eq!(feedback_position(true, 96), (24, 640, 836, 58));
        let (_, y, _, _) = feedback_position(true, 144);
        assert_eq!(y, 960);
    }
    #[test]
    fn ctlcolor_static_tracks_window_and_button_tracks_btnface() {
        // Statics sit on the window background; buttons on the button face.
        assert_eq!(
            super::ctlcolor_index(WM_CTLCOLORSTATIC),
            COLOR_WINDOW,
            "statics should track COLOR_WINDOW (theme-correct in light + dark)"
        );
        assert_eq!(
            super::ctlcolor_index(WM_CTLCOLORBTN),
            COLOR_BTNFACE,
            "buttons should track COLOR_BTNFACE"
        );
    }
    #[test]
    fn ctlcolor_static_returns_the_window_color() {
        // The acceptance test: the WM_CTLCOLORSTATIC handler returns a brush whose
        // color equals GetSysColor(COLOR_WINDOW), not a literal white.
        assert_eq!(
            super::ctlcolor_index(WM_CTLCOLORSTATIC) as i32,
            COLOR_WINDOW,
            "the handler must use COLOR_WINDOW, not RGB(255,255,255)"
        );
    }
}
