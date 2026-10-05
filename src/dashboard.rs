//! Native, on-demand dashboard. Win32 calls never retain state borrows.
use crate::{
    app::{Action, Shared, SharedState},
    commands::Outcome,
    config::{Config, ExtraGame},
    control::CoreCommand,
    discovery::canonical,
    tray,
    ui_commands::Command,
    wide,
};
use std::{
    cell::RefCell,
    collections::HashSet,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{LibraryLoader::GetModuleHandleW, SystemServices::SS_OWNERDRAW},
    UI::{
        Controls::Dialogs::*, Controls::*, HiDpi::*, Input::KeyboardAndMouse::*,
        WindowsAndMessaging::*,
    },
};

const STATUS: i32 = 100;
static RUNNING_REQUESTED: AtomicBool = AtomicBool::new(false);
pub fn needs_running_apps() -> bool {
    RUNNING_REQUESTED.load(Ordering::Relaxed)
}
const AUTO: i32 = Command::Automation as i32;
const STARTUP: i32 = Command::Startup as i32;
const NAVIGATION: i32 = 103;
const RUNNING_GAMES: i32 = 132;
const HELP: i32 = 133;
const REFRESH: i32 = Command::Refresh as i32;
const ADD: i32 = 107;
const SEARCH: i32 = 108;
const LIST: i32 = 109;
const DETAILS: i32 = 110;
const TOGGLE: i32 = 111;
const REMOVE: i32 = 112;
const RESUME: i32 = Command::Resume as i32;
const PAUSE: i32 = Command::Pause as i32;
const DELAY: i32 = 115;
const ADDRESS: i32 = 116;
const SAVE: i32 = 117;
const CLI: i32 = 118;
const FEEDBACK: i32 = 119;
const SETTINGS: i32 = 120;
const RENAME: i32 = 121;
const VERIFY: i32 = Command::Verify as i32;
const OPEN_FOLDER: i32 = Command::OpenFolder as i32;
const QUIT: i32 = Command::Quit as i32;
const PROVIDER_DETAIL: i32 = 137;
const LM_ENABLED: i32 = 138;
const NOTIFICATIONS: i32 = 144;
const SOUND: i32 = 145;
const OLLAMA_ENABLED: i32 = 146;
const OLLAMA_ADDRESS: i32 = 147;
const OLLAMA_ADDRESS_LABEL: i32 = 148;
const OLLAMA_SAVE: i32 = 149;
const CONTRIBUTE: i32 = 150;
const DOCTOR: i32 = Command::Doctor as i32;
const OLLAMA_DISCLOSURE: &str = "Experimental. Not tested with a live Ollama installation.\r\n\r\nOnly local GGUF completion models with supported context and finite expiry are eligible. GamePause restores verified identity/context and the remaining observed residency deadline. It cannot preserve all load options, parallelism, conversations or KV cache. Embedding/cloud models and unknown settings are refused.\r\n\r\nThe user-owned service stays running. GamePause does not download models or fight later client reloads.\r\n\r\nEnable experimental Ollama control?";
const DELAY_LABEL: i32 = 122;
const ADDRESS_LABEL: i32 = 123;
const TITLE: i32 = 130;
const SEARCH_LABEL: i32 = 131;
const OPTIONS_BOX: i32 = 140;
const GAMES_BOX: i32 = 141;
const ACTIONS_BOX: i32 = 142;
const SETTINGS_BOX: i32 = 143;
const WINDOW_STYLE: u32 = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
fn dashboard_font(dpi: i32) -> HFONT {
    unsafe {
        CreateFontW(
            -scale(16, dpi),
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
        )
    }
}
fn outer_size(settings_visible: bool, dpi: i32) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: scale(884, dpi),
        bottom: window_height(settings_visible, dpi),
    };
    unsafe {
        AdjustWindowRectExForDpi(&mut rect, WINDOW_STYLE, 0, 0, dpi as u32);
    }
    (rect.right - rect.left, rect.bottom - rect.top)
}
/// Scale a 96-DPI design coordinate to the target DPI.
#[must_use]
pub fn scale(value: i32, dpi: i32) -> i32 {
    value * dpi / 96
}

/// Dashboard height for the settings-panel visibility state, at the target DPI.
/// Client height; native nonclient margins are added separately.
#[must_use]
pub fn window_height(settings_visible: bool, dpi: i32) -> i32 {
    scale(if settings_visible { 1244 } else { 804 }, dpi)
}

/// Feedback-line geometry `(x, y, w, h)` for the settings-panel visibility state.
#[must_use]
pub fn footer_position(settings_visible: bool, dpi: i32) -> (i32, i32, i32, i32) {
    (
        scale(24, dpi),
        scale(if settings_visible { 1174 } else { 734 }, dpi),
        scale(836, dpi),
        scale(26, dpi),
    )
}

/// Classic system/contrast colors. This does not opt native client controls into app dark mode.
#[must_use]
pub fn ctlcolor_index(message: u32) -> SYS_COLOR_INDEX {
    if matches!(
        message,
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX
    ) {
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
        members: &[AUTO, SETTINGS],
    },
    GroupBox {
        id: GAMES_BOX,
        members: &[
            NAVIGATION,
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
        members: &[TOGGLE, REMOVE, PAUSE, RESUME, QUIT, RENAME],
    },
    GroupBox {
        id: SETTINGS_BOX,
        members: &[
            DELAY_LABEL,
            DELAY,
            ADDRESS_LABEL,
            ADDRESS,
            SAVE,
            CLI,
            STARTUP,
            VERIFY,
            OPEN_FOLDER,
            PROVIDER_DETAIL,
            LM_ENABLED,
            NOTIFICATIONS,
            SOUND,
            OLLAMA_ENABLED,
            OLLAMA_ADDRESS,
            OLLAMA_ADDRESS_LABEL,
            OLLAMA_SAVE,
            CONTRIBUTE,
            DOCTOR,
        ],
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
const fn control(
    id: i32,
    class: &'static str,
    label: &'static str,
    style: u32,
    rect: [i32; 4],
) -> Layout {
    Layout {
        id,
        class,
        label,
        style,
        x: rect[0],
        y: rect[1],
        w: rect[2],
        h: rect[3],
    }
}
const READABLE: u32 = ES_MULTILINE as u32 | ES_READONLY as u32 | ES_AUTOVSCROLL as u32 | WS_TABSTOP;
const ACTION_BUTTON: u32 = WS_TABSTOP | BS_NOTIFY as u32;
const STATE_ACCENT: i32 = 152;
const APPEARANCE: i32 = 153;
const APPEARANCE_LABEL: i32 = 154;
const LAYOUT: [Layout; 46] = [
    control(
        OPTIONS_BOX,
        "BUTTON",
        "Automatic pausing",
        BS_GROUPBOX as u32,
        [16, 262, 852, 54],
    ),
    control(
        GAMES_BOX,
        "BUTTON",
        "Game recognition",
        BS_GROUPBOX as u32,
        [16, 322, 852, 298],
    ),
    control(
        ACTIONS_BOX,
        "BUTTON",
        "Actions",
        BS_GROUPBOX as u32,
        [16, 628, 852, 98],
    ),
    control(
        SETTINGS_BOX,
        "BUTTON",
        "Advanced settings",
        BS_GROUPBOX as u32,
        [16, 734, 852, 432],
    ),
    control(
        APPEARANCE_LABEL,
        "STATIC",
        "Appearance",
        0,
        [24, 1116, 332, 28],
    ),
    control(
        APPEARANCE,
        "COMBOBOX",
        "",
        WS_TABSTOP | CBS_DROPDOWNLIST as u32,
        [376, 1114, 218, 28],
    ),
    control(STATE_ACCENT, "STATIC", "", SS_OWNERDRAW, [16, 4, 852, 3]),
    control(TITLE, "STATIC", "GamePause", 0, [24, 734, 836, 26]),
    control(
        RUNNING_GAMES,
        "EDIT",
        "Games: waiting for detection.",
        READABLE,
        [24, 16, 836, 54],
    ),
    control(
        STATUS,
        "EDIT",
        "LM Studio: waiting for detection.",
        READABLE,
        [24, 78, 836, 102],
    ),
    control(
        HELP,
        "EDIT",
        "Tab moves between controls. Focus an action for its description.",
        READABLE,
        [24, 186, 836, 34],
    ),
    control(
        AUTO,
        "BUTTON",
        "Automatically pause AI while gaming",
        BS_AUTOCHECKBOX as u32 | WS_TABSTOP | BS_NOTIFY as u32,
        [24, 282, 430, 32],
    ),
    control(
        STARTUP,
        "BUTTON",
        "Start when I sign in to Windows",
        BS_AUTOCHECKBOX as u32 | WS_TABSTOP | BS_NOTIFY as u32,
        [24, 874, 430, 32],
    ),
    control(
        NAVIGATION,
        "SysTabControl32",
        "",
        WS_TABSTOP,
        [24, 346, 380, 34],
    ),
    control(
        SETTINGS,
        "BUTTON",
        "Advanced settings",
        BS_AUTOCHECKBOX as u32 | WS_TABSTOP | BS_NOTIFY as u32,
        [478, 282, 382, 32],
    ),
    control(
        REFRESH,
        "BUTTON",
        "Refresh games",
        ACTION_BUTTON,
        [600, 346, 132, 32],
    ),
    control(
        ADD,
        "BUTTON",
        "Add game...",
        ACTION_BUTTON,
        [740, 346, 120, 32],
    ),
    control(SEARCH_LABEL, "STATIC", "Search", 0, [24, 388, 62, 25]),
    control(
        SEARCH,
        "EDIT",
        "",
        WS_TABSTOP | ES_AUTOHSCROLL as u32,
        [90, 386, 770, 28],
    ),
    control(
        LIST,
        "LISTBOX",
        "",
        WS_TABSTOP
            | WS_VSCROLL
            | WS_HSCROLL
            | LBS_NOTIFY as u32
            | LBS_OWNERDRAWFIXED as u32
            | LBS_HASSTRINGS as u32,
        [24, 422, 836, 108],
    ),
    control(DETAILS, "RICHEDIT50W", "", READABLE, [24, 540, 836, 70]),
    control(
        TOGGLE,
        "BUTTON",
        "Ignore selected",
        ACTION_BUTTON,
        [24, 650, 184, 32],
    ),
    control(
        REMOVE,
        "BUTTON",
        "Remove selected",
        ACTION_BUTTON,
        [218, 650, 152, 32],
    ),
    control(
        RENAME,
        "BUTTON",
        "Rename",
        ACTION_BUTTON,
        [380, 650, 100, 32],
    ),
    control(
        PAUSE,
        "BUTTON",
        "Pause AI",
        ACTION_BUTTON,
        [490, 650, 150, 32],
    ),
    control(
        RESUME,
        "BUTTON",
        "Resume AI",
        ACTION_BUTTON,
        [650, 650, 210, 32],
    ),
    control(
        VERIFY,
        "BUTTON",
        "Test round-trip",
        ACTION_BUTTON,
        [24, 914, 176, 32],
    ),
    control(
        DELAY_LABEL,
        "STATIC",
        "Restore after (seconds)",
        0,
        [24, 756, 180, 25],
    ),
    control(
        DELAY,
        "EDIT",
        "30",
        WS_TABSTOP | ES_AUTOHSCROLL as u32,
        [206, 754, 65, 28],
    ),
    control(ADDRESS_LABEL, "STATIC", "Local API", 0, [292, 756, 80, 25]),
    control(
        ADDRESS,
        "EDIT",
        "127.0.0.1:1234",
        WS_TABSTOP | ES_AUTOHSCROLL as u32,
        [376, 754, 218, 28],
    ),
    control(
        SAVE,
        "BUTTON",
        "Save settings",
        ACTION_BUTTON,
        [604, 754, 128, 32],
    ),
    control(
        CLI,
        "BUTTON",
        "Locate lms...",
        ACTION_BUTTON,
        [742, 754, 118, 32],
    ),
    control(
        OPEN_FOLDER,
        "BUTTON",
        "Open logs and status folder",
        ACTION_BUTTON,
        [210, 914, 282, 32],
    ),
    control(QUIT, "BUTTON", "Quit", ACTION_BUTTON, [742, 690, 118, 32]),
    control(PROVIDER_DETAIL, "EDIT", "", READABLE, [24, 794, 836, 70]),
    control(
        LM_ENABLED,
        "BUTTON",
        "Enable LM Studio control",
        BS_AUTOCHECKBOX as u32 | WS_TABSTOP | BS_NOTIFY as u32,
        [478, 874, 382, 32],
    ),
    control(
        NOTIFICATIONS,
        "BUTTON",
        "Windows notifications",
        BS_AUTOCHECKBOX as u32 | WS_TABSTOP | BS_NOTIFY as u32,
        [24, 954, 430, 32],
    ),
    control(
        SOUND,
        "BUTTON",
        "Sound for notifications",
        BS_AUTOCHECKBOX as u32 | WS_TABSTOP | BS_NOTIFY as u32,
        [478, 954, 382, 32],
    ),
    control(FEEDBACK, "EDIT", "", READABLE, [24, 222, 836, 34]),
    control(
        OLLAMA_ENABLED,
        "BUTTON",
        "Enable experimental Ollama (not live-tested)",
        BS_AUTOCHECKBOX as u32 | WS_TABSTOP | BS_NOTIFY as u32,
        [24, 994, 836, 32],
    ),
    control(
        OLLAMA_ADDRESS_LABEL,
        "STATIC",
        "Ollama loopback endpoint",
        0,
        [24, 1034, 332, 28],
    ),
    control(
        OLLAMA_ADDRESS,
        "EDIT",
        "127.0.0.1:11434",
        WS_TABSTOP | ES_AUTOHSCROLL as u32,
        [376, 1034, 218, 28],
    ),
    control(
        OLLAMA_SAVE,
        "BUTTON",
        "Save Ollama endpoint",
        ACTION_BUTTON,
        [604, 1034, 256, 32],
    ),
    control(
        CONTRIBUTE,
        "BUTTON",
        "Contribute Ollama fixes or live evidence",
        ACTION_BUTTON,
        [24, 1074, 570, 32],
    ),
    control(
        DOCTOR,
        "BUTTON",
        "Read-only diagnostics",
        ACTION_BUTTON,
        [604, 1074, 256, 32],
    ),
];
#[cfg(test)]
pub(crate) fn shared_command_ids(advanced: bool) -> Vec<i32> {
    LAYOUT
        .iter()
        .filter_map(|control| {
            Command::from_id(control.id)
                .filter(|command| command.visible(advanced))
                .map(|command| command as i32)
        })
        .collect()
}

/// Settings-row control ids, toggled together and repositioned by `relayout`.
const SETTINGS_ROW: [i32; 22] = [
    APPEARANCE,
    APPEARANCE_LABEL,
    DOCTOR,
    OLLAMA_ENABLED,
    OLLAMA_ADDRESS,
    OLLAMA_ADDRESS_LABEL,
    OLLAMA_SAVE,
    CONTRIBUTE,
    NOTIFICATIONS,
    SOUND,
    STARTUP,
    VERIFY,
    OPEN_FOLDER,
    PROVIDER_DETAIL,
    LM_ENABLED,
    DELAY,
    ADDRESS,
    SAVE,
    CLI,
    DELAY_LABEL,
    ADDRESS_LABEL,
    SETTINGS_BOX,
];

#[cfg(test)]
fn is_settings_row(id: i32) -> bool {
    SETTINGS_ROW.contains(&id)
}

/// Show/hide settings controls. Positioning uses the current viewport separately.
fn apply_settings_visibility(hwnd: HWND, visible: bool) {
    unsafe {
        for id in SETTINGS_ROW {
            let child = GetDlgItem(hwnd, id);
            if !child.is_null() {
                ShowWindow(child, if visible { SW_SHOW } else { SW_HIDE });
            }
        }
    }
}

/// Pixel offsets into the fixed logical canvas. Controls keep their measured
/// captions even when a work area cannot fit the whole canvas.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Viewport {
    x: i32,
    y: i32,
}
impl Viewport {
    fn bounded(self, content: (i32, i32), client: (i32, i32)) -> Self {
        Self {
            x: self.x.clamp(0, (content.0 - client.0).max(0)),
            y: self.y.clamp(0, (content.1 - client.1).max(0)),
        }
    }
    fn reveal(self, rect: (i32, i32, i32, i32), client: (i32, i32)) -> Self {
        fn axis(offset: i32, start: i32, size: i32, page: i32) -> i32 {
            if start < offset || size > page {
                start
            } else if start + size > offset + page {
                start + size - page
            } else {
                offset
            }
        }
        Self {
            x: axis(self.x, rect.0, rect.2, client.0),
            y: axis(self.y, rect.1, rect.3, client.1),
        }
    }
}

fn needed_scrollbars(content: (i32, i32), available: (i32, i32), bar: (i32, i32)) -> (bool, bool) {
    let mut horizontal = content.0 > available.0;
    let mut vertical = content.1 > available.1;
    // One required bar can make the other axis overflow.
    for _ in 0..2 {
        horizontal |= content.0 > available.0 - if vertical { bar.0 } else { 0 };
        vertical |= content.1 > available.1 - if horizontal { bar.1 } else { 0 };
    }
    (horizontal, vertical)
}

// Native EDIT retains wrapping, keyboard scrolling and selection. Show a bar
// only when its fully wrapped text exceeds the available height.
fn update_text_scrollbar(hwnd: HWND) {
    unsafe {
        if hwnd.is_null() || GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & ES_READONLY as u32 == 0 {
            return;
        }
        ShowScrollBar(hwnd, SB_VERT, 0);
        let mut rect: RECT = std::mem::zeroed();
        SendMessageW(hwnd, EM_GETRECT, 0, &mut rect as *mut RECT as isize);
        let dc = GetDC(hwnd);
        if dc.is_null() {
            return;
        }
        let font = SendMessageW(hwnd, WM_GETFONT, 0, 0) as HGDIOBJ;
        let old = SelectObject(dc, font);
        let mut metrics: TEXTMETRICW = std::mem::zeroed();
        let measured = GetTextMetricsW(dc, &mut metrics) != 0;
        SelectObject(dc, old);
        ReleaseDC(hwnd, dc);
        let lines = SendMessageW(hwnd, EM_GETLINECOUNT, 0, 0) as i32;
        if measured && lines * metrics.tmHeight > rect.bottom - rect.top {
            ShowScrollBar(hwnd, SB_VERT, 1);
        }
    }
}

fn position_controls() {
    let Some(state) = snapshot() else { return };
    if state.positioning {
        return;
    }
    STATE.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.positioning = true;
        }
    });
    unsafe {
        let dpi = GetDpiForWindow(state.hwnd) as i32;
        let content = (scale(884, dpi), window_height(state.settings_visible, dpi));
        // Measure the space without bars, then solve both axes together.
        // Changing nonclient bars can dispatch WM_SIZE; positioning guards reentry.
        let mut client: RECT = std::mem::zeroed();
        GetClientRect(state.hwnd, &mut client);
        let style = GetWindowLongPtrW(state.hwnd, GWL_STYLE) as u32;
        let bar = (
            GetSystemMetricsForDpi(SM_CXVSCROLL, dpi as u32),
            GetSystemMetricsForDpi(SM_CYHSCROLL, dpi as u32),
        );
        let available = (
            client.right + if style & WS_VSCROLL != 0 { bar.0 } else { 0 },
            client.bottom + if style & WS_HSCROLL != 0 { bar.1 } else { 0 },
        );
        let (horizontal, vertical) = needed_scrollbars(content, available, bar);
        ShowScrollBar(state.hwnd, SB_HORZ, i32::from(horizontal));
        ShowScrollBar(state.hwnd, SB_VERT, i32::from(vertical));
        GetClientRect(state.hwnd, &mut client);
        let page = (client.right.max(0), client.bottom.max(0));
        let viewport = state.viewport.bounded(content, page);
        STATE.with(|s| {
            if let Some(s) = s.borrow_mut().as_mut() {
                s.viewport = viewport;
            }
        });
        for (bar, extent, size, pos) in [
            (SB_HORZ, content.0, page.0, viewport.x),
            (SB_VERT, content.1, page.1, viewport.y),
        ] {
            let info = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
                nMin: 0,
                nMax: extent - 1,
                nPage: size as u32,
                nPos: pos,
                nTrackPos: 0,
            };
            SetScrollInfo(state.hwnd, bar, &info, 1);
        }
        let mut positions = Vec::with_capacity(LAYOUT.len());
        let mut resized_text = Vec::new();
        for e in LAYOUT {
            let (x, y, w, h) = if e.id == TITLE {
                footer_position(state.settings_visible, dpi)
            } else {
                (
                    scale(e.x, dpi),
                    scale(e.y, dpi),
                    scale(e.w, dpi),
                    scale(e.h, dpi),
                )
            };
            let child = GetDlgItem(state.hwnd, e.id);
            if child.is_null() {
                continue;
            }
            let mut old: RECT = std::mem::zeroed();
            GetWindowRect(child, &mut old);
            if matches!(e.class, "EDIT" | "RICHEDIT50W")
                && e.style & ES_READONLY as u32 != 0
                && (old.right - old.left != w || old.bottom - old.top != h)
            {
                resized_text.push(child);
            }
            positions.push((child, x - viewport.x, y - viewport.y, w, h));
        }
        let mut batch = BeginDeferWindowPos(positions.len() as i32);
        for &(child, x, y, w, h) in &positions {
            if batch.is_null() {
                break;
            }
            batch = DeferWindowPos(
                batch,
                child,
                null_mut(),
                x,
                y,
                w,
                h,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW,
            );
        }
        if batch.is_null() || EndDeferWindowPos(batch) == 0 {
            // A failed batch discards earlier deferred moves. Apply all moves.
            for &(child, x, y, w, h) in &positions {
                SetWindowPos(
                    child,
                    null_mut(),
                    x,
                    y,
                    w,
                    h,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW,
                );
            }
        }
        for child in resized_text {
            update_text_scrollbar(child);
        }
        RedrawWindow(
            state.hwnd,
            null(),
            null_mut(),
            RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
        );
    }
    STATE.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.positioning = false;
        }
    });
}

