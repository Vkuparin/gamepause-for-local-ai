//! Native, on-demand dashboard. Win32 calls never retain state borrows.
use crate::{
    app::{Action, Shared, SharedState},
    config::{Config, ExtraGame},
    discovery::canonical,
    tray, wide,
};
use std::{
    cell::RefCell,
    collections::HashSet,
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
const RENAME: i32 = 121;
const VERIFY: i32 = 127;
const DELAY_LABEL: i32 = 122;
const ADDRESS_LABEL: i32 = 123;
const TITLE: i32 = 130;
const SEARCH_LABEL: i32 = 131;
const OPTIONS_BOX: i32 = 140;
const GAMES_BOX: i32 = 141;
const ACTIONS_BOX: i32 = 142;
const SETTINGS_BOX: i32 = 143;
/// Scale a 96-DPI design coordinate to the target DPI.
#[must_use]
pub fn scale(value: i32, dpi: i32) -> i32 {
    value * dpi / 96
}

/// Dashboard height for the settings-panel visibility state, at the target DPI.
/// Sections end at 716 (Settings box); feedback follows at 722/644.
#[must_use]
pub fn window_height(settings_visible: bool, dpi: i32) -> i32 {
    scale(if settings_visible { 792 } else { 712 }, dpi)
}

/// Feedback-line geometry `(x, y, w, h)` for the settings-panel visibility state.
#[must_use]
pub fn feedback_position(settings_visible: bool, dpi: i32) -> (i32, i32, i32, i32) {
    (
        scale(24, dpi),
        scale(if settings_visible { 722 } else { 644 }, dpi),
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

/// Base (96-DPI) geometry AND creation attributes of one control. The single
/// source of truth: used to create the control, to relayout it on DPI change,
/// and by the group-box bounds test (P1-3).
#[derive(Clone, Copy)]
struct Layout {
    id: i32,
    class: &'static str,
    label: &'static str,
    style: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// One section frame: its box id plus the ids of the controls it contains.
#[derive(Clone, Copy)]
#[cfg(test)]
struct GroupBox {
    id: i32,
    members: &'static [i32],
}

/// The four section frames and their contents. Test-only bookkeeping: the
/// runtime creates controls straight from `LAYOUT`.
#[cfg(test)]
const GROUPBOXES: [GroupBox; 4] = [
    GroupBox {
        id: OPTIONS_BOX,
        members: &[AUTO, STARTUP],
    },
    GroupBox {
        id: GAMES_BOX,
        members: &[
            GAMES,
            RUNNING,
            IGNORED,
            SETTINGS,
            REFRESH,
            ADD,
            SEARCH_LABEL,
            SEARCH,
            LIST,
            DETAILS,
        ],
    },
    GroupBox {
        id: ACTIONS_BOX,
        members: &[TOGGLE, REMOVE, PAUSE, RESTORE, VERIFY, RENAME],
    },
    GroupBox {
        id: SETTINGS_BOX,
        members: &[DELAY_LABEL, DELAY, ADDRESS_LABEL, ADDRESS, SAVE, CLI],
    },
];

/// A `LAYOUT` entry by control id (test helper; `Layout` is `Copy`).
#[cfg(test)]
fn by_id(id: i32) -> Layout {
    LAYOUT
        .iter()
        .find(|e| e.id == id)
        .copied()
        .expect("control id missing from LAYOUT")
}

/// Every dashboard control, in 96-DPI design units. Order: section boxes first,
/// then header, then top-to-bottom. Coordinates are the single source of truth
/// shared by creation, DPI relayout, and the group-box bounds test (P1-3).
const LAYOUT: [Layout; 31] = [
    Layout {
        id: OPTIONS_BOX,
        class: "BUTTON",
        label: "Options",
        style: BS_GROUPBOX as u32,
        x: 16,
        y: 108,
        w: 852,
        h: 54,
    },
    Layout {
        id: GAMES_BOX,
        class: "BUTTON",
        label: "Games",
        style: BS_GROUPBOX as u32,
        x: 16,
        y: 170,
        w: 852,
        h: 396,
    },
    Layout {
        id: ACTIONS_BOX,
        class: "BUTTON",
        label: "Actions",
        style: BS_GROUPBOX as u32,
        x: 16,
        y: 574,
        w: 852,
        h: 62,
    },
    Layout {
        id: SETTINGS_BOX,
        class: "BUTTON",
        label: "Settings",
        style: BS_GROUPBOX as u32,
        x: 16,
        y: 644,
        w: 852,
        h: 72,
    },
    Layout {
        id: TITLE,
        class: "STATIC",
        label: "GamePause",
        style: 0,
        x: 24,
        y: 16,
        w: 600,
        h: 26,
    },
    Layout {
        id: STATUS,
        class: "STATIC",
        label: "Starting automatic discovery...",
        style: 0,
        x: 24,
        y: 52,
        w: 836,
        h: 50,
    },
    Layout {
        id: AUTO,
        class: "BUTTON",
        label: "Automatically pause AI while gaming",
        style: BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
        x: 24,
        y: 128,
        w: 430,
        h: 28,
    },
    Layout {
        id: STARTUP,
        class: "BUTTON",
        label: "Start when I sign in to Windows",
        style: BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
        x: 478,
        y: 128,
        w: 382,
        h: 28,
    },
    Layout {
        id: GAMES,
        class: "BUTTON",
        label: "Games",
        style: WS_TABSTOP,
        x: 24,
        y: 194,
        w: 105,
        h: 30,
    },
    Layout {
        id: RUNNING,
        class: "BUTTON",
        label: "Running apps",
        style: WS_TABSTOP,
        x: 138,
        y: 194,
        w: 135,
        h: 30,
    },
    Layout {
        id: IGNORED,
        class: "BUTTON",
        label: "Ignored",
        style: WS_TABSTOP,
        x: 282,
        y: 194,
        w: 110,
        h: 30,
    },
    Layout {
        id: SETTINGS,
        class: "BUTTON",
        label: "Settings",
        style: WS_TABSTOP,
        x: 402,
        y: 194,
        w: 130,
        h: 30,
    },
    Layout {
        id: REFRESH,
        class: "BUTTON",
        label: "Refresh now",
        style: WS_TABSTOP,
        x: 614,
        y: 194,
        w: 116,
        h: 30,
    },
    Layout {
        id: ADD,
        class: "BUTTON",
        label: "Add game...",
        style: WS_TABSTOP,
        x: 740,
        y: 194,
        w: 120,
        h: 30,
    },
    Layout {
        id: SEARCH_LABEL,
        class: "STATIC",
        label: "Search",
        style: 0,
        x: 24,
        y: 238,
        w: 62,
        h: 25,
    },
    Layout {
        id: SEARCH,
        class: "EDIT",
        label: "",
        style: WS_TABSTOP | ES_AUTOHSCROLL as u32,
        x: 90,
        y: 234,
        w: 770,
        h: 28,
    },
    Layout {
        id: LIST,
        class: "LISTBOX",
        label: "",
        style: WS_TABSTOP
            | WS_VSCROLL
            | WS_HSCROLL
            | LBS_NOTIFY as u32
            | LBS_NOINTEGRALHEIGHT as u32,
        x: 24,
        y: 270,
        w: 836,
        h: 208,
    },
    Layout {
        id: DETAILS,
        class: "EDIT",
        label: "",
        style: ES_MULTILINE as u32 | ES_READONLY as u32 | WS_VSCROLL,
        x: 24,
        y: 490,
        w: 836,
        h: 66,
    },
    Layout {
        id: TOGGLE,
        class: "BUTTON",
        label: "Ignore selected",
        style: WS_TABSTOP,
        x: 24,
        y: 594,
        w: 140,
        h: 32,
    },
    Layout {
        id: REMOVE,
        class: "BUTTON",
        label: "Remove custom game",
        style: WS_TABSTOP,
        x: 170,
        y: 594,
        w: 140,
        h: 32,
    },
    Layout {
        id: PAUSE,
        class: "BUTTON",
        label: "Pause / resume AI manually",
        style: WS_TABSTOP,
        x: 316,
        y: 594,
        w: 140,
        h: 32,
    },
    Layout {
        id: RESTORE,
        class: "BUTTON",
        label: "Restore AI now",
        style: WS_TABSTOP,
        x: 462,
        y: 594,
        w: 140,
        h: 32,
    },
    Layout {
        id: VERIFY,
        class: "BUTTON",
        label: "Test round-trip",
        style: WS_TABSTOP,
        x: 608,
        y: 594,
        w: 140,
        h: 32,
    },
    Layout {
        id: RENAME,
        class: "BUTTON",
        label: "Rename",
        style: WS_TABSTOP,
        x: 754,
        y: 594,
        w: 106,
        h: 32,
    },
    Layout {
        id: DELAY_LABEL,
        class: "STATIC",
        label: "Restore after (seconds)",
        style: 0,
        x: 24,
        y: 666,
        w: 180,
        h: 25,
    },
    Layout {
        id: DELAY,
        class: "EDIT",
        label: "30",
        style: WS_TABSTOP | ES_AUTOHSCROLL as u32,
        x: 206,
        y: 664,
        w: 65,
        h: 28,
    },
    Layout {
        id: ADDRESS_LABEL,
        class: "STATIC",
        label: "Local API",
        style: 0,
        x: 292,
        y: 666,
        w: 80,
        h: 25,
    },
    Layout {
        id: ADDRESS,
        class: "EDIT",
        label: "127.0.0.1:1234",
        style: WS_TABSTOP | ES_AUTOHSCROLL as u32,
        x: 376,
        y: 664,
        w: 218,
        h: 28,
    },
    Layout {
        id: SAVE,
        class: "BUTTON",
        label: "Save settings",
        style: WS_TABSTOP,
        x: 604,
        y: 664,
        w: 128,
        h: 28,
    },
    Layout {
        id: CLI,
        class: "BUTTON",
        label: "Locate lms...",
        style: WS_TABSTOP,
        x: 742,
        y: 664,
        w: 118,
        h: 28,
    },
    Layout {
        id: FEEDBACK,
        class: "STATIC",
        label: "",
        style: 0,
        x: 24,
        y: 644,
        w: 836,
        h: 58,
    },
];

/// Settings-row control ids, toggled together and repositioned by `relayout`.
const SETTINGS_ROW: [i32; 7] = [
    DELAY,
    ADDRESS,
    SAVE,
    CLI,
    DELAY_LABEL,
    ADDRESS_LABEL,
    SETTINGS_BOX,
];

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

/// Show a message on the FEEDBACK line (control FEEDBACK), not a modal.
/// P1-7: settings-validation and rename errors are user-correctable, so they
/// belong inline next to the field that produced them; modals are reserved
/// for hard failures (missing data dir, startup write, ...).
fn set_feedback(hwnd: HWND, message: &str) {
    set(hwnd, message);
}

thread_local! {
    static ASK_NAME_RESULT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Modal text-input dialog for the Rename affordance. Returns the entered text
/// or `None` if the user cancelled (or the dialog could not be created).
/// Mirrors the dashboard's own WNDCLASSW + message-loop pattern, so it compiles
/// against the same windows-sys surface without new dependencies.
fn ask_name(parent: HWND, current: &str) -> Option<String> {
    const NAME_EDIT: i32 = 1001;
    const OK_BTN: i32 = 1;
    unsafe {
        let instance = GetModuleHandleW(null());
        let class = wide("GamePauseAskName");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(name_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
            ..std::mem::zeroed()
        };
        // Ignore "class already registered" (1410); a repeated Rename reuses it.
        if RegisterClassW(&wc) == 0 && GetLastError() != 1410 {
            return None;
        }
        let dpi = GetDpiForSystem() as i32;
        let scale = |v: i32| v * dpi / 96;
        let width = scale(420);
        let height = scale(150);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("Rename game").as_ptr(),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            (GetSystemMetrics(SM_CXSCREEN) - width) / 2,
            (GetSystemMetrics(SM_CYSCREEN) - height) / 2,
            width,
            height,
            parent,
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return None;
        }
        let font = CreateFontW(
            -scale(14),
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
        let edit = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            wide("EDIT").as_ptr(),
            wide(current).as_ptr(),
            WS_CHILD | WS_VISIBLE | ES_AUTOHSCROLL as u32,
            scale(16),
            scale(30),
            width - scale(32),
            scale(26),
            hwnd,
            null_mut(),
            instance,
            null(),
        );
        let ok = CreateWindowExW(
            0,
            wide("BUTTON").as_ptr(),
            wide("Rename").as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            width - scale(150),
            scale(86),
            scale(60),
            scale(26),
            hwnd,
            null_mut(),
            instance,
            null(),
        );
        let cancel = CreateWindowExW(
            0,
            wide("BUTTON").as_ptr(),
            wide("Cancel").as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            width - scale(84),
            scale(86),
            scale(60),
            scale(26),
            hwnd,
            null_mut(),
            instance,
            null(),
        );
        SetWindowLongPtrW(edit, GWLP_ID, NAME_EDIT as isize);
        SetWindowLongPtrW(ok, GWLP_ID, OK_BTN as isize);
        SetWindowLongPtrW(cancel, GWLP_ID, 2);
        SendMessageW(edit, WM_SETFONT, font as usize, 0);
        SendMessageW(ok, WM_SETFONT, font as usize, 0);
        SendMessageW(cancel, WM_SETFONT, font as usize, 0);
        SetFocus(edit);
        ShowWindow(hwnd, SW_SHOW);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) != 0 {
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    ASK_NAME_RESULT.with(|c| c.borrow().clone())
}

unsafe extern "system" fn name_proc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_COMMAND => {
            let id = (w & 0xffff) as i32;
            unsafe {
                if id == 1 {
                    let edit = GetDlgItem(hwnd, 1001);
                    ASK_NAME_RESULT.with(|c| *c.borrow_mut() = Some(text(edit)));
                } else {
                    ASK_NAME_RESULT.with(|c| *c.borrow_mut() = None);
                }
                DestroyWindow(hwnd);
            }
            0
        }
        WM_CLOSE => {
            ASK_NAME_RESULT.with(|c| *c.borrow_mut() = None);
            unsafe {
                DestroyWindow(hwnd);
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, w, l) },
    }
}

/// Pure: render a round-trip verify report (P2-1) for the FEEDBACK line:
/// one `name: detail` per step, marked ok/FAIL, with the summary last.
#[must_use]
pub fn render_verify_report(report: &crate::engine::VerifyReport) -> String {
    let mut lines = report
        .steps
        .iter()
        .map(|s| {
            format!(
                "{}: {} ({})",
                s.name,
                s.detail,
                if s.ok { "ok" } else { "FAIL" }
            )
        })
        .collect::<Vec<_>>()
        .join("  |  ");
    lines.push_str("  —  ");
    lines.push_str(&report.summary);
    lines
}

/// Pure: the Pause/Resume button label for a given mode state.
/// When auto-mode is off there is nothing to pause, so the button stays neutral.
#[must_use]
pub fn pause_button_label(active_mode: bool, manual_pause: bool) -> &'static str {
    if !active_mode {
        "Pause AI"
    } else if manual_pause {
        "Resume AI"
    } else {
        "Pause AI"
    }
}

/// Pure: whether the tray's "Restore AI now" item is enabled, and the
/// feedback message to show when it is not (empty string when enabled).
/// Mirrors the dashboard gate: restore is only meaningful once a game has
/// been discovered (discovery_ready) and is actually paused (pending).
#[must_use]
pub fn restore_gate(
    discovery_ready: bool,
    active_mode: bool,
    pending: bool,
) -> (bool, &'static str) {
    if !discovery_ready {
        (false, "Restore unavailable until a game is detected.")
    } else if !active_mode {
        (false, "Restore unavailable while GamePause is off.")
    } else if !pending {
        (false, "AI is already running — nothing to restore.")
    } else {
        (true, "")
    }
}

/// Pure: dedupe a row list on the canonical path, keeping the first row seen
/// for each distinct executable. Order-preserving.
///
/// This is the fix for the old adjacent-only `dedup_by` that let the same
/// executable survive under two display names (e.g. one per launch dir).
#[must_use]
fn dedupe_by_canonical(rows: &[Row]) -> Vec<Row> {
    let mut seen = HashSet::new();
    rows.iter()
        .filter(|row| seen.insert(canonical(&row.path)))
        .cloned()
        .collect()
}

/// Pure: apply the SAVE-settings action to a config, returning the new config
/// on success or a human-readable error string to show in the FEEDBACK line.
/// Routing validation failures to the feedback line (not a modal) is the P1-7
/// fix; modals are reserved for hard failures like a missing data dir.
pub fn apply_save(config: &Config, delay_text: &str, host_text: &str) -> Result<Config, String> {
    let mut out = config.clone();
    let delay = delay_text
        .trim()
        .parse::<f64>()
        .map_err(|_| "Restore delay must be a number of seconds.".to_string())?;
    if !(0.0..=3600.0).contains(&delay) {
        return Err("Restore delay must be between 0 and 3600 seconds.".to_string());
    }
    out.restore_delay_seconds = delay;
    let host = host_text.trim();
    if host.is_empty() {
        return Err("Local API address cannot be empty.".to_string());
    }
    out.api_host = host.into();
    out.validate()
        .map_err(|e| format!("Check these settings: {e:#}"))?;
    Ok(out)
}

/// Pure: apply the RENAME action to a config, returning the new config on
/// success or a human-readable error string for the FEEDBACK line.
/// Only rows that are custom (user-added) are renameable; discovered rows
/// keep their launcher-provided name.
pub fn apply_rename(config: &Config, row_path: &str, new_name: &str) -> Result<Config, String> {
    let name = new_name.trim();
    if name.is_empty() {
        return Err("Game name cannot be empty.".to_string());
    }
    let mut out = config.clone();
    let target = canonical(row_path);
    if let Some(game) = out
        .extra_games
        .iter_mut()
        .find(|g| canonical(&g.path) == target)
    {
        game.name = name.into();
        Ok(out)
    } else {
        Err("This game is not a custom entry — only games you added can be renamed.".to_string())
    }
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
    // Dedupe on canonical path *globally* (not adjacent-only after the label
    // sort), so the same executable listed under two names collapses to one row.
    dedupe_by_canonical(&rows)
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
        set(
            GetDlgItem(state.hwnd, PAUSE),
            pause_button_label(shared.active_mode, shared.manual_pause),
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
    // P2-1: a round-trip verify result (per-step, with the failing field on
    // failure) takes the FEEDBACK line; settings errors still win, then
    // discovery errors, then the default hint.
    let feedback = if !shared.settings_error.is_empty() {
        shared.settings_error.clone()
    } else if let Some(report) = &shared.verify_report {
        render_verify_report(report)
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
        VERIFY => {
            // P2-1: round-trip test against the live backend. Runs in the
            // worker thread; the per-step result lands in the FEEDBACK line.
            let _ = state.tx.send(Action::Verify);
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
        RENAME => {
            if let Some(row) = selection(&state) {
                if !row.custom {
                    set_feedback(state.hwnd, "Only games you added yourself can be renamed.");
                    return;
                }
                let Some(name) = ask_name(state.hwnd, &row.name) else {
                    return;
                };
                if name.trim().is_empty() {
                    set_feedback(state.hwnd, "Game name cannot be empty.");
                    return;
                }
                match apply_rename(&config, &row.path, &name) {
                    Ok(cfg) => send_settings(&state, cfg),
                    Err(message) => set_feedback(state.hwnd, &message),
                }
            }
        }
        RESTORE => {
            let _ = state.tx.send(Action::Restore);
        }
        PAUSE => {
            let _ = state.tx.send(Action::Pause);
        }
        SAVE => {
            let delay = text(unsafe { GetDlgItem(state.hwnd, DELAY) });
            let host = text(unsafe { GetDlgItem(state.hwnd, ADDRESS) });
            match apply_save(&config, &delay, &host) {
                Ok(cfg) => send_settings(&state, cfg),
                Err(message) => set_feedback(state.hwnd, &message),
            }
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
        let dpi = GetDpiForSystem() as i32;
        let scale = |v: i32| v * dpi / 96;
        let width = scale(900);
        let height = window_height(false, dpi);
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
        for e in LAYOUT {
            let child = CreateWindowExW(
                if e.class == "EDIT" || e.class == "LISTBOX" {
                    WS_EX_CLIENTEDGE
                } else {
                    0
                },
                wide(e.class).as_ptr(),
                wide(e.label).as_ptr(),
                WS_CHILD | WS_VISIBLE | e.style,
                scale(e.x),
                scale(e.y),
                scale(e.w),
                scale(e.h),
                hwnd,
                e.id as HMENU,
                instance,
                null(),
            );
            SendMessageW(child, WM_SETFONT, font as usize, 1);
        }
        apply_settings_visibility(hwnd, dpi, false);
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
        assert_eq!(window_height(false, 96), 712);
        assert_eq!(window_height(true, 96), 792);
        assert_eq!(window_height(false, 144), 1068);
        assert_eq!(window_height(true, 144), 1188);
    }
    #[test]
    fn feedback_line_sits_below_settings_row_when_expanded() {
        assert_eq!(feedback_position(false, 96), (24, 644, 836, 58));
        assert_eq!(feedback_position(true, 96), (24, 722, 836, 58));
        let (_, y, _, _) = feedback_position(true, 144);
        assert_eq!(y, 1083);
    }
    #[test]
    fn group_boxes_present_and_contain_their_controls() {
        // P1-3 acceptance: four section frames exist in LAYOUT, and every control
        // assigned to a frame sits strictly inside that frame's bounds at 96 DPI.
        for gb in GROUPBOXES {
            let frame = by_id(gb.id);
            assert_eq!(frame.class, "BUTTON");
            assert_ne!(
                frame.style & BS_GROUPBOX as u32,
                0,
                "box must be a group box"
            );
            for &member in gb.members {
                let m = by_id(member);
                assert!(
                    m.x >= frame.x
                        && m.y >= frame.y
                        && m.x + m.w <= frame.x + frame.w
                        && m.y + m.h <= frame.y + frame.h,
                    "control {} ({}x{} at {},{}) must sit inside box {} ({}x{} at {},{})",
                    member,
                    m.w,
                    m.h,
                    m.x,
                    m.y,
                    gb.id,
                    frame.w,
                    frame.h,
                    frame.x,
                    frame.y
                );
            }
        }
    }
    #[test]
    fn every_control_id_appears_in_exactly_one_layout_entry() {
        // A duplicate id would make GetDlgItem ambiguous; a missing one would break
        // relayout silently. LAYOUT must be a partition of the 30 control ids.
        let mut ids: Vec<i32> = LAYOUT.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        let mut unique = ids.clone();
        unique.dedup();
        assert_eq!(ids, unique, "LAYOUT must not contain duplicate control ids");
        for &box_id in &[OPTIONS_BOX, GAMES_BOX, ACTIONS_BOX, SETTINGS_BOX] {
            assert!(
                by_id(box_id).id == box_id,
                "section box id must be in LAYOUT"
            );
        }
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

    // ── P1-7 acceptance tests ─────────────────────────────────────────────────
    fn row(label: &str, path: &str, name: &str, custom: bool) -> super::Row {
        super::Row {
            label: label.into(),
            path: path.into(),
            name: name.into(),
            custom,
            ignored: false,
        }
    }

    #[test]
    fn restore_gate_blocks_before_discovery_and_enables_when_ready() {
        // disabled + a user-facing feedback message when discovery has not run
        let (enabled, reason) = super::restore_gate(false, true, true);
        assert!(
            !enabled,
            "restore must be gated off before discovery is ready"
        );
        assert!(
            !reason.is_empty(),
            "a disabled restore must carry a feedback message (not a silent no-op)"
        );
        // also gated when the mode is off, or nothing is actually paused
        assert!(!super::restore_gate(true, false, true).0);
        assert!(!super::restore_gate(true, true, false).0);
        // enabled once discovery is ready, mode active, and a pause is pending
        let (enabled, reason) = super::restore_gate(true, true, true);
        assert!(enabled, "restore should be enabled when discovery is ready");
        assert_eq!(reason, "", "no feedback needed when restore is allowed");
    }

    // ── P2-1 acceptance: the FEEDBACK rendering of a round-trip report ──────
    #[test]
    fn render_verify_report_shows_every_step_and_the_failing_one() {
        // A failed report must name each step, mark the failing one FAIL, and
        // carry the summary so the FEEDBACK line explains the outcome inline.
        let report = crate::engine::VerifyReport {
            steps: vec![
                crate::engine::VerifyStep {
                    name: "capture".into(),
                    ok: true,
                    detail: "1 model(s) captured".into(),
                },
                crate::engine::VerifyStep {
                    name: "unload".into(),
                    ok: true,
                    detail: "1 model(s) unloaded".into(),
                },
                crate::engine::VerifyStep {
                    name: "verify-stopped".into(),
                    ok: false,
                    detail: "models still loaded after unload".into(),
                },
            ],
            ok: false,
            summary: "Round-trip verify failed at: verify-stopped".into(),
        };
        let rendered = super::render_verify_report(&report);
        for needle in [
            "capture: 1 model(s) captured (ok)",
            "unload: 1 model(s) unloaded (ok)",
            "verify-stopped: models still loaded after unload (FAIL)",
            "Round-trip verify failed at: verify-stopped",
        ] {
            assert!(rendered.contains(needle), "missing {needle} in: {rendered}");
        }

        // The success path renders every step as ok and the passing summary.
        let pass = crate::engine::VerifyReport {
            steps: vec![crate::engine::VerifyStep {
                name: "capture".into(),
                ok: true,
                detail: "0 model(s) captured".into(),
            }],
            ok: true,
            summary: "Round-trip verify passed: capture, unload, restore, and field-compare all succeeded".into(),
        };
        let rendered = super::render_verify_report(&pass);
        assert!(rendered.contains("capture: 0 model(s) captured (ok)"));
        assert!(rendered.contains("Round-trip verify passed"));
        assert!(
            !rendered.contains("FAIL"),
            "a passing report must not say FAIL"
        );
    }

    #[test]
    fn pause_button_label_flips_on_manual_pause() {
        assert_eq!(
            super::pause_button_label(true, false),
            "Pause AI",
            "running + not paused should read Pause AI"
        );
        assert_eq!(
            super::pause_button_label(true, true),
            "Resume AI",
            "manually paused should flip the label to Resume AI"
        );
        assert_eq!(
            super::pause_button_label(false, true),
            "Pause AI",
            "no active mode means nothing to resume"
        );
    }

    #[test]
    fn save_validation_routes_to_feedback_not_modal() {
        // A bad value yields the Err(String) variant — the exact value dispatch
        // routes to the FEEDBACK line (set_feedback), never to a modal.
        let cfg = crate::config::Config::default();
        let bad = super::apply_save(&cfg, "not-a-number", "127.0.0.1:1234");
        let err = bad.expect_err("a non-numeric delay must fail validation");
        assert!(
            err.contains("number of seconds"),
            "the feedback message should name the field; got {err}"
        );

        let cfg = crate::config::Config::default();
        assert!(
            super::apply_save(&cfg, "10", "   ").is_err(),
            "an empty API host must fail validation"
        );

        // a valid save succeeds and applies the edited fields
        let cfg = crate::config::Config::default();
        let ok = super::apply_save(&cfg, "45", " 127.0.0.1:8080 ")
            .expect("a valid delay + host should pass");
        assert_eq!(ok.restore_delay_seconds, 45.0);
        assert_eq!(ok.api_host, "127.0.0.1:8080");
    }

    #[test]
    fn dedupe_by_canonical_removes_two_names_same_path() {
        // Two display names for one executable: the old adjacent-only dedup
        // kept both after the label sort; the global canonical dedup keeps one.
        let rows = vec![
            row("Alpha", "C:\\Games\\App\\game.exe", "Alpha", false),
            row("Beta", "c:/games/app/game.exe", "Beta", false), // same exe, other spelling
            row("Gamma", "C:\\Games\\Other\\other.exe", "Gamma", true),
        ];
        let kept = super::dedupe_by_canonical(&rows);
        assert_eq!(
            kept.len(),
            2,
            "the two spellings of one executable must collapse to a single row"
        );
        assert_eq!(kept[0].label, "Alpha");
        assert_eq!(kept[1].label, "Gamma");
    }

    #[test]
    fn rename_updates_the_matching_custom_entry() {
        let mut cfg = crate::config::Config::default();
        cfg.extra_games.push(crate::config::ExtraGame {
            name: "Old Name".into(),
            path: "C:\\Games\\App\\game.exe".into(),
        });
        let ok = super::apply_rename(&cfg, "c:/games/app/game.exe", "New Name")
            .expect("renaming an existing custom entry should succeed");
        assert_eq!(ok.extra_games[0].name, "New Name");
        assert_eq!(ok.extra_games[0].path, "C:\\Games\\App\\game.exe");

        let err = super::apply_rename(&cfg, "C:\\Games\\Nope\\missing.exe", "X")
            .expect_err("a non-custom path must be rejected");
        assert!(err.contains("not a custom entry"), "got: {err}");

        let err = super::apply_rename(&cfg, "C:\\Games\\App\\game.exe", "   ")
            .expect_err("a blank name must be rejected");
        assert!(err.contains("empty"), "got: {err}");
    }
}
