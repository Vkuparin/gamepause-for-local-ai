//! Shared native gameplay confirmation. No UI/shared borrow survives a Win32 call.
use crate::{
    app::{Action, SharedState},
    commands::Outcome,
    control::CoreCommand,
    gameplay::RestoreOffer,
    wide,
};
use anyhow::{Context, Result, bail};
use std::{
    cell::Cell,
    ptr::{null, null_mut},
    sync::mpsc::Sender,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

const WARNING_TEXT: &str = "Loading AI while these games are running may use more VRAM than is available.\r\nResume AI temporarily allows AI to run alongside the games listed below.\r\nThis permission ends when a listed game exits or restarts, another game that is not ignored starts, you pause AI, or you restart GamePause.";
const IGNORE_HELP: &str = "Optional: select games to ignore in future. No games are selected by default.\r\nResume AI works without selecting any games. Cancel leaves AI and settings unchanged.";
const CONFIRM: i32 = 1;
const CANCEL: i32 = 2;
const GAMES: i32 = 101;
thread_local! { static OPEN: Cell<bool> = const { Cell::new(false) }; }
struct DialogOpen;
impl Drop for DialogOpen {
    fn drop(&mut self) {
        OPEN.with(|open| open.set(false));
    }
}
struct DialogState {
    done: Cell<bool>,
    accepted: Cell<bool>,
}

pub fn resume_label(gaming: bool, coexisting: bool, pending: bool) -> &'static str {
    if coexisting && pending {
        "Retry resume"
    } else if gaming && pending {
        "Resume AI..."
    } else {
        "Resume AI"
    }
}

/// Both native surfaces call this flow. The worker alone accepts offer IDs.
pub fn request(parent: HWND, state: &SharedState, tx: &Sender<Action>) {
    let Ok(snapshot) = state.lock().map(|shared| shared.clone()) else {
        return;
    };
    let availability = snapshot.controls().availability();
    if !snapshot.pending && availability.resume {
        crate::app::request_core(state, tx, CoreCommand::Resume);
        return;
    }
    if !availability.restore {
        crate::app::local_result(state, Outcome::Failed, availability.reason);
        return;
    }
    if snapshot.coexistence {
        crate::app::request_action(state, tx, Action::RetryGameplayRestore, "Resume AI retry");
        return;
    }
    if snapshot.active_games.is_empty() {
        crate::app::request_core(state, tx, CoreCommand::Restore);
        return;
    }
    let Some(offer) = snapshot.restore_offer else {
        return;
    };
    if OPEN.with(|open| open.replace(true)) {
        return;
    }
    let _open = DialogOpen;
    let result = show(parent, &offer);
    match result {
        Ok(Some(ignored)) => crate::app::request_action(
            state,
            tx,
            Action::ConfirmedRestore {
                offer_id: offer.id,
                ignored,
            },
            "Resume AI during gameplay",
        ),
        Ok(None) => crate::app::local_result(
            state,
            Outcome::Cancelled,
            "Resume cancelled; AI and preferences unchanged.",
        ),
        Err(error) => crate::app::local_result(
            state,
            Outcome::Failed,
            format!("Could not open Resume confirmation: {error:#}"),
        ),
    }
}

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        let context = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const DialogState;
        match message {
            WM_COMMAND
                if !context.is_null() && [CONFIRM, CANCEL].contains(&((w & 0xffff) as i32)) =>
            {
                (*context).accepted.set((w & 0xffff) as i32 == CONFIRM);
                (*context).done.set(true);
                0
            }
            WM_CLOSE | WM_DESTROY if !context.is_null() => {
                (*context).done.set(true);
                0
            }
            DM_GETDEFID => (DC_HASDEFID as isize) << 16 | CANCEL as isize,
            _ => DefWindowProcW(hwnd, message, w, l),
        }
    }
}

fn selected_paths(offer: &RestoreOffer, checked: impl IntoIterator<Item = bool>) -> Vec<String> {
    let mut selected = Vec::new();
    for (game, checked) in offer.games.iter().zip(checked) {
        if checked
            && !selected.iter().any(|path: &String| {
                crate::discovery::canonical(path) == crate::discovery::canonical(&game.executable)
            })
        {
            selected.push(game.executable.clone());
        }
    }
    selected
}