fn reveal_focus() {
    let Some(state) = snapshot() else { return };
    unsafe {
        let focus = GetFocus();
        if IsChild(state.hwnd, focus) == 0 {
            return;
        }
        let mut rect: RECT = std::mem::zeroed();
        GetWindowRect(focus, &mut rect);
        MapWindowPoints(
            null_mut(),
            state.hwnd,
            &mut rect as *mut RECT as *mut POINT,
            2,
        );
        let mut client: RECT = std::mem::zeroed();
        GetClientRect(state.hwnd, &mut client);
        let viewport = state.viewport.reveal(
            (
                rect.left + state.viewport.x,
                rect.top + state.viewport.y,
                rect.right - rect.left,
                rect.bottom - rect.top,
            ),
            (client.right, client.bottom),
        );
        if viewport != state.viewport {
            STATE.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.viewport = viewport;
                }
            });
            position_controls();
        }
    }
}

fn scroll_viewport(horizontal: bool, request: i32) {
    let Some(state) = snapshot() else { return };
    unsafe {
        let bar = if horizontal { SB_HORZ } else { SB_VERT };
        let mut info = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_ALL,
            ..std::mem::zeroed()
        };
        if GetScrollInfo(state.hwnd, bar, &mut info) == 0 {
            return;
        }
        let line = scale(24, GetDpiForWindow(state.hwnd) as i32);
        let pos = match request {
            SB_LINEUP => info.nPos - line,
            SB_LINEDOWN => info.nPos + line,
            SB_PAGEUP => info.nPos - info.nPage as i32,
            SB_PAGEDOWN => info.nPos + info.nPage as i32,
            SB_THUMBTRACK | SB_THUMBPOSITION => info.nTrackPos,
            SB_TOP => 0,
            SB_BOTTOM => info.nMax,
            _ => return,
        };
        STATE.with(|s| {
            if let Some(s) = s.borrow_mut().as_mut() {
                if horizontal {
                    s.viewport.x = pos;
                } else {
                    s.viewport.y = pos;
                }
            }
        });
        position_controls();
    }
}

/// Update cached fonts and control geometry without changing the user's window
/// dimensions or overriding the rectangle supplied with WM_DPICHANGED.
fn relayout(hwnd: HWND, dpi: i32, settings_visible: bool) {
    unsafe {
        let cached = snapshot().and_then(|s| s.fonts.get(&dpi).copied());
        let font = cached.unwrap_or_else(|| dashboard_font(dpi));
        if font.is_null() {
            return;
        }
        STATE.with(|state| {
            if let Some(s) = state.borrow_mut().as_mut() {
                s.font = font;
                s.fonts.insert(dpi, font);
            }
        });
        // Controls may retain selected fonts in cached DCs until destruction.
        // Keep one owned font per DPI and release them after all children die.
        for e in LAYOUT {
            SendMessageW(GetDlgItem(hwnd, e.id), WM_SETFONT, font as usize, 1);
        }
        SendMessageW(
            GetDlgItem(hwnd, LIST),
            LB_SETITEMHEIGHT,
            0,
            row_height(dpi) as isize,
        );
        apply_settings_visibility(hwnd, settings_visible);
        position_controls();
        if let Some(state) = snapshot() {
            SendMessageW(
                state.tooltips.hwnd,
                TTM_SETMAXTIPWIDTH,
                0,
                scale(480, dpi) as isize,
            );
            SendMessageW(state.tooltips.hwnd, WM_SETFONT, font as usize, 1);
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Games,
    Running,
    Ignored,
}
impl Page {
    fn tab(self) -> usize {
        match self {
            Self::Games => 0,
            Self::Running => 1,
            Self::Ignored => 2,
        }
    }
    fn from_tab(index: isize) -> Option<Self> {
        match index {
            0 => Some(Self::Games),
            1 => Some(Self::Running),
            2 => Some(Self::Ignored),
            _ => None,
        }
    }
}
fn select_page(index: isize) {
    let Some(page) = Page::from_tab(index) else {
        return;
    };
    let Some(state) = snapshot() else { return };
    RUNNING_REQUESTED.store(page == Page::Running, Ordering::Relaxed);
    STATE.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.page = page;
        }
    });
    set(unsafe { GetDlgItem(state.hwnd, SEARCH) }, "");
    refresh();
}
#[derive(Clone, PartialEq)]
struct Row {
    label: String,
    path: String,
    name: String,
    custom: bool,
    ignored: bool,
    running: bool,
}
fn selected_index(rows: &[Row], path: &str) -> Option<usize> {
    rows.iter()
        .position(|row| canonical(&row.path) == canonical(path))
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
    fonts: std::collections::BTreeMap<i32, HFONT>,
    theme: Option<std::rc::Rc<crate::theme::Theme>>,
    settings_visible: bool,
    viewport: Viewport,
    positioning: bool,
    tooltips: Tooltips,
}
#[derive(Clone)]
struct Tooltips {
    hwnd: HWND,
    // Native tooltip controls retain these pointers until destruction.
    _texts: Arc<Vec<Vec<u16>>>,
}
const DESCRIPTIONS: &[(i32, &str)] = &[
    (
        APPEARANCE,
        "Choose Follow Windows, Light or Dark. The saved choice applies to the dashboard and tray menu. High contrast always uses Windows colors.",
    ),
    (
        DOCTOR,
        "Probe enabled providers without starting services or loading/unloading models. Results are cached observations; repeat after changing settings or provider state.",
    ),
    (OLLAMA_ENABLED, OLLAMA_DISCLOSURE),
    (
        OLLAMA_ADDRESS,
        "Configured loopback endpoint only. Saving does not start or discover a service. Its unfinished recovery must finish before this route can change.",
    ),
    (
        OLLAMA_SAVE,
        "Save the Ollama endpoint independently of LM Studio settings. This does not enable Ollama control.",
    ),
    (
        CONTRIBUTE,
        "Open the project contribution page in your browser. Include exact versions and sanitized evidence; never share private journals or prompts.",
    ),
    (
        NOTIFICATIONS,
        "Save whether Windows displays completion/failure notifications. Persistent AI state remains in the dashboard and tray; fullscreen delivery is not guaranteed.",
    ),
    (
        SOUND,
        "Use one sound source: Windows notification sound when visuals are enabled, or one standalone system sound when visuals are disabled. Windows settings still apply.",
    ),
    (
        OPEN_FOLDER,
        "Open local logs and status files. This does not change AI.",
    ),
    (
        QUIT,
        "Quit GamePause. Pending recovery is retained for the next launch; restore it before uninstalling.",
    ),
    (
        LM_ENABLED,
        "Enable LM Studio control. Disabling is blocked while its own recovery is unfinished; restore its saved AI first.",
    ),
    (
        NAVIGATION,
        "Games, Running apps and Ignored are pages. Use Left/Right arrows on these tabs.",
    ),
    (
        AUTO,
        "Save whether recognized games automatically pause AI. Existing recovery is retained.",
    ),
    (
        STARTUP,
        "Save whether GamePause starts in the tray when you sign in to Windows.",
    ),
    (
        SETTINGS,
        "Save Advanced tool visibility in the dashboard and tray. Hiding tools leaves AI control and recovery unchanged.",
    ),
    (
        REFRESH,
        "Refresh launcher metadata. Completion is reported after discovery returns; repeated requests coalesce.",
    ),
    (
        ADD,
        "Choose the actual game executable to add a missed standalone game. Cancel leaves settings unchanged.",
    ),
    (
        SEARCH,
        "Filter the current page by name or path. This does not change game recognition.",
    ),
    (
        LIST,
        "Select an entry to read its path and recognition details. Arrow keys move between entries.",
    ),
    (
        TOGGLE,
        "Add a selected running app, ignore a recognized game or enable an ignored entry, depending on the page. Ignore never restores AI.",
    ),
    (
        REMOVE,
        "Remove only the selected game you added yourself, after confirmation. Launcher games use Ignore.",
    ),
    (
        RENAME,
        "Rename the selected custom game. Its executable path remains unchanged.",
    ),
    (
        PAUSE,
        "Pause AI creates a manual hold until you choose Resume AI.",
    ),
    (
        RESUME,
        "Resume captured AI immediately and release the manual hold. During gameplay, confirmation is required because models can compete for VRAM.",
    ),
    (
        VERIFY,
        "Test LM Studio capture, unload and restore on purpose. Ollama is left unchanged. Requires confirmation, no running games and no pending recovery.",
    ),
    (
        DELAY,
        "Seconds to wait after the last recognized game exits before restoring saved AI. Save settings applies the value.",
    ),
    (
        ADDRESS,
        "LM Studio's local API address. Save settings validates and applies it.",
    ),
    (
        SAVE,
        "Validate and save connection and delay settings. A failed save leaves previous settings intact.",
    ),
    (
        CLI,
        "Locate the installed lms executable. This saves a path; it does not download or install anything.",
    ),
];
fn description(id: i32, page: Page, shared: &Shared) -> String {
    let availability = shared.controls().availability();
    match id {
        PAUSE => {
            let available = if shared.manual_pause { availability.resume } else { availability.pause };
            if !available { return availability.reason.into(); }
        }
        RESUME if !availability.restore => return availability.reason.into(),
        TOGGLE => return match page {
            Page::Running => "Add the selected executable as a custom game. Choose the game, not its launcher.",
            Page::Ignored => "Enable the selected ignored entry. This changes future pause triggers; it does not restore AI.",
            Page::Games => "Ignore or enable the selected game for future automatic pausing. Pending recovery still remembers it.",
        }.into(),
        _ => (),
    }
    DESCRIPTIONS.iter().find(|(control, _)| *control == id).map(|(_, description)| (*description).into())
        .unwrap_or_else(|| "Tab moves between controls. Focus an action for its description; F1 moves to this help text.".into())
}
fn update_help(id: i32) {
    let Some(state) = snapshot() else { return };
    let Ok(shared) = state.shared.lock().map(|s| s.clone()) else {
        return;
    };
    let help = unsafe { GetDlgItem(state.hwnd, HELP) };
    let description = description(id, state.page, &shared);
    if text(help) != description {
        set(help, &description);
    }
}
fn create_tooltips(parent: HWND, instance: HINSTANCE, dpi: i32) -> Tooltips {
    let texts = Arc::new(
        DESCRIPTIONS
            .iter()
            .map(|(_, description)| wide(description))
            .collect::<Vec<_>>(),
    );
    unsafe {
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST,
            wide("tooltips_class32").as_ptr(),
            null(),
            WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX,
            0,
            0,
            0,
            0,
            parent,
            null_mut(),
            instance,
            null(),
        );
        if !hwnd.is_null() {
            // Native classic tooltip drawing honors explicit palette colors.
            // Set its visual style once, before assigning the owned DPI font.
            let empty = wide("");
            SetWindowTheme(hwnd, empty.as_ptr(), empty.as_ptr());
            SendMessageW(hwnd, TTM_SETMAXTIPWIDTH, 0, scale(480, dpi) as isize);
            for ((id, _), description) in DESCRIPTIONS.iter().zip(texts.iter()) {
                let mut tool: TTTOOLINFOW = std::mem::zeroed();
                tool.cbSize = std::mem::size_of::<TTTOOLINFOW>() as u32;
                tool.uFlags = TTF_IDISHWND | TTF_SUBCLASS;
                tool.hwnd = parent;
                tool.uId = GetDlgItem(parent, *id) as usize;
                tool.lpszText = description.as_ptr() as *mut u16;
                SendMessageW(hwnd, TTM_ADDTOOLW, 0, &tool as *const _ as isize);
            }
        }
        Tooltips {
            hwnd,
            _texts: texts,
        }
    }
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
    update_text_scrollbar(hwnd);
    if unsafe { GetDlgCtrlID(hwnd) } == DETAILS {
        format_details(hwnd, value);
    }
}
fn format_details(hwnd: HWND, value: &str) {
    let palette = snapshot()
        .and_then(|s| s.theme)
        .map(|t| t.palette)
        .unwrap_or_else(|| crate::theme::Palette::for_mode(false));
    crate::rich_text::format(hwnd, value, palette);
}
fn set_if_changed(hwnd: HWND, value: &str) {
    let current = text(hwnd);
    let unchanged = if unsafe { GetDlgCtrlID(hwnd) } == DETAILS {
        current.replace("\r\n", "\n").replace('\r', "\n")
            == value.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        current == value
    };
    if !unchanged {
        set(hwnd, value);
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
    crate::app::request_action(
        &state.shared,
        &state.tx,
        Action::Settings(Box::new(config)),
        "Save settings",
    );
}
fn send_advanced_settings(state: &WindowState, config: Config) {
    crate::app::request_action(
        &state.shared,
        &state.tx,
        Action::AdvancedSettings(Box::new(config)),
        "Advanced settings",
    );
}

/// Show a message on the FEEDBACK line (control FEEDBACK), not a modal.
/// P1-7: settings-validation and rename errors are user-correctable, so they
/// belong inline next to the field that produced them; modals are reserved
/// for hard failures (missing data dir, startup write, ...).
fn set_feedback(state: &WindowState, message: &str) {
    crate::app::local_result(&state.shared, Outcome::Failed, message);
    set(unsafe { GetDlgItem(state.hwnd, FEEDBACK) }, message);
}

thread_local! {
    static ASK_NAME_RESULT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Modal text-input dialog for the Rename affordance. Returns the entered text
/// or `None` if the user cancelled. Native failures return an error.
/// Mirrors the dashboard's own WNDCLASSW + message-loop pattern, so it compiles
/// against the same windows-sys surface without new dependencies.
fn ask_name(parent: HWND, current: &str) -> anyhow::Result<Option<String>> {
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
            anyhow::bail!("Could not register rename dialog");
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
            anyhow::bail!("Could not create rename dialog");
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
        if [edit, ok, cancel].iter().any(|control| control.is_null()) {
            DestroyWindow(hwnd);
            if !font.is_null() {
                DeleteObject(font);
            }
            anyhow::bail!("Could not create rename controls");
        }
        ASK_NAME_RESULT.with(|c| *c.borrow_mut() = None);
        let parent_enabled = IsWindowEnabled(parent) != 0;
        if parent_enabled {
            EnableWindow(parent, 0);
        }
        SetFocus(edit);
        ShowWindow(hwnd, SW_SHOW);
        let mut msg: MSG = std::mem::zeroed();
        let mut failure = None;
        while IsWindow(hwnd) != 0 {
            let status = GetMessageW(&mut msg, null_mut(), 0, 0);
            if status <= 0 {
                if status == 0 {
                    PostQuitMessage(msg.wParam as i32);
                } else {
                    failure = Some(std::io::Error::last_os_error());
                }
                ASK_NAME_RESULT.with(|c| *c.borrow_mut() = None);
                break;
            }
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if IsWindow(hwnd) != 0 {
            DestroyWindow(hwnd);
        }
        if !font.is_null() {
            DeleteObject(font);
        }
        if parent_enabled && IsWindow(parent) != 0 {
            EnableWindow(parent, 1);
            SetForegroundWindow(parent);
        }
        if let Some(error) = failure {
            return Err(error.into());
        }
    }
    Ok(ASK_NAME_RESULT.with(|c| c.borrow().clone()))
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
    out.lm_mut().map_err(|e| e.to_string())?.endpoint = host.into();
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
                    running,
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
                running: true,
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
                    .map(|g| format!("{}  |  Automatic pausing off", g.name))
                    .unwrap_or_else(|| {
                        format!(
                            "{}  |  Automatic pausing off",
                            path.rsplit(['\\', '/']).next().unwrap_or(path)
                        )
                    }),
                path: path.clone(),
                name: String::new(),
                custom: false,
                ignored: true,
                running: false,
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
/// Height of one owner-drawn listbox item in design units at the given DPI.
/// Single-line text with vertical padding.
fn row_height(dpi: i32) -> i32 {
    scale(24, dpi)
}

/// `WM_MEASUREITEM` for the games listbox. `LBS_OWNERDRAWFIXED` asks the owner
/// for each item's size before it draws; not answering leaves rows at zero height.
unsafe fn measure_list_item(lparam: LPARAM, dpi: i32) {
    unsafe {
        let item = &mut *(lparam as *mut MEASUREITEMSTRUCT);
        if item.CtlID == LIST as u32 {
            item.itemHeight = row_height(dpi) as u32;
        }
    }
}
/// Draw text-only rows. Transparent text prevents an opaque text rectangle
/// from covering selection. Preserve the supplied DC state.
unsafe fn draw_list_item(lparam: LPARAM, index: usize, font: HFONT, rows: &[Row], dpi: i32) {
    let palette = snapshot()
        .and_then(|s| s.theme.clone())
        .map(|theme| theme.palette)
        .unwrap_or_else(|| crate::theme::Palette::for_mode(false));
    unsafe {
        draw_list_item_palette(lparam, index, font, rows, dpi, palette);
    }
}
unsafe fn draw_list_item_palette(
    lparam: LPARAM,
    index: usize,
    font: HFONT,
    rows: &[Row],
    dpi: i32,
    palette: crate::theme::Palette,
) {
    unsafe {
        let item = *(lparam as *const DRAWITEMSTRUCT);
        let saved = SaveDC(item.hDC);
        if saved == 0 {
            return;
        }
        let selected = item.itemState & ODS_SELECTED != 0;
        crate::theme::fill(
            item.hDC,
            &item.rcItem,
            if selected {
                palette.selected
            } else {
                palette.background
            },
        );
        if let Some(row) = rows.get(index) {
            SetTextColor(
                item.hDC,
                if item.itemState & ODS_DISABLED != 0 {
                    palette.disabled
                } else if selected {
                    palette.selected_text
                } else {
                    palette.text
                },
            );
            SetBkMode(item.hDC, TRANSPARENT as i32);
            SelectObject(item.hDC, font);
            let mut rect = item.rcItem;
            rect.left += scale(8, dpi);
            rect.right -= scale(4, dpi);
            draw_row_text(
                item.hDC,
                &row.label,
                rect,
                palette,
                item.itemState & ODS_DISABLED != 0,
            );
        }
        if item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0 {
            DrawFocusRect(item.hDC, &item.rcItem);
        }
        if saved != 0 {
            RestoreDC(item.hDC, saved);
        }
    }
}

unsafe fn draw_row_text(
    dc: HDC,
    label: &str,
    mut rect: RECT,
    palette: crate::theme::Palette,
    disabled: bool,
) {
    unsafe {
        let base = GetTextColor(dc);
        let span = if !disabled && palette.color_words {
            crate::rich_text::pausing_word(label)
        } else {
            None
        };
        let parts = if let Some((start, end, on)) = span {
            vec![
                (&label[..start], base),
                (
                    &label[start..end],
                    if on {
                        crate::theme::on_color(palette.dark)
                    } else {
                        crate::theme::off_color(palette.dark)
                    },
                ),
                (&label[end..], base),
            ]
        } else {
            vec![(label, base)]
        };
        for (part, color) in parts {
            let value = wide(part);
            let mut size: SIZE = std::mem::zeroed();
            GetTextExtentPoint32W(dc, value.as_ptr(), (value.len() - 1) as i32, &mut size);
            SetTextColor(dc, color);
            DrawTextW(
                dc,
                value.as_ptr(),
                (value.len() - 1) as i32,
                &mut rect,
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );
            rect.left += size.cx;
            if rect.left >= rect.right {
                break;
            }
        }
        SetTextColor(dc, base);
    }
}

pub fn refresh() {
    let Some(mut state) = snapshot() else { return };
    let Ok(shared) = state.shared.lock().map(|s| s.clone()) else {
        return;
    };
    let desired_dark = crate::theme::effective_dark(shared.config.appearance);
    if state
        .theme
        .as_ref()
        .is_none_or(|theme| theme.dark != desired_dark)
    {
        set_client_theme(desired_dark);
        state = snapshot().unwrap_or(state);
    }
    let query = text(unsafe { GetDlgItem(state.hwnd, SEARCH) });
    if state.settings_visible != shared.config.advanced_settings_visible {
        let focused = unsafe { GetFocus() };
        if !shared.config.advanced_settings_visible
            && SETTINGS_ROW.contains(&unsafe { GetDlgCtrlID(focused) })
        {
            unsafe {
                let target = if shared.commands.settings_pending {
                    GetDlgItem(state.hwnd, AUTO)
                } else {
                    let checkbox = GetDlgItem(state.hwnd, SETTINGS);
                    EnableWindow(checkbox, 1);
                    checkbox
                };
                SetFocus(target);
            }
        }
        STATE.with(|s| {
            if let Some(s) = s.borrow_mut().as_mut() {
                s.settings_visible = shared.config.advanced_settings_visible;
            }
        });
        apply_settings_visibility(state.hwnd, shared.config.advanced_settings_visible);
        position_controls();
    }
    let focus = unsafe { GetFocus() };
    if !focus.is_null()
        && unsafe { IsChild(state.hwnd, focus) } != 0
        && unsafe { GetDlgCtrlID(focus) } != HELP
    {
        update_help(unsafe { GetDlgCtrlID(focus) });
    }
    let dark = state.theme.as_ref().is_some_and(|theme| theme.dark);
    let accent = crate::theme::state_accent(shared.activity, dark);
    unsafe {
        crate::theme::set_accent(state.hwnd, accent);
        for id in [STATE_ACCENT, NAVIGATION, PAUSE, RESUME] {
            crate::theme::set_accent(GetDlgItem(state.hwnd, id), accent);
        }
    }
    let updated = rows(&shared, state.page, &query);
    let summary = crate::presentation::summarize(&shared);
    let status = summary.ai_text();
    let games_control = unsafe { GetDlgItem(state.hwnd, RUNNING_GAMES) };
    if text(games_control) != summary.games {
        set(games_control, &summary.games);
    }
    if state.last_status != status {
        set(unsafe { GetDlgItem(state.hwnd, STATUS) }, &status);
    }
    unsafe {
        let appearance = GetDlgItem(state.hwnd, APPEARANCE);
        if SendMessageW(appearance, CB_GETCURSEL, 0, 0) != shared.config.appearance.index() as isize
        {
            SendMessageW(
                appearance,
                CB_SETCURSEL,
                shared.config.appearance.index(),
                0,
            );
        }
        EnableWindow(appearance, i32::from(!shared.commands.settings_pending));
        SendMessageW(
            GetDlgItem(state.hwnd, NOTIFICATIONS),
            BM_SETCHECK,
            usize::from(shared.config.notifications_enabled),
            0,
        );
        SendMessageW(
            GetDlgItem(state.hwnd, SOUND),
            BM_SETCHECK,
            usize::from(shared.config.sound_enabled),
            0,
        );
        for id in [NOTIFICATIONS, SOUND] {
            EnableWindow(
                GetDlgItem(state.hwnd, id),
                i32::from(!shared.commands.settings_pending),
            );
        }
        SendMessageW(
            GetDlgItem(state.hwnd, SETTINGS),
            BM_SETCHECK,
            usize::from(shared.config.advanced_settings_visible),
            0,
        );
        EnableWindow(
            GetDlgItem(state.hwnd, SETTINGS),
            i32::from(!shared.commands.settings_pending),
        );
        SendMessageW(
            GetDlgItem(state.hwnd, LM_ENABLED),
            BM_SETCHECK,
            usize::from(shared.config.lm_enabled()),
            0,
        );
        EnableWindow(
            GetDlgItem(state.hwnd, LM_ENABLED),
            i32::from(
                !shared.provider_pending(crate::provider::Kind::LMStudio)
                    && !shared.commands.settings_pending
                    && shared.config.lm().is_some(),
            ),
        );
        let ollama = shared
            .config
            .providers
            .iter()
            .find(|provider| provider.kind() == crate::provider::Kind::Ollama);
        SendMessageW(
            GetDlgItem(state.hwnd, OLLAMA_ENABLED),
            BM_SETCHECK,
            usize::from(ollama.is_some_and(|provider| provider.enabled())),
            0,
        );
        for id in [OLLAMA_ENABLED, OLLAMA_ADDRESS, OLLAMA_SAVE] {
            EnableWindow(
                GetDlgItem(state.hwnd, id),
                i32::from(
                    ollama.is_some()
                        && !shared.commands.settings_pending
                        && !shared.provider_pending(crate::provider::Kind::Ollama),
                ),
            );
        }
        let mut detail = format!(
            "LM Studio: {}. Endpoint: {}. Guarantee: captured settings and original server-state verification.{}\r\nOllama: {}. Endpoint: {}. Experimental. Not tested with a live Ollama installation. Limited identity/context/remaining-deadline guarantee; full load settings are not preserved.",
            if shared.config.lm_enabled() {
                "enabled"
            } else {
                "disabled"
            },
            shared.config.lm_endpoint(),
            if shared.provider_pending(crate::provider::Kind::LMStudio) {
                " Recovery pending: restore before disabling or changing the endpoint."
            } else {
                ""
            },
            if ollama.is_some_and(|provider| provider.enabled()) {
                "enabled"
            } else {
                "disabled"
            },
            ollama.map_or("not configured", |provider| provider.endpoint())
        );
        detail.push_str("\r\n");
        detail.push_str(&crate::diagnostics::render(&shared));
        if text(GetDlgItem(state.hwnd, PROVIDER_DETAIL)) != detail {
            set(GetDlgItem(state.hwnd, PROVIDER_DETAIL), &detail);
        }
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
        set(GetDlgItem(state.hwnd, PAUSE), "Pause AI");
        set(
            GetDlgItem(state.hwnd, RESUME),
            crate::restore_dialog::resume_label(
                !shared.active_games.is_empty(),
                shared.coexistence,
                shared.pending,
            ),
        );
        EnableWindow(
            GetDlgItem(state.hwnd, DOCTOR),
            i32::from(!shared.doctor_pending),
        );
        EnableWindow(
            GetDlgItem(state.hwnd, VERIFY),
            i32::from(crate::ui_commands::verify_available(&shared)),
        );
        let availability = shared.controls().availability();
        EnableWindow(GetDlgItem(state.hwnd, PAUSE), i32::from(availability.pause));
        EnableWindow(
            GetDlgItem(state.hwnd, RESUME),
            i32::from(availability.restore || availability.resume),
        );
        let navigation = GetDlgItem(state.hwnd, NAVIGATION);
        if SendMessageW(navigation, TCM_GETCURSEL, 0, 0) != state.page.tab() as isize {
            SendMessageW(navigation, TCM_SETCURSEL, state.page.tab(), 0);
        }
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
                && let Some(i) = selected_index(&updated, &path)
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
        && unsafe { GetFocus() } != unsafe { GetDlgItem(state.hwnd, OLLAMA_ADDRESS) }
    {
        set(
            unsafe { GetDlgItem(state.hwnd, DELAY) },
            &format!("{}", shared.config.restore_delay_seconds),
        );
        set(
            unsafe { GetDlgItem(state.hwnd, ADDRESS) },
            shared.config.lm_endpoint(),
        );
        set(
            unsafe { GetDlgItem(state.hwnd, OLLAMA_ADDRESS) },
            shared
                .config
                .providers
                .iter()
                .find(|provider| provider.kind() == crate::provider::Kind::Ollama)
                .map_or("", |provider| provider.endpoint()),
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
    } else if let Some(result) = &shared.commands.latest {
        result.message.clone()
    } else if let Some(feedback) = &shared.restore_feedback {
        feedback.text()
    } else if let Some(report) = &shared.verify_report {
        render_verify_report(report)
    } else if !errors.is_empty() {
        format!("Some discovery needs attention: {errors}")
    } else {
        "Games refresh automatically. New recognized games are enabled without setup.".into()
    };
    set_if_changed(unsafe { GetDlgItem(state.hwnd, FEEDBACK) }, &feedback);
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
    set_if_changed(unsafe { GetDlgItem(state.hwnd, DETAILS) }, &detail);
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
        let custom = row.as_ref().is_some_and(|row| row.custom);
        EnableWindow(GetDlgItem(state.hwnd, REMOVE), i32::from(custom));
        EnableWindow(GetDlgItem(state.hwnd, RENAME), i32::from(custom));
    }
}
fn browse(hwnd: HWND) -> anyhow::Result<Option<String>> {
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
            let error = CommDlgExtendedError();
            if error != 0 {
                anyhow::bail!("File picker failed (code {error})");
            }
            return Ok(None);
        }
        let n = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        Ok(Some(String::from_utf16_lossy(&buffer[..n])))
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
    if notification == BN_SETFOCUS || notification == EN_SETFOCUS {
        update_help(id);
        return;
    }
    let Some(state) = snapshot() else { return };
    if id == SEARCH && notification == EN_CHANGE {
        refresh();
        return;
    }
    if id == LIST && notification == LBN_SELCHANGE {
        update_selection();
        return;
    }
    if id == APPEARANCE && notification == CBN_SELCHANGE {
        let available = state
            .shared
            .lock()
            .is_ok_and(|s| s.config.advanced_settings_visible && !s.commands.settings_pending);
        if available
            && let Some(choice) = crate::config::Appearance::from_index(unsafe {
                SendMessageW(GetDlgItem(state.hwnd, APPEARANCE), CB_GETCURSEL, 0, 0)
            })
        {
            crate::app::request_action(
                &state.shared,
                &state.tx,
                Action::Appearance(choice),
                "Change appearance",
            );
        }
        refresh();
        return;
    }
    if notification != BN_CLICKED {
        return;
    }
    let Ok(mut config) = state.shared.lock().map(|s| s.config.clone()) else {
        return;
    };
    if let Some(command) = Command::from_id(id) {
        if !crate::ui_commands::allowed(&state.shared, command) {
            return;
        }
    } else if SETTINGS_ROW.contains(&id) && !config.advanced_settings_visible {
        crate::app::local_result(
            &state.shared,
            Outcome::Failed,
            "Advanced settings is hidden; stale command refused.",
        );
        return;
    }
    match id {
        OLLAMA_ENABLED => {
            let enable = checked(unsafe { GetDlgItem(state.hwnd, OLLAMA_ENABLED) });
            let confirmed_provider = config
                .providers
                .iter()
                .find(|provider| provider.kind() == crate::provider::Kind::Ollama)
                .cloned();
            let disclosure = format!(
                "Saved Ollama endpoint: {}\r\n\r\n{OLLAMA_DISCLOSURE}",
                confirmed_provider
                    .as_ref()
                    .map_or("not configured", |provider| provider.endpoint())
            );
            if enable
                && unsafe {
                    MessageBoxW(
                        state.hwnd,
                        wide(&disclosure).as_ptr(),
                        wide("Experimental Ollama opt-in").as_ptr(),
                        MB_OKCANCEL | MB_DEFBUTTON2 | MB_ICONWARNING,
                    )
                } != IDOK
            {
                crate::app::local_result(
                    &state.shared,
                    Outcome::Cancelled,
                    "Ollama opt-in cancelled; settings unchanged.",
                );
                refresh();
                return;
            }
            // A modal can process settings timers; use the current saved config.
            if let Ok(shared) = state.shared.lock() {
                if !shared.config.advanced_settings_visible
                    || shared.commands.settings_pending
                    || shared.provider_pending(crate::provider::Kind::Ollama)
                    || shared
                        .config
                        .providers
                        .iter()
                        .find(|provider| provider.kind() == crate::provider::Kind::Ollama)
                        != confirmed_provider.as_ref()
                {
                    drop(shared);
                    crate::app::local_result(
                        &state.shared,
                        Outcome::Failed,
                        "Ollama settings changed while confirming; retry after recovery or saving finishes.",
                    );
                    refresh();
                    return;
                }
                config = shared.config.clone();
            } else {
                return;
            }
            if let Some(crate::config::Provider::Ollama { enabled, .. }) = config
                .providers
                .iter_mut()
                .find(|provider| provider.kind() == crate::provider::Kind::Ollama)
            {
                *enabled = enable;
                send_advanced_settings(&state, config);
                refresh();
            }
        }
        OLLAMA_SAVE => {
            let endpoint = text(unsafe { GetDlgItem(state.hwnd, OLLAMA_ADDRESS) });
            if let Some(crate::config::Provider::Ollama {
                endpoint: saved, ..
            }) = config
                .providers
                .iter_mut()
                .find(|provider| provider.kind() == crate::provider::Kind::Ollama)
            {
                *saved = endpoint.trim().into();
                match config.validate() {
                    Ok(()) => send_advanced_settings(&state, config),
                    Err(error) => {
                        set_feedback(&state, &format!("Check Ollama endpoint: {error:#}"))
                    }
                }
            }
        }
        CONTRIBUTE => {
            let result = unsafe {
                windows_sys::Win32::UI::Shell::ShellExecuteW(
                    state.hwnd,
                    wide("open").as_ptr(),
                    wide(concat!(
                        env!("CARGO_PKG_REPOSITORY"),
                        "/blob/main/CONTRIBUTING.md"
                    ))
                    .as_ptr(),
                    null(),
                    null(),
                    SW_SHOWNORMAL,
                )
            };
            crate::app::local_result(
                &state.shared,
                if tray::shell_execute_failed(result) {
                    Outcome::Failed
                } else {
                    Outcome::Completed
                },
                if tray::shell_execute_failed(result) {
                    "Could not open contribution page."
                } else {
                    "Contribution page opened in your browser."
                },
            );
        }
        AUTO => {
            config.automation_enabled = checked(unsafe { GetDlgItem(state.hwnd, AUTO) });
            send_settings(&state, config);
        }
        STARTUP => {
            tray::request_startup(
                &state.shared,
                checked(unsafe { GetDlgItem(state.hwnd, STARTUP) }),
                &state.folder,
            );
        }
        SETTINGS => {
            let visible = checked(unsafe { GetDlgItem(state.hwnd, SETTINGS) });
            crate::app::request_action(
                &state.shared,
                &state.tx,
                Action::AdvancedVisibility(visible),
                "Advanced visibility",
            );
            refresh();
        }
        NOTIFICATIONS | SOUND => {
            crate::app::request_action(
                &state.shared,
                &state.tx,
                Action::NotificationPreferences {
                    visual: checked(unsafe { GetDlgItem(state.hwnd, NOTIFICATIONS) }),
                    sound: checked(unsafe { GetDlgItem(state.hwnd, SOUND) }),
                },
                "Notification preferences",
            );
            refresh();
        }
        OPEN_FOLDER => tray::request_folder(&state.shared, &state.folder),
        QUIT => {
            crate::app::request_quit(&state.shared, &state.tx);
        }
        LM_ENABLED => {
            if let Some(crate::config::Provider::LMStudio { enabled, .. }) = config
                .providers
                .iter_mut()
                .find(|p| p.kind() == crate::provider::Kind::LMStudio)
            {
                *enabled = checked(unsafe { GetDlgItem(state.hwnd, LM_ENABLED) });
                crate::app::request_action(
                    &state.shared,
                    &state.tx,
                    Action::AdvancedSettings(Box::new(config)),
                    "LM Studio preference",
                );
            }
        }
        REFRESH => {
            crate::app::request_action(
                &state.shared,
                &state.tx,
                Action::Refresh,
                "Discovery refresh",
            );
        }
        DOCTOR => {
            crate::app::request_action(
                &state.shared,
                &state.tx,
                Action::Doctor,
                "Read-only diagnostics",
            );
        }
        VERIFY => {
            // P2-1: round-trip test against the live backend. Runs in the
            // worker thread; the per-step result lands in the FEEDBACK line.
            if unsafe { tray::confirm_verify(state.hwnd) } {
                crate::app::request_verify(&state.shared, &state.tx);
            } else {
                crate::app::local_result(
                    &state.shared,
                    Outcome::Cancelled,
                    "Test round-trip cancelled; AI unchanged.",
                );
            }
        }
        ADD => match browse(state.hwnd) {
            Ok(Some(path)) => {
                let name = std::path::Path::new(&path)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                add_game(&mut config, path, name);
                send_settings(&state, config);
            }
            Ok(None) => crate::app::local_result(
                &state.shared,
                Outcome::Cancelled,
                "Add game cancelled; settings unchanged.",
            ),
            Err(error) => set_feedback(&state, &format!("Could not add game: {error:#}")),
        },
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
                if !row.custom {
                    set_feedback(
                        &state,
                        "Only games you added yourself can be removed. Use Ignore for launcher games.",
                    );
                    return;
                }
                let result = unsafe {
                    MessageBoxW(state.hwnd, wide(&format!("Remove custom game \"{}\"?\r\n{}\r\nOther games and pending recovery remain unchanged.", row.name, row.path)).as_ptr(), wide("Remove selected game").as_ptr(), MB_OKCANCEL | MB_DEFBUTTON2 | MB_ICONQUESTION)
                };
                if result == IDOK {
                    crate::app::request_action(
                        &state.shared,
                        &state.tx,
                        Action::RemoveCustom {
                            path: row.path,
                            name: row.name,
                        },
                        "Remove selected game",
                    );
                } else {
                    crate::app::local_result(
                        &state.shared,
                        if result == 0 {
                            Outcome::Failed
                        } else {
                            Outcome::Cancelled
                        },
                        if result == 0 {
                            "Could not open removal confirmation; settings unchanged."
                        } else {
                            "Custom removal cancelled; settings unchanged."
                        },
                    );
                }
            }
        }
        RENAME => {
            if let Some(row) = selection(&state) {
                if !row.custom {
                    set_feedback(&state, "Only games you added yourself can be renamed.");
                    return;
                }
                let name = match ask_name(state.hwnd, &row.name) {
                    Ok(Some(name)) => name,
                    Ok(None) => {
                        crate::app::local_result(
                            &state.shared,
                            Outcome::Cancelled,
                            "Rename cancelled; settings unchanged.",
                        );
                        return;
                    }
                    Err(error) => {
                        set_feedback(&state, &format!("Could not rename game: {error:#}"));
                        return;
                    }
                };
                if name.trim().is_empty() {
                    set_feedback(&state, "Game name cannot be empty.");
                    return;
                }
                match apply_rename(&config, &row.path, &name) {
                    Ok(cfg) => send_settings(&state, cfg),
                    Err(message) => set_feedback(&state, &message),
                }
            }
        }
        RESUME => {
            crate::restore_dialog::request(state.hwnd, &state.shared, &state.tx);
        }
        PAUSE => {
            crate::app::request_core(&state.shared, &state.tx, CoreCommand::Pause);
        }
        SAVE => {
            let delay = text(unsafe { GetDlgItem(state.hwnd, DELAY) });
            let host = text(unsafe { GetDlgItem(state.hwnd, ADDRESS) });
            match apply_save(&config, &delay, &host) {
                Ok(cfg) => send_advanced_settings(&state, cfg),
                Err(message) => set_feedback(&state, &message),
            }
        }
        CLI => match browse(state.hwnd) {
            Ok(Some(path)) => match config.lm_mut() {
                Ok(lm) => {
                    lm.lms_path = path;
                    send_advanced_settings(&state, config);
                }
                Err(e) => set_feedback(&state, &e.to_string()),
            },
            Ok(None) => crate::app::local_result(
                &state.shared,
                Outcome::Cancelled,
                "Locate lms cancelled; settings unchanged.",
            ),
            Err(error) => set_feedback(&state, &format!("Could not locate lms: {error:#}")),
        },
        _ => (),
    }
}
unsafe extern "system" fn procedure(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        procedure_inner(hwnd, message, w, l)
    }))
    .unwrap_or_else(|_| {
        crate::app::log(
            &crate::config::data_directory(),
            "Dashboard callback panic contained",
        );
        0
    })
}
unsafe fn procedure_inner(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_COMMAND => {
            command((w & 0xffff) as i32, ((w >> 16) & 0xffff) as u32);
            reveal_focus();
            0
        }
        WM_SIZE => {
            if w != SIZE_MINIMIZED as usize {
                position_controls();
            }
            0
        }
        WM_HSCROLL | WM_VSCROLL if l == 0 => {
            scroll_viewport(message == WM_HSCROLL, (w & 0xffff) as i32);
            0
        }
        WM_NOTIFY if l != 0 => unsafe {
            let notification = &*(l as *const NMHDR);
            if notification.idFrom == NAVIGATION as usize && notification.code == TCN_SELCHANGE {
                select_page(SendMessageW(notification.hwndFrom, TCM_GETCURSEL, 0, 0));
            }
            0
        },
        WM_TIMER => {
            refresh();
            0
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            theme_changed();
            0
        }
        WM_ERASEBKGND => unsafe {
            if let Some(theme) = snapshot().filter(|s| s.hwnd == hwnd).and_then(|s| s.theme) {
                let mut rect: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rect);
                FillRect(w as HDC, &rect, theme.background);
                1
            } else {
                DefWindowProcW(hwnd, message, w, l)
            }
        },
        WM_DPICHANGED => {
            if l != 0 {
                unsafe {
                    let rect = *(l as *const RECT);
                    SetWindowPos(
                        hwnd,
                        null_mut(),
                        rect.left,
                        rect.top,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
            let dpi = (w >> 16) as i32;
            let settings_visible = snapshot().map(|s| s.settings_visible).unwrap_or(false);
            relayout(hwnd, dpi, settings_visible);
            reveal_focus();
            0
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => unsafe {
            if let Some(theme) = snapshot()
                .filter(|s| s.hwnd == hwnd)
                .and_then(|s| s.theme)
                .filter(|theme| theme.dark)
            {
                let button = message == WM_CTLCOLORBTN;
                SetBkColor(
                    w as HDC,
                    if button {
                        theme.palette.surface
                    } else {
                        theme.palette.background
                    },
                );
                SetTextColor(
                    w as HDC,
                    if IsWindowEnabled(l as HWND) != 0 {
                        theme.palette.text
                    } else {
                        theme.palette.disabled
                    },
                );
                return if button {
                    theme.surface
                } else {
                    theme.background
                } as LRESULT;
            }
            let idx = ctlcolor_index(message);
            SetBkColor(w as HDC, GetSysColor(idx));
            let text = if IsWindowEnabled(l as HWND) != 0 {
                if message == WM_CTLCOLORBTN {
                    COLOR_BTNTEXT
                } else {
                    COLOR_WINDOWTEXT
                }
            } else {
                COLOR_GRAYTEXT
            };
            SetTextColor(w as HDC, GetSysColor(text));
            GetSysColorBrush(idx) as LRESULT
        },
        // P1-4: owner-drawn games listbox — supply the row height, then paint it.
        WM_MEASUREITEM => unsafe {
            let dpi = GetDpiForWindow(hwnd) as i32;
            measure_list_item(l, dpi);
            1 // TRUE: handled
        },
        WM_DRAWITEM => unsafe {
            let item = *(l as *const DRAWITEMSTRUCT);
            if item.CtlID == LIST as u32 {
                let state = snapshot();
                if let Some(st) = state {
                    draw_list_item(
                        l,
                        item.itemID as usize,
                        st.font,
                        &st.rows,
                        GetDpiForWindow(hwnd) as i32,
                    );
                }
                1 // TRUE: handled
            } else if item.CtlID == STATE_ACCENT as u32 {
                crate::theme::fill(item.hDC, &item.rcItem, crate::theme::accent(hwnd));
                1
            } else if item.CtlType == ODT_BUTTON
                && snapshot()
                    .and_then(|s| s.theme)
                    .is_some_and(|theme| theme.palette.color_words)
            {
                let dark = snapshot()
                    .and_then(|s| s.theme)
                    .is_some_and(|theme| theme.dark);
                crate::theme::paint_control(item.hwndItem, item.hDC, false, dark);
                1
            } else {
                0
            }
        },
        WM_CLOSE => {
            unsafe {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_DESTROY => {
            RUNNING_REQUESTED.store(false, Ordering::Relaxed);
            if let Some(state) = snapshot() {
                unsafe {
                    if IsWindow(state.tooltips.hwnd) != 0 {
                        DestroyWindow(state.tooltips.hwnd);
                    }
                }
            }
            0
        }
        WM_NCDESTROY => {
            let old = STATE.with(|s| s.borrow_mut().take());
            if let Some(old) = old {
                unsafe {
                    for font in old.fonts.values() {
                        DeleteObject(*font);
                    }
                }
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, w, l) },
    }
}
pub fn is_dialog_message(msg: &MSG) -> bool {
    if let Some(state) = snapshot() {
        if msg.message == WM_KEYDOWN
            && msg.wParam == VK_F1 as usize
            && unsafe { IsChild(state.hwnd, GetFocus()) } != 0
        {
            let id = unsafe { GetDlgCtrlID(GetFocus()) };
            if id != HELP {
                update_help(id);
            }
            unsafe {
                SetFocus(GetDlgItem(state.hwnd, HELP));
            }
            reveal_focus();
            return true;
        }
        let handled = unsafe { IsDialogMessageW(state.hwnd, msg) != 0 };
        if handled {
            reveal_focus();
        }
        handled
    } else {
        false
    }
}
pub fn theme_changed() {
    let choice = snapshot()
        .and_then(|s| s.shared.lock().ok().map(|s| s.config.appearance))
        .unwrap_or_default();
    set_client_theme(crate::theme::effective_dark(choice));
}
fn set_client_theme(dark: bool) {
    if let Some(state) = snapshot() {
        let Some(theme) = crate::theme::Theme::new(dark) else {
            return;
        };
        let old = STATE.with(|s| {
            s.borrow_mut()
                .as_mut()
                .and_then(|s| s.theme.replace(theme.clone()))
        });
        unsafe {
            tray::apply_theme_mode(state.hwnd, dark);
            for control in LAYOUT
                .iter()
                .filter(|e| e.class == "BUTTON" || e.id == NAVIGATION)
            {
                crate::theme::apply_control(
                    GetDlgItem(state.hwnd, control.id),
                    theme.dark,
                    control.id == NAVIGATION,
                );
            }
            if IsWindow(state.tooltips.hwnd) != 0 {
                SendMessageW(
                    state.tooltips.hwnd,
                    TTM_SETTIPBKCOLOR,
                    theme.palette.surface as usize,
                    0,
                );
                SendMessageW(
                    state.tooltips.hwnd,
                    TTM_SETTIPTEXTCOLOR,
                    theme.palette.text as usize,
                    0,
                );
            }
            let detail = GetDlgItem(state.hwnd, DETAILS);
            format_details(detail, &text(detail));
            RedrawWindow(
                state.hwnd,
                null(),
                null_mut(),
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
            );
        }
        drop(old);
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
        if !crate::rich_text::initialize() {
            tray::error("Could not initialize the native game details control.");
            return;
        }
        let instance = GetModuleHandleW(null());
        let controls = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_TAB_CLASSES,
        };
        if InitCommonControlsEx(&controls) == 0 {
            tray::error("Could not initialize dashboard navigation.");
            return;
        }
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
        let (desired_width, desired_height) = outer_size(false, dpi);
        let mut work = RECT {
            left: 0,
            top: 0,
            right: GetSystemMetrics(SM_CXSCREEN),
            bottom: GetSystemMetrics(SM_CYSCREEN),
        };
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut RECT as *mut _, 0);
        let width = desired_width.min(work.right - work.left);
        let height = desired_height.min(work.bottom - work.top);
        let hwnd = CreateWindowExW(
            WS_EX_COMPOSITED,
            class.as_ptr(),
            wide("GamePause for LM Studio").as_ptr(),
            WINDOW_STYLE,
            work.left + (work.right - work.left - width) / 2,
            work.top + (work.bottom - work.top - height) / 2,
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
        let font = dashboard_font(dpi);
        if font.is_null() {
            DestroyWindow(hwnd);
            tray::error("Could not create dashboard font.");
            return;
        }
        for e in LAYOUT {
            let child = CreateWindowExW(
                if (e.class == "EDIT" && e.style & ES_READONLY as u32 == 0) || e.class == "LISTBOX"
                {
                    WS_EX_CLIENTEDGE
                } else {
                    0
                },
                wide(e.class).as_ptr(),
                wide(e.label).as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | e.style,
                scale(e.x),
                scale(e.y),
                scale(e.w),
                scale(e.h),
                hwnd,
                e.id as HMENU,
                instance,
                null(),
            );
            if child.is_null() {
                DestroyWindow(hwnd);
                DeleteObject(font);
                tray::error("Could not create dashboard controls.");
                return;
            }
            SendMessageW(child, WM_SETFONT, font as usize, 1);
        }
        // Group boxes are overlapping siblings, rather than parents. Keep them
        // behind their controls so WS_CLIPSIBLINGS cannot hide the controls.
        for e in LAYOUT
            .iter()
            .filter(|e| e.class == "BUTTON" && e.style & BS_TYPEMASK as u32 == BS_GROUPBOX as u32)
        {
            SetWindowPos(
                GetDlgItem(hwnd, e.id),
                HWND_BOTTOM,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        set(
            GetDlgItem(hwnd, TITLE),
            &format!("GamePause v{}", env!("CARGO_PKG_VERSION")),
        );
        for (index, label) in ["Games", "Running apps", "Ignored"].iter().enumerate() {
            let mut label = wide(label);
            let mut item: TCITEMW = std::mem::zeroed();
            item.mask = TCIF_TEXT;
            item.pszText = label.as_mut_ptr();
            if SendMessageW(
                GetDlgItem(hwnd, NAVIGATION),
                TCM_INSERTITEMW,
                index,
                &item as *const _ as isize,
            ) < 0
            {
                DestroyWindow(hwnd);
                DeleteObject(font);
                tray::error("Could not create dashboard navigation pages.");
                return;
            }
        }
        let appearance = GetDlgItem(hwnd, APPEARANCE);
        for label in ["Follow Windows", "Light", "Dark"] {
            SendMessageW(appearance, CB_ADDSTRING, 0, wide(label).as_ptr() as isize);
        }
        SendMessageW(appearance, CB_SETMINVISIBLE, 3, 0);
        let tooltips = create_tooltips(hwnd, instance, dpi);
        SendMessageW(tooltips.hwnd, WM_SETFONT, font as usize, 1);
        apply_settings_visibility(hwnd, false);
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
                fonts: [(dpi, font)].into_iter().collect(),
                theme: None,
                settings_visible: false,
                viewport: Viewport::default(),
                positioning: false,
                tooltips,
            })
        });
        theme_changed();
        position_controls();
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
    #[test]
    fn viewport_keeps_focus_reachable_at_each_supported_dpi() {
        for dpi in [96, 144, 192] {
            for settings in [false, true] {
                let content = (scale(884, dpi), window_height(settings, dpi));
                let client = (scale(520, dpi), scale(400, dpi));
                let bottom = Viewport {
                    x: i32::MAX,
                    y: i32::MAX,
                }
                .bounded(content, client);
                assert_eq!(
                    bottom,
                    Viewport {
                        x: content.0 - client.0,
                        y: content.1 - client.1
                    }
                );
                for entry in LAYOUT.iter().filter(|entry| entry.style & WS_TABSTOP != 0) {
                    if is_settings_row(entry.id) && !settings {
                        continue;
                    }
                    let rect = if entry.id == TITLE {
                        footer_position(settings, dpi)
                    } else {
                        (
                            scale(entry.x, dpi),
                            scale(entry.y, dpi),
                            scale(entry.w, dpi),
                            scale(entry.h, dpi),
                        )
                    };
                    let view = bottom.reveal(rect, client).bounded(content, client);
                    assert!(rect.0 >= view.x && rect.0 < view.x + client.0);
                    assert!(rect.1 >= view.y && rect.1 + rect.3 <= view.y + client.1);
                    if rect.2 <= client.0 {
                        assert!(rect.0 + rect.2 <= view.x + client.0);
                    }
                    assert_eq!(view.reveal(rect, client).bounded(content, client), view);
                }
                assert_eq!(bottom.bounded(content, content), Viewport::default());
                assert_eq!(
                    Viewport { x: -10, y: -20 }.bounded(content, client),
                    Viewport::default()
                );
            }
        }
    }

    #[test]
    fn hidden_native_resize_preserves_edit_selection_and_settings_reachability() {
        // This private window is never shown. Exercise WM_SIZE and real EDIT
        // selection without a provider, message loop, or physical desktop test.
        struct Fixture(HWND);
        impl Drop for Fixture {
            fn drop(&mut self) {
                unsafe {
                    DestroyWindow(self.0);
                }
            }
        }
        unsafe {
            let instance = GetModuleHandleW(null());
            let class = wide("GamePauseHiddenViewportFixture");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            assert_ne!(RegisterClassW(&wc), 0);
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                wide("Fixture").as_ptr(),
                WINDOW_STYLE,
                0,
                0,
                540,
                440,
                null_mut(),
                null_mut(),
                instance,
                null(),
            );
            assert!(!hwnd.is_null());
            let fixture = Fixture(hwnd);
            for id in [SEARCH, TITLE, SAVE] {
                let child = CreateWindowExW(
                    0,
                    wide("EDIT").as_ptr(),
                    wide("Fixture text").as_ptr(),
                    WS_CHILD | WS_VISIBLE | ES_MULTILINE as u32,
                    0,
                    0,
                    100,
                    30,
                    hwnd,
                    id as HMENU,
                    instance,
                    null(),
                );
                assert!(!child.is_null());
            }
            let (tx, _) = std::sync::mpsc::channel();
            STATE.with(|s| {
                *s.borrow_mut() = Some(WindowState {
                    hwnd,
                    shared: Arc::new(std::sync::Mutex::new(Shared::default())),
                    tx,
                    folder: PathBuf::new(),
                    page: Page::Games,
                    rows: vec![],
                    last_status: String::new(),
                    revision: 0,
                    font: null_mut(),
                    fonts: Default::default(),
                    theme: None,
                    settings_visible: true,
                    viewport: Viewport {
                        x: 100_000,
                        y: 100_000,
                    },
                    positioning: false,
                    tooltips: Tooltips {
                        hwnd: null_mut(),
                        _texts: Arc::new(vec![]),
                    },
                })
            });
            let edit = GetDlgItem(hwnd, SEARCH);
            SendMessageW(edit, EM_SETSEL, 2, 7);
            SetWindowPos(hwnd, null_mut(), 0, 0, 550, 450, SWP_NOMOVE | SWP_NOZORDER);
            let state = snapshot().unwrap();
            assert!(state.viewport.x > 0 && state.viewport.y > 0);
            assert!(!state.positioning);
            let mut client: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut client);
            let dpi = GetDpiForWindow(hwnd) as i32;
            assert_eq!(state.viewport.y, window_height(true, dpi) - client.bottom);
            let mut feedback: RECT = std::mem::zeroed();
            GetWindowRect(GetDlgItem(hwnd, TITLE), &mut feedback);
            MapWindowPoints(
                null_mut(),
                hwnd,
                &mut feedback as *mut RECT as *mut POINT,
                2,
            );
            assert!(feedback.top >= 0 && feedback.bottom <= client.bottom);
            STATE.with(|s| s.borrow_mut().as_mut().unwrap().settings_visible = false);
            apply_settings_visibility(hwnd, false);
            position_controls();
            assert_eq!(
                GetWindowLongPtrW(GetDlgItem(hwnd, SAVE), GWL_STYLE) as u32 & WS_VISIBLE,
                0
            );
            assert_eq!(
                snapshot().unwrap().viewport.y,
                window_height(false, dpi) - client.bottom
            );
            let mut start = 0u32;
            let mut end = 0u32;
            SendMessageW(
                edit,
                EM_GETSEL,
                &mut start as *mut _ as usize,
                &mut end as *mut _ as isize,
            );
            assert_eq!((start, end), (2, 7));
            assert_eq!(text(edit), "Fixture text");
            let dc = CreateCompatibleDC(null_mut());
            assert!(!dc.is_null());
            EnableWindow(edit, 0);
            let brush = SendMessageW(hwnd, WM_CTLCOLORSTATIC, dc as usize, edit as isize);
            assert_eq!(brush, GetSysColorBrush(COLOR_WINDOW) as isize);
            assert_eq!(GetTextColor(dc), GetSysColor(COLOR_GRAYTEXT));
            assert_eq!(GetBkColor(dc), GetSysColor(COLOR_WINDOW));
            set_client_theme(true);
            let theme = snapshot().unwrap().theme.unwrap();
            let brush = SendMessageW(hwnd, WM_CTLCOLORSTATIC, dc as usize, edit as isize);
            assert_eq!(brush, theme.background as isize);
            assert_eq!(GetTextColor(dc), theme.palette.disabled);
            assert_eq!(GetBkColor(dc), theme.palette.background);
            SendMessageW(
                edit,
                EM_GETSEL,
                &mut start as *mut _ as usize,
                &mut end as *mut _ as isize,
            );
            assert_eq!((start, end), (2, 7), "theme must preserve native selection");
            set_client_theme(false);
            DeleteDC(dc);
            let (width, height) = outer_size(false, dpi);
            SetWindowPos(
                hwnd,
                null_mut(),
                0,
                0,
                width + 100,
                height + 100,
                SWP_NOMOVE | SWP_NOZORDER,
            );
            assert_eq!(snapshot().unwrap().viewport, Viewport::default());
            drop(fixture);
            assert!(snapshot().is_none());
            assert_ne!(UnregisterClassW(class.as_ptr(), instance), 0);
        }
    }

    #[test]
    fn selected_text_only_row_preserves_dc_state_and_has_no_dot() {
        unsafe {
            let dc = CreateCompatibleDC(null_mut());
            assert!(!dc.is_null());
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = 256;
            info.bmiHeader.biHeight = -24;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            let mut pixels = null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut pixels, null_mut(), 0);
            assert!(!bitmap.is_null());
            let old_bitmap = SelectObject(dc, bitmap);
            let font = dashboard_font(96);
            assert!(!font.is_null());
            SetBkMode(dc, OPAQUE as i32);
            SetBkColor(dc, 0x00ffffff);
            SetTextColor(dc, 0x00012345);
            let old_font = GetCurrentObject(dc, OBJ_FONT as u32);
            let rows = [Row {
                // Leading whitespace exposes opaque white text backgrounds
                // inside the selected text extent, not merely outside it.
                label: "   Fixture game".into(),
                name: "Fixture game".into(),
                path: r"D:\Fixture Games\play.exe".into(),
                custom: true,
                ignored: false,
                running: true,
            }];
            let item = DRAWITEMSTRUCT {
                CtlID: LIST as u32,
                itemID: 0,
                itemState: ODS_SELECTED,
                hDC: dc,
                rcItem: RECT {
                    left: 0,
                    top: 0,
                    right: 256,
                    bottom: 24,
                },
                ..std::mem::zeroed()
            };
            draw_list_item(&item as *const _ as isize, 0, font, &rows, 96);
            let restored = (
                GetBkMode(dc),
                GetBkColor(dc),
                GetTextColor(dc),
                GetCurrentObject(dc, OBJ_FONT as u32),
            );
            let former_dot = GetPixel(dc, 12, 12);
            let selection = crate::theme::Palette::for_mode(false).selected;
            let dark = crate::theme::Palette::for_mode(true);
            draw_list_item_palette(&item as *const _ as isize, 0, font, &rows, 96, dark);
            assert_eq!(
                GetPixel(dc, 12, 12),
                dark.selected,
                "whitespace inside the text extent must retain selection color"
            );
            let dark_restored = (
                GetBkMode(dc),
                GetBkColor(dc),
                GetTextColor(dc),
                GetCurrentObject(dc, OBJ_FONT as u32),
            );
            assert_eq!(dark_restored, restored);
            SelectObject(dc, old_bitmap);
            DeleteObject(bitmap);
            DeleteObject(font);
            DeleteDC(dc);
            assert_eq!(restored, (OPAQUE as i32, 0x00ffffff, 0x00012345, old_font));
            assert_eq!(former_dot, selection);
        }
    }
    #[test]
    fn native_navigation_is_distinct_and_every_action_has_focus_help() {
        assert_eq!(by_id(NAVIGATION).class, "SysTabControl32");
        for page in [Page::Games, Page::Running, Page::Ignored] {
            assert!(Page::from_tab(page.tab() as isize) == Some(page));
        }
        assert!(Page::from_tab(-1).is_none());
        let shared = Shared::default();
        for control in LAYOUT
            .iter()
            .filter(|control| control.style & WS_TABSTOP != 0)
        {
            let help = description(control.id, Page::Games, &shared);
            assert!(!help.is_empty());
            if control.class == "BUTTON" {
                assert!(DESCRIPTIONS.iter().any(|(id, _)| *id == control.id));
                assert_ne!(control.style & BS_NOTIFY as u32, 0);
            }
        }
        for id in [STATUS, RUNNING_GAMES, HELP, FEEDBACK] {
            assert_eq!(by_id(id).class, "EDIT");
            assert_ne!(by_id(id).style & ES_READONLY as u32, 0);
            assert_eq!(by_id(id).style & WS_VSCROLL, 0);
        }
    }
    #[test]
    fn captions_fit_measured_dashboard_font_at_100_150_200_percent() {
        for dpi in [96, 144, 192] {
            let mut measurements = Vec::new();
            unsafe {
                let dc = CreateCompatibleDC(null_mut());
                assert!(!dc.is_null());
                let font = dashboard_font(dpi);
                assert!(!font.is_null());
                let old = SelectObject(dc, font);
                let mut captions = LAYOUT
                    .iter()
                    .filter(|e| e.class == "BUTTON" && e.style & BS_GROUPBOX as u32 == 0)
                    .map(|e| (e.id, e.label))
                    .collect::<Vec<_>>();
                captions.extend([
                    (TOGGLE, "Add selected as game"),
                    (TOGGLE, "Enable selected"),
                    (RESUME, "Resume AI..."),
                    (RESUME, "Retry resume"),
                ]);
                for (id, label) in captions {
                    let text = wide(label);
                    let mut extent: SIZE = std::mem::zeroed();
                    assert_ne!(
                        GetTextExtentPoint32W(
                            dc,
                            text.as_ptr(),
                            (text.len() - 1) as i32,
                            &mut extent
                        ),
                        0
                    );
                    let geometry = by_id(id);
                    let padding = if geometry.style & BS_AUTOCHECKBOX as u32 != 0 {
                        34
                    } else {
                        24
                    };
                    measurements.push((
                        id,
                        label,
                        extent,
                        scale(geometry.w, dpi) - scale(padding, dpi),
                        scale(geometry.h, dpi) - scale(8, dpi),
                    ));
                }
                SelectObject(dc, old);
                DeleteObject(font);
                DeleteDC(dc);
            }
            for (id, label, extent, available, height) in measurements {
                assert!(
                    extent.cx <= available,
                    "{dpi} DPI: {label} needs {} pixels; {available} available",
                    extent.cx
                );
                assert!(
                    extent.cy <= height,
                    "{dpi} DPI: {label} exceeds padded control height"
                );
                if [REMOVE, VERIFY, CLI].contains(&id) {
                    eprintln!(
                        "{dpi} DPI: {label}: text {}x{}, padded width {available}, padded height {height}",
                        extent.cx, extent.cy
                    );
                }
            }
        }
    }
    #[test]
    fn selected_path_survives_unrelated_row_updates_and_reordering() {
        let row = |name: &str, path: &str| super::Row {
            label: name.into(),
            name: name.into(),
            path: path.into(),
            custom: true,
            ignored: false,
            running: false,
        };
        let mut rows = vec![
            row("Fixture A", r"D:\Fixture Games\a.exe"),
            row("Fixture B", r"D:\Fixture Games\b.exe"),
        ];
        rows.insert(0, row("Fixture launcher", r"D:\Fixture Steam\game"));
        rows[2].label = "Fixture B running".into();
        assert_eq!(
            super::selected_index(&rows, r"d:\fixture games\B.EXE"),
            Some(2)
        );
        assert_eq!(
            super::selected_index(&rows, r"D:\Fixture Games\missing.exe"),
            None
        );
    }
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
            created_at: 1,
            executable: r"D:\Games\Witcher\play.exe".into(),
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
        assert_eq!(window_height(false, 96), 804);
        assert_eq!(window_height(true, 96), 1244);
        assert_eq!(window_height(false, 144), 1206);
        assert_eq!(window_height(true, 144), 1866);
    }
    #[test]
    fn version_footer_sits_below_settings_and_feedback_stays_below_help() {
        assert!(by_id(FEEDBACK).y >= by_id(HELP).y + by_id(HELP).h);
        assert!(by_id(FEEDBACK).y + by_id(FEEDBACK).h <= by_id(OPTIONS_BOX).y);
        assert_eq!(footer_position(false, 96), (24, 734, 836, 26));
        assert_eq!(footer_position(true, 96), (24, 1174, 836, 26));
        let (_, y, _, _) = footer_position(true, 144);
        assert_eq!(y, 1761);
        for dpi in [96, 144, 192] {
            let (_, y, _, height) = footer_position(true, dpi);
            let frame = by_id(SETTINGS_BOX);
            assert!(scale(frame.y + frame.h, dpi) < y);
            assert!(y + height < window_height(true, dpi));
        }
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
            "statics should track the system window color"
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

    #[test]
    fn row_height_scales_with_dpi() {
        assert_eq!(super::row_height(96), 24);
        assert_eq!(super::row_height(144), 36);
        assert_eq!(super::row_height(192), 48);
    }
    #[test]
    fn draw_list_item_on_empty_rows_is_guarded_not_crashing() {
        let mut item: super::DRAWITEMSTRUCT = unsafe { std::mem::zeroed() };
        item.CtlID = super::LIST as u32;
        item.itemID = 0;
        // Null device context: the fill is a harmless no-op. The behavior under
        // test is that an out-of-range item index is guarded by `rows.get(..)`
        // rather than dereferenced — an empty/short list must not panic.
        let lp = &mut item as *mut _ as isize;
        unsafe {
            super::draw_list_item(lp, 0, std::ptr::null_mut(), &[], 96);
        }
        assert_eq!(
            item.CtlID,
            super::LIST as u32,
            "struct still well-formed after the call"
        );
    }
    #[test]
    fn game_rows_report_running_and_ignore_state_in_text() {
        let mut shared = Shared::default();
        shared.games.push(crate::discovery::Game::new(
            "Steam",
            "1",
            "Alpha",
            r"D:\G\alpha",
        ));
        shared.games.push(crate::discovery::Game::new(
            "Epic",
            "2",
            "Beta",
            r"D:\G\beta",
        ));
        shared.active_games.push(crate::processes::ActiveGame {
            pid: 7,
            created_at: 1,
            executable: r"D:\G\alpha\play.exe".into(),
            game: "Alpha".into(),
            launcher: "Steam".into(),
            path: r"D:\G\alpha".into(),
        });
        shared.config.ignored_games.push(r"d:\g\beta".into());
        let rows = super::rows(&shared, super::Page::Games, "");
        let alpha = rows.iter().find(|r| r.name == "Alpha").unwrap();
        let beta = rows.iter().find(|r| r.name == "Beta").unwrap();
        assert!(alpha.running && alpha.label.contains("Running"));
        assert!(
            beta.ignored && !beta.running,
            "an ignored idle game stays labelled in text"
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
            running: false,
        }
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
                    name: "verify-unloaded".into(),
                    ok: false,
                    detail: "models still loaded after unload".into(),
                },
            ],
            ok: false,
            summary: "Round-trip verify failed at: verify-unloaded".into(),
        };
        let rendered = super::render_verify_report(&report);
        for needle in [
            "capture: 1 model(s) captured (ok)",
            "unload: 1 model(s) unloaded (ok)",
            "verify-unloaded: models still loaded after unload (FAIL)",
            "Round-trip verify failed at: verify-unloaded",
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
    fn scrollbars_only_appear_for_overflow_and_account_for_each_other() {
        assert_eq!(
            needed_scrollbars((884, 804), (884, 804), (17, 17)),
            (false, false)
        );
        assert_eq!(
            needed_scrollbars((884, 804), (883, 804), (17, 17)),
            (true, true)
        );
        assert_eq!(
            needed_scrollbars((884, 804), (1000, 600), (17, 17)),
            (false, true)
        );
        assert_eq!(
            needed_scrollbars((884, 804), (600, 1000), (17, 17)),
            (true, false)
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
        assert_eq!(ok.lm_endpoint(), "127.0.0.1:8080");
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
    #[test]
    #[ignore = "requires an interactive Windows desktop; opens the test dashboard"]
    fn native_ollama_opt_in_handles_cancel_accept_and_stale_timer_reentry() {
        use std::cell::Cell;
        use windows_sys::Win32::System::Threading::GetCurrentThreadId;
        thread_local! {
            static OWNER: Cell<HWND> = const { Cell::new(null_mut()) };
            static CANCEL_DEFAULT: Cell<bool> = const { Cell::new(false) };
            static REPLY: Cell<i32> = const { Cell::new(IDCANCEL) };
            static HIDE: Cell<bool> = const { Cell::new(false) };
            static SHARED: RefCell<Option<SharedState>> = const { RefCell::new(None) };
        }
        unsafe extern "system" fn cancel_dialog(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
            if code == HCBT_ACTIVATE as i32 {
                let dialog = w as HWND;
                let owner = OWNER.with(Cell::get);
                if unsafe { GetWindow(dialog, GW_OWNER) } == owner
                    && !unsafe { GetDlgItem(dialog, IDCANCEL) }.is_null()
                {
                    CANCEL_DEFAULT.with(|value| {
                        value.set(
                            unsafe { SendMessageW(dialog, DM_GETDEFID, 0, 0) } as u32 & 0xffff
                                == IDCANCEL as u32,
                        )
                    });
                    if HIDE.with(Cell::get)
                        && let Some(shared) = SHARED.with(|value| value.borrow().clone())
                    {
                        let mut shared = shared.lock().unwrap();
                        shared.config.advanced_settings_visible = false;
                        shared.revision += 1;
                    }
                    unsafe {
                        SendMessageW(owner, WM_TIMER, 1, 0);
                        PostMessageW(dialog, WM_COMMAND, REPLY.with(Cell::get) as usize, 0);
                    }
                }
            }
            unsafe { CallNextHookEx(null_mut(), code, w, l) }
        }
        struct Fixture {
            hwnd: HWND,
            hook: HHOOK,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                unsafe {
                    UnhookWindowsHookEx(self.hook);
                    SendMessageW(self.hwnd, WM_CLOSE, 0, 0);
                }
                OWNER.with(|value| value.set(null_mut()));
                SHARED.with(|value| *value.borrow_mut() = None);
            }
        }
        let shared = Arc::new(std::sync::Mutex::new(Shared {
            active_mode: false,
            config: Config {
                advanced_settings_visible: true,
                ..Default::default()
            },
            ..Default::default()
        }));
        let (tx, rx) = std::sync::mpsc::channel();
        show(shared.clone(), tx, PathBuf::new());
        let hwnd = snapshot().unwrap().hwnd;
        OWNER.with(|value| value.set(hwnd));
        SHARED.with(|value| *value.borrow_mut() = Some(shared.clone()));
        let hook = unsafe {
            SetWindowsHookExW(
                WH_CBT,
                Some(cancel_dialog),
                null_mut(),
                GetCurrentThreadId(),
            )
        };
        assert!(!hook.is_null());
        let fixture = Fixture { hwnd, hook };
        for (reply, hide) in [(IDCANCEL, false), (IDOK, false), (IDOK, true)] {
            REPLY.with(|value| value.set(reply));
            HIDE.with(|value| value.set(hide));
            shared.lock().unwrap().commands.settings_pending = false;
            unsafe {
                SendMessageW(
                    GetDlgItem(hwnd, OLLAMA_ENABLED),
                    BM_SETCHECK,
                    BST_CHECKED as usize,
                    0,
                );
            }
            command(OLLAMA_ENABLED, BN_CLICKED);
            assert!(CANCEL_DEFAULT.with(Cell::get));
            if reply == IDOK && !hide {
                assert!(
                    matches!(rx.try_recv(), Ok(Action::Tracked { action, .. }) if matches!(*action, Action::AdvancedSettings(ref config) if config.providers[1].enabled()))
                );
            } else {
                assert!(rx.try_recv().is_err());
            }
            assert!(
                !shared.lock().unwrap().config.providers[1].enabled(),
                "enablement waits for worker persistence"
            );
            assert!(!checked(unsafe { GetDlgItem(hwnd, OLLAMA_ENABLED) }));
        }
        drop(fixture);
    }
    #[test]
    #[ignore = "requires an interactive Windows desktop; renders an isolated mock dashboard"]
    fn native_dashboard_controls_are_visible_and_scrollbars_follow_overflow() {
        let shared = Arc::new(std::sync::Mutex::new(Shared {
            active_mode: true,
            discovery_ready: true,
            detection_ok: true,
            activity: crate::control::Activity::Watching,
            ..Default::default()
        }));
        let (tx, rx) = std::sync::mpsc::channel();
        show(shared.clone(), tx, PathBuf::new());
        let hwnd = snapshot().unwrap().hwnd;
        struct Fixture(HWND);
        impl Drop for Fixture {
            fn drop(&mut self) {
                unsafe {
                    DestroyWindow(self.0);
                }
            }
        }
        let _fixture = Fixture(hwnd);
        unsafe {
            let dpi = GetDpiForWindow(hwnd) as i32;
            let size = outer_size(false, dpi);
            SetWindowPos(hwnd, HWND_TOP, 0, 0, size.0, size.1, SWP_NOMOVE);
            position_controls();
            assert_eq!(
                GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & (WS_HSCROLL | WS_VSCROLL),
                0
            );
            for id in [
                AUTO, SETTINGS, NAVIGATION, REFRESH, ADD, SEARCH, LIST, TOGGLE, PAUSE, RESUME, QUIT,
            ] {
                let child = GetDlgItem(hwnd, id);
                let mut rect: RECT = std::mem::zeroed();
                GetWindowRect(child, &mut rect);
                let mut point = POINT {
                    x: (rect.left + rect.right) / 2,
                    y: (rect.top + rect.bottom) / 2,
                };
                ScreenToClient(hwnd, &mut point);
                assert_eq!(
                    ChildWindowFromPointEx(hwnd, point, CWP_SKIPINVISIBLE),
                    child,
                    "control {id} is covered by a sibling"
                );
            }
            let edit = GetDlgItem(hwnd, STATUS);
            set(edit, "Short readable status.");
            assert_eq!(GetWindowLongPtrW(edit, GWL_STYLE) as u32 & WS_VSCROLL, 0);
            set(edit, &"A long provider status line.\r\n".repeat(50));
            assert_ne!(GetWindowLongPtrW(edit, GWL_STYLE) as u32 & WS_VSCROLL, 0);
            SendMessageW(edit, EM_SETSEL, 2, 7);
            update_text_scrollbar(edit);
            let mut start = 0u32;
            let mut end = 0u32;
            SendMessageW(
                edit,
                EM_GETSEL,
                &mut start as *mut _ as usize,
                &mut end as *mut _ as isize,
            );
            assert_eq!((start, end), (2, 7));
            set(edit, "Short readable status.");
            assert_eq!(GetWindowLongPtrW(edit, GWL_STYLE) as u32 & WS_VSCROLL, 0);
            SetWindowPos(
                hwnd,
                null_mut(),
                0,
                0,
                scale(540, dpi),
                scale(440, dpi),
                SWP_NOMOVE | SWP_NOZORDER,
            );
            assert_ne!(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_VSCROLL, 0);
            SetWindowPos(
                hwnd,
                null_mut(),
                0,
                0,
                size.0,
                size.1,
                SWP_NOMOVE | SWP_NOZORDER,
            );
            assert_eq!(
                GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & (WS_HSCROLL | WS_VSCROLL),
                0
            );
            assert_eq!(
                (
                    snapshot().unwrap().viewport.x,
                    snapshot().unwrap().viewport.y
                ),
                (0, 0)
            );
            STATE.with(|state| state.borrow_mut().as_mut().unwrap().last_status.clear());
            {
                let mut state = shared.lock().unwrap();
                state.games = vec![
                    crate::discovery::Game::new(
                        "Steam",
                        "on",
                        "Fixture adventure",
                        r"D:\Fixture Games\on.exe",
                    ),
                    crate::discovery::Game::new(
                        "Custom",
                        "off",
                        "Fixture racing",
                        r"D:\Fixture Games\off.exe",
                    ),
                ];
                state
                    .config
                    .ignored_games
                    .push(r"D:\Fixture Games\off.exe".into());
                state.revision += 1;
            }
            refresh();
            SendMessageW(GetDlgItem(hwnd, LIST), LB_SETCURSEL, 0, 0);
            update_selection();
            let details = GetDlgItem(hwnd, DETAILS);
            SendMessageW(details, EM_SETSEL, 2, 7);
            update_selection();
            let mut start = 0u32;
            let mut end = 0u32;
            SendMessageW(
                details,
                EM_GETSEL,
                &mut start as *mut _ as usize,
                &mut end as *mut _ as isize,
            );
            assert_eq!(
                (start, end),
                (2, 7),
                "unchanged details must preserve selection"
            );
            set_client_theme(true);
            assert_ne!(
                GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_CLIPCHILDREN,
                0
            );
            assert_ne!(
                GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_COMPOSITED,
                0
            );
            shared.lock().unwrap().config.advanced_settings_visible = true;
            refresh();
            let appearance = GetDlgItem(hwnd, APPEARANCE);
            SendMessageW(appearance, CB_SETCURSEL, 1, 0);
            command(APPEARANCE, CBN_SELCHANGE);
            assert!(
                matches!(rx.try_recv(), Ok(Action::Tracked { action, .. }) if matches!(*action, Action::Appearance(crate::config::Appearance::Light)))
            );
            assert_eq!(
                SendMessageW(appearance, CB_GETCURSEL, 0, 0),
                shared.lock().unwrap().config.appearance.index() as isize
            );
            shared.lock().unwrap().commands.settings_pending = false;
            for _ in 0..30 {
                scroll_viewport(false, SB_BOTTOM);
                RedrawWindow(
                    hwnd,
                    null(),
                    null_mut(),
                    RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_UPDATENOW,
                );
                assert!(snapshot().unwrap().viewport.y > 0);
                scroll_viewport(false, SB_TOP);
                RedrawWindow(
                    hwnd,
                    null(),
                    null_mut(),
                    RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_UPDATENOW,
                );
                assert_eq!(snapshot().unwrap().viewport.y, 0);
            }
            SendMessageW(
                details,
                EM_GETSEL,
                &mut start as *mut _ as usize,
                &mut end as *mut _ as isize,
            );
            assert_eq!(
                (start, end),
                (2, 7),
                "scrolling must not reset details selection"
            );
            shared.lock().unwrap().config.advanced_settings_visible = false;
            shared.lock().unwrap().commands.settings_pending = false;
            refresh();
            assert!(!checked(GetDlgItem(hwnd, SETTINGS)));
            assert_ne!(IsWindowEnabled(GetDlgItem(hwnd, SETTINGS)), 0);
            std::fs::create_dir_all("build").unwrap();
            for dark in [true, false] {
                shared.lock().unwrap().config.appearance = if dark {
                    crate::config::Appearance::Dark
                } else {
                    crate::config::Appearance::Light
                };
                set_client_theme(dark);
                refresh();
                for activity in [
                    crate::control::Activity::Watching,
                    crate::control::Activity::ManualHold,
                    crate::control::Activity::Restoring,
                ] {
                    shared.lock().unwrap().activity = activity;
                    refresh();
                    assert_eq!(
                        crate::theme::accent(hwnd),
                        crate::theme::state_accent(activity, dark)
                    );
                }
                shared.lock().unwrap().activity = crate::control::Activity::Watching;
                refresh();
                for id in [PAUSE, RESUME, REMOVE, TOGGLE] {
                    let button = GetDlgItem(hwnd, id);
                    if dark {
                        assert_eq!(
                            GetWindowLongPtrW(button, GWL_STYLE) as u32 & BS_TYPEMASK as u32,
                            BS_OWNERDRAW as u32
                        );
                        SendMessageW(button, WM_MOUSEMOVE, 0, 0);
                        SendMessageW(button, WM_MOUSELEAVE, 0, 0);
                        SendMessageW(button, BM_SETSTATE, 1, 0);
                        SendMessageW(button, BM_SETSTATE, 0, 0);
                    }
                }
                RedrawWindow(
                    hwnd,
                    null(),
                    null_mut(),
                    RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW,
                );
                let mut message: MSG = std::mem::zeroed();
                while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                RedrawWindow(
                    GetDlgItem(hwnd, LIST),
                    null(),
                    null_mut(),
                    RDW_INVALIDATE | RDW_ERASE | RDW_UPDATENOW,
                );
                let mut rect: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rect);
                let source = GetDC(hwnd);
                let dc = CreateCompatibleDC(source);
                let info = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: rect.right,
                        biHeight: -rect.bottom,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB,
                        ..std::mem::zeroed()
                    },
                    ..std::mem::zeroed()
                };
                let mut bits = null_mut();
                let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
                assert!(!bitmap.is_null());
                let old = SelectObject(dc, bitmap);
                assert_ne!(
                    windows_sys::Win32::Storage::Xps::PrintWindow(
                        hwnd,
                        dc,
                        windows_sys::Win32::Storage::Xps::PW_CLIENTONLY | 2
                    ),
                    0
                );
                GdiFlush();
                let pixels = std::slice::from_raw_parts(
                    bits as *const u8,
                    (rect.right * rect.bottom * 4) as usize,
                );
                // Inspect the actual composited button surface, not an isolated WM_PRINTCLIENT.
                let x = scale(by_id(PAUSE).x + 5, dpi);
                let y = scale(by_id(PAUSE).y + 5, dpi);
                let offset = ((y * rect.right + x) * 4) as usize;
                if dark {
                    assert_ne!(&pixels[offset..offset + 3], &[0x20, 0x20, 0x20]);
                }
                let mut bmp = Vec::new();
                bmp.extend_from_slice(b"BM");
                bmp.extend_from_slice(&(54u32 + pixels.len() as u32).to_le_bytes());
                bmp.extend_from_slice(&[0; 4]);
                bmp.extend_from_slice(&54u32.to_le_bytes());
                let header =
                    std::slice::from_raw_parts(&info.bmiHeader as *const _ as *const u8, 40);
                bmp.extend_from_slice(header);
                bmp.extend_from_slice(pixels);
                let name = if dark { "dark" } else { "light" };
                std::fs::write(format!("build/dashboard-review-{name}.bmp"), bmp).unwrap();
                SelectObject(dc, old);
                DeleteObject(bitmap);
                DeleteDC(dc);
                ReleaseDC(hwnd, source);
            }
        }
        assert!(
            rx.try_recv().is_err(),
            "rendering must not request provider work"
        );
        {
            let mut state = shared.lock().unwrap();
            state.manual_pause = true;
            state.pending = true;
            state.activity = crate::control::Activity::ManualHold;
            state.revision += 1;
        }
        refresh();
        unsafe {
            assert_eq!(IsWindowEnabled(GetDlgItem(hwnd, PAUSE)), 0);
            assert_ne!(IsWindowEnabled(GetDlgItem(hwnd, RESUME)), 0);
        }
        command(RESUME, BN_CLICKED);
        assert!(
            matches!(rx.try_recv(), Ok(Action::Tracked { action, .. }) if matches!(*action, Action::Restore))
        );
        {
            let mut state = shared.lock().unwrap();
            state.activity = crate::control::Activity::Restoring;
            state.revision += 1;
        }
        refresh();
        command(RESUME, BN_CLICKED);
        assert!(
            rx.try_recv().is_err(),
            "busy Resume must be rejected again at dispatch"
        );
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop; opens the test dashboard"]
    fn native_dashboard_rescales_font_and_fixed_rows_without_gdi_leaks() {
        use std::sync::{Arc, Mutex, mpsc};
        use windows_sys::Win32::System::Threading::{
            GR_GDIOBJECTS, GetCurrentProcess, GetGuiResources,
        };
        let shared = Arc::new(Mutex::new(Shared {
            active_mode: false,
            ..Default::default()
        }));
        let (tx, rx) = mpsc::channel();
        show(shared.clone(), tx, PathBuf::new());
        fn pump() {
            unsafe {
                let mut message: MSG = std::mem::zeroed();
                while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
        let hwnd = snapshot().unwrap().hwnd;
        for id in [
            VERIFY,
            DOCTOR,
            CLI,
            OPEN_FOLDER,
            STARTUP,
            SAVE,
            NOTIFICATIONS,
            SOUND,
            OLLAMA_ENABLED,
            OLLAMA_SAVE,
            CONTRIBUTE,
        ] {
            command(id, BN_CLICKED);
        }
        assert!(rx.try_recv().is_err(), "hidden tools must not dispatch");
        unsafe {
            SendMessageW(
                GetDlgItem(hwnd, SETTINGS),
                BM_SETCHECK,
                BST_CHECKED as usize,
                0,
            );
        }
        command(SETTINGS, BN_CLICKED);
        assert!(
            matches!(rx.try_recv(), Ok(Action::Tracked { action, .. }) if matches!(*action, Action::AdvancedVisibility(true)))
        );
        assert!(
            !snapshot().unwrap().settings_visible,
            "visibility waits for persistence"
        );
        {
            let mut state = shared.lock().unwrap();
            state.config.advanced_settings_visible = true;
            state.commands.settings_pending = false;
            state.revision += 1;
        }
        refresh();
        for id in SETTINGS_ROW {
            assert_ne!(
                unsafe { GetWindowLongPtrW(GetDlgItem(hwnd, id), GWL_STYLE) } as u32 & WS_VISIBLE,
                0
            );
        }
        command(DOCTOR, BN_CLICKED);
        assert!(
            matches!(rx.try_recv(), Ok(Action::Tracked { action, .. }) if matches!(*action, Action::Doctor))
        );
        command(DOCTOR, BN_CLICKED);
        assert!(rx.try_recv().is_err(), "pending diagnostics must coalesce");
        refresh();
        assert_eq!(unsafe { IsWindowEnabled(GetDlgItem(hwnd, DOCTOR)) }, 0);
        {
            let mut state = shared.lock().unwrap();
            let provider = state
                .config
                .providers
                .iter()
                .find(|provider| matches!(provider, crate::config::Provider::LMStudio { .. }))
                .unwrap();
            let connection = state.config.lm().unwrap().clone();
            state.doctor_report = Some(serde_json::json!({"providers":[{
                "id":provider.id(),"endpoint":provider.endpoint(),"enabled":provider.enabled(),
                "connection":connection,"observed_at_unix_seconds":0,
                "cli_version":{"ok":false,"error":"fixture CLI unavailable"}
            }]}));
            state.doctor_pending = false;
        }
        refresh();
        assert_ne!(unsafe { IsWindowEnabled(GetDlgItem(hwnd, DOCTOR)) }, 0);
        let cached_detail = text(unsafe { GetDlgItem(hwnd, PROVIDER_DETAIL) });
        assert!(cached_detail.contains("fixture CLI unavailable"));
        assert!(cached_detail.contains("1970-01-01 00:00:00 UTC"));
        refresh();
        assert_eq!(
            text(unsafe { GetDlgItem(hwnd, PROVIDER_DETAIL) }),
            cached_detail
        );
        unsafe {
            SendMessageW(
                GetDlgItem(hwnd, NOTIFICATIONS),
                BM_SETCHECK,
                BST_UNCHECKED as usize,
                0,
            );
        }
        command(NOTIFICATIONS, BN_CLICKED);
        assert!(
            matches!(rx.try_recv(), Ok(Action::Tracked { action, .. }) if matches!(*action, Action::NotificationPreferences { visual:false, sound:true }))
        );
        assert!(shared.lock().unwrap().config.notifications_enabled);
        assert!(
            checked(unsafe { GetDlgItem(hwnd, NOTIFICATIONS) }),
            "pending save must show saved preference"
        );
        unsafe {
            SetFocus(GetDlgItem(hwnd, SAVE));
        }
        {
            let mut state = shared.lock().unwrap();
            state.config.advanced_settings_visible = false;
            state.commands.settings_pending = false;
        }
        refresh();
        assert_eq!(unsafe { GetFocus() }, unsafe { GetDlgItem(hwnd, SETTINGS) });
        let mut tab = unsafe { GetDlgItem(hwnd, SETTINGS) };
        for _ in 0..LAYOUT.len() {
            tab = unsafe { GetNextDlgTabItem(hwnd, tab, 0) };
            assert!(
                !SETTINGS_ROW.contains(&unsafe { GetDlgCtrlID(tab) }),
                "hidden tool remained in tab order"
            );
        }
        // Prime both themes at all font DPIs. The expanded native drawing paths
        // take more than the old single-theme 49 relayouts to reach a plateau.
        for (index, dpi) in [96, 144, 192, 144, 96, 192, 96]
            .into_iter()
            .cycle()
            .take(98)
            .enumerate()
        {
            shared.lock().unwrap().config.appearance = if index % 2 == 0 {
                crate::config::Appearance::Dark
            } else {
                crate::config::Appearance::Light
            };
            set_client_theme(index % 2 == 0);
            relayout(hwnd, dpi, false);
            pump();
        }
        let before = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        for (index, dpi) in [96, 144, 192, 144, 96, 192, 96]
            .into_iter()
            .cycle()
            .take(49)
            .enumerate()
        {
            shared.lock().unwrap().config.appearance = if index % 2 == 0 {
                crate::config::Appearance::Dark
            } else {
                crate::config::Appearance::Light
            };
            set_client_theme(index % 2 == 0);
            relayout(hwnd, dpi, false);
            pump();
            let state = snapshot().unwrap();
            assert_eq!(
                unsafe { SendMessageW(state.tooltips.hwnd, TTM_GETTIPBKCOLOR, 0, 0) } as u32,
                state.theme.as_ref().unwrap().palette.surface
            );
            let mut font: LOGFONTW = unsafe { std::mem::zeroed() };
            unsafe {
                assert_ne!(
                    GetObjectW(
                        state.font,
                        std::mem::size_of::<LOGFONTW>() as i32,
                        &mut font as *mut _ as *mut _
                    ),
                    0
                );
                assert_eq!(font.lfHeight, -scale(16, dpi));
                assert_eq!(
                    SendMessageW(GetDlgItem(hwnd, LIST), LB_GETITEMHEIGHT, 0, 0),
                    row_height(dpi) as isize
                );
            }
        }
        theme_changed();
        pump();
        let after = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        eprintln!("GDI after 98 warmups / 49 measured theme-DPI relayouts: {before} / {after}");
        close();
        assert!(
            after <= before + 2,
            "GDI handles leaked: {before} -> {after}"
        );
    }
}