fn show(parent: HWND, offer: &RestoreOffer) -> Result<Option<Vec<String>>> {
    unsafe {
        let controls = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES,
        };
        if InitCommonControlsEx(&controls) == 0 {
            bail!("Could not initialize native game list");
        }
        let instance = GetModuleHandleW(null());
        let class = wide("GamePauseGameplayRestore");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
            ..std::mem::zeroed()
        };
        if RegisterClassW(&wc) == 0 && GetLastError() != 1410 {
            bail!("Could not register Restore confirmation");
        }
        let dpi = if parent.is_null() {
            GetDpiForSystem()
        } else {
            GetDpiForWindow(parent)
        }
        .max(96) as i32;
        let scale = |value: i32| value * dpi / 96;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: scale(600),
            bottom: scale(390),
        };
        AdjustWindowRectEx(
            &mut rect,
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            0,
            WS_EX_DLGMODALFRAME,
        );
        let hwnd = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            class.as_ptr(),
            wide("Resume AI during gameplay?").as_ptr(),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            (GetSystemMetrics(SM_CXSCREEN) - (rect.right - rect.left)) / 2,
            (GetSystemMetrics(SM_CYSCREEN) - (rect.bottom - rect.top)) / 2,
            rect.right - rect.left,
            rect.bottom - rect.top,
            parent,
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            bail!("Could not create Restore confirmation");
        }
        let context = DialogState {
            done: Cell::new(false),
            accepted: Cell::new(false),
        };
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, &context as *const _ as isize);
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
        let child = |class: &str, label: &str, style: u32, id: i32, x, y, width, height| {
            let control = CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(label).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                scale(x),
                scale(y),
                scale(width),
                scale(height),
                hwnd,
                id as usize as HMENU,
                instance,
                null(),
            );
            if !control.is_null() {
                SendMessageW(control, WM_SETFONT, font as usize, 1);
            }
            control
        };
        let warning = child("STATIC", WARNING_TEXT, 0, 100, 16, 12, 568, 96);
        let description = child("STATIC", IGNORE_HELP, 0, 102, 16, 116, 568, 68);
        let list = child(
            "SysListView32",
            "Running games",
            WS_TABSTOP | LVS_REPORT | LVS_NOCOLUMNHEADER,
            GAMES,
            16,
            192,
            568,
            140,
        );
        SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            LVS_EX_CHECKBOXES as usize,
            LVS_EX_CHECKBOXES as isize,
        );
        let mut column: LVCOLUMNW = std::mem::zeroed();
        column.mask = LVCF_WIDTH;
        column.cx = scale(548);
        let column_created =
            SendMessageW(list, LVM_INSERTCOLUMNW, 0, &column as *const _ as isize) >= 0;
        let mut rows_created = true;
        for (index, game) in offer.games.iter().enumerate() {
            let mut label = wide(&format!(
                "{} — {} (PID {})",
                game.game, game.executable, game.pid
            ));
            let mut item: LVITEMW = std::mem::zeroed();
            item.mask = LVIF_TEXT | LVIF_STATE;
            item.iItem = index as i32;
            item.pszText = label.as_mut_ptr();
            item.stateMask = LVIS_STATEIMAGEMASK;
            item.state = 1 << 12; // native checkbox image 1: unchecked
            rows_created &= SendMessageW(list, LVM_INSERTITEMW, 0, &item as *const _ as isize)
                == index as isize;
        }
        let cancel = child(
            "BUTTON",
            "Cancel",
            WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
            CANCEL,
            298,
            350,
            110,
            28,
        );
        let confirm = child(
            "BUTTON",
            "Resume AI",
            WS_TABSTOP | BS_PUSHBUTTON as u32,
            CONFIRM,
            420,
            350,
            164,
            28,
        );
        let complete = [warning, description, list, cancel, confirm]
            .iter()
            .all(|control| !control.is_null())
            && column_created
            && rows_created
            && SendMessageW(list, LVM_GETITEMCOUNT, 0, 0) == offer.games.len() as isize;
        let parent_enabled = !parent.is_null() && IsWindowEnabled(parent) != 0;
        if complete {
            crate::tray::apply_theme(hwnd);
            if parent_enabled {
                EnableWindow(parent, 0);
            }
            ShowWindow(hwnd, SW_SHOW);
            SetFocus(cancel);
        }
        let result = (|| -> Result<Option<Vec<String>>> {
            if !complete {
                bail!("Could not create Restore confirmation controls");
            }
            let mut msg: MSG = std::mem::zeroed();
            while !context.done.get() {
                let status = GetMessageW(&mut msg, null_mut(), 0, 0);
                if status == 0 {
                    PostQuitMessage(msg.wParam as i32);
                    return Ok(None);
                }
                if status < 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("Restore dialog message loop failed");
                }
                if IsDialogMessageW(hwnd, &msg) == 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            if !context.accepted.get() {
                return Ok(None);
            }
            let checked = (0..offer.games.len()).map(|index| {
                SendMessageW(list, LVM_GETITEMSTATE, index, LVIS_STATEIMAGEMASK as isize)
                    & LVIS_STATEIMAGEMASK as isize
                    == 2 << 12
            });
            Ok(Some(selected_paths(offer, checked)))
        })();
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
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::Shared, control::Activity, gameplay::fixtures::game};
    use std::sync::{Arc, Mutex, mpsc};
    #[test]
    fn rewritten_help_fits_gameplay_popup_at_supported_dpi() {
        unsafe {
            let dc = CreateCompatibleDC(null_mut());
            for dpi in [96, 144, 192] {
                let font = CreateFontW(
                    -crate::dashboard::scale(14, dpi),
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
                let old = SelectObject(dc, font);
                for (value, height) in [(WARNING_TEXT, 96), (IGNORE_HELP, 68)] {
                    let mut rect = RECT {
                        left: 0,
                        top: 0,
                        right: crate::dashboard::scale(568, dpi),
                        bottom: 0,
                    };
                    DrawTextW(
                        dc,
                        wide(value).as_ptr(),
                        -1,
                        &mut rect,
                        DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
                    );
                    assert!(
                        rect.bottom <= crate::dashboard::scale(height, dpi),
                        "help exceeds its control at {dpi} DPI"
                    );
                }
                SelectObject(dc, old);
                DeleteObject(font);
            }
            DeleteDC(dc);
        }
    }

    #[test]
    fn resume_uses_immediate_recovery_or_releases_an_empty_hold_and_blocks_busy_work() {
        for (pending, manual, busy, expected) in [
            (true, true, false, Some(CoreCommand::Restore)),
            (true, false, false, Some(CoreCommand::Restore)),
            (false, true, false, Some(CoreCommand::Resume)),
            (false, false, false, None),
            (true, true, true, None),
        ] {
            let shared = Arc::new(Mutex::new(Shared {
                active_mode: true,
                discovery_ready: true,
                detection_ok: true,
                pending,
                manual_pause: manual,
                activity: if busy {
                    Activity::Restoring
                } else {
                    Activity::Watching
                },
                ..Default::default()
            }));
            let (tx, rx) = mpsc::channel();
            request(null_mut(), &shared, &tx);
            match expected {
                Some(command) => {
                    let Action::Tracked { action, .. } = rx.try_recv().unwrap() else {
                        panic!("untracked request")
                    };
                    assert!(matches!(
                        (command, *action),
                        (CoreCommand::Restore, Action::Restore)
                            | (CoreCommand::Resume, Action::Resume)
                    ));
                }
                None => assert!(rx.try_recv().is_err()),
            }
        }
        assert_eq!(resume_label(false, false, true), "Resume AI");
        assert_eq!(resume_label(true, false, true), "Resume AI...");
        assert_eq!(resume_label(true, true, true), "Retry resume");
    }

    #[test]
    fn default_is_cancel_and_only_checked_executables_are_selected() {
        assert_eq!(
            unsafe { procedure(null_mut(), DM_GETDEFID, 0, 0) } & 0xffff,
            CANCEL as isize
        );
        let offer = RestoreOffer {
            id: 1,
            games: vec![game(42, 10), game(7, 2)],
        };
        assert!(selected_paths(&offer, [false, false]).is_empty());
        assert_eq!(
            selected_paths(&offer, [false, true]),
            vec![offer.games[1].executable.clone()]
        );
    }
    #[test]
    fn modal_reentry_cannot_open_another_confirmation_or_enqueue_work() {
        let game = game(42, 10);
        let state = Arc::new(Mutex::new(Shared {
            activity: Activity::Paused,
            active_mode: true,
            discovery_ready: true,
            detection_ok: true,
            pending: true,
            active_games: vec![game.clone()],
            restore_offer: Some(RestoreOffer {
                id: 1,
                games: vec![game],
            }),
            ..Default::default()
        }));
        let (tx, rx) = mpsc::channel();
        OPEN.with(|open| open.set(true));
        let guard = DialogOpen;
        request(null_mut(), &state, &tx);
        assert!(rx.try_recv().is_err());
        assert!(state.lock().unwrap().restore_feedback.is_none());
        drop(guard);
        assert!(!OPEN.with(Cell::get));
    }
}
