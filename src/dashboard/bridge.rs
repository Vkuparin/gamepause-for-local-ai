//! The on-demand dashboard UI thread and the mailbox other threads reach it by.
//! A closed dashboard has no window or renderer; its thread blocks on the mailbox.
//! `BRIDGE` is never held across renderer or native dispatch.

use super::{Dashboard, Modal, native::native_options};
use crate::{
    app::{Action, SharedState},
    commands::Outcome,
    dashboard_theme as design, tray,
};
use eframe::egui::Context;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
    },
    time::{Duration, Instant},
};
use windows_sys::Win32::Foundation::HWND;
pub(super) static RUNNING_REQUESTED: AtomicBool = AtomicBool::new(false);
pub(super) static UI_VISIBLE: AtomicBool = AtomicBool::new(false);
pub(super) static UI_STOP: AtomicBool = AtomicBool::new(false);
pub(super) static BRIDGE: Mutex<Option<Bridge>> = Mutex::new(None);
pub(super) struct Bridge {
    pub(super) tx: Sender<UiRequest>,
    pub(super) ctx: Option<Context>,
    pub(super) shared: SharedState,
    pub(super) folder: PathBuf,
    pub(super) fingerprint: String,
    pub(super) thread: Option<std::thread::JoinHandle<()>>,
    /// The open dashboard window, or 0. Used only by the tray UI thread to
    /// bring a minimized window back; never handed to a worker.
    pub(super) window: isize,
}
pub(super) enum UiRequest {
    Show,
    Resume,
    Verify,
    Theme,
    Stop,
}
pub fn needs_running_apps() -> bool {
    RUNNING_REQUESTED.load(Ordering::Relaxed)
}
pub fn theme_changed() {
    dispatch(UiRequest::Theme);
}
/// A minimized window draws no frames, and requests are read while drawing:
/// without this, Show, Resume and Stop would wait until someone restored it.
/// A fullscreen game minimizes the dashboard, so that is the usual case.
fn wake_window(command: i32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, ShowWindowAsync};
    let window = BRIDGE
        .lock()
        .ok()
        .and_then(|bridge| bridge.as_ref().map(|b| b.window))
        .unwrap_or(0) as HWND;
    // No lock is held here, and the call does not wait for the UI thread.
    if !window.is_null() && unsafe { IsIconic(window) } != 0 {
        unsafe {
            ShowWindowAsync(window, command);
        }
    }
}
pub(super) fn dispatch(request: UiRequest) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_RESTORE, SW_SHOWNOACTIVATE};
    let wake = match request {
        UiRequest::Show | UiRequest::Resume | UiRequest::Verify => Some(SW_RESTORE),
        UiRequest::Stop => Some(SW_SHOWNOACTIVATE),
        UiRequest::Theme => None,
    };
    if let Some(command) = wake {
        wake_window(command);
    }
    let ctx = BRIDGE
        .lock()
        .ok()
        .and_then(|bridge| {
            bridge.as_ref().map(|b| {
                let _ = b.tx.send(request);
                b.ctx.clone()
            })
        })
        .flatten();
    if let Some(ctx) = ctx {
        ctx.request_repaint();
    }
}
pub fn close() {
    UI_STOP.store(true, Ordering::Relaxed);
    wake_window(windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE);
    let bridge = BRIDGE.lock().ok().and_then(|mut b| b.take());
    if let Some(mut bridge) = bridge {
        let _ = bridge.tx.send(UiRequest::Stop);
        if let Some(ctx) = bridge.ctx {
            ctx.request_repaint();
        }
        if let Some(thread) = bridge.thread.take() {
            // Quit must not hang on a renderer that draws no frame. The
            // process is exiting; a thread that has not stopped is left to it.
            let deadline = Instant::now() + Duration::from_secs(3);
            while !thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
    RUNNING_REQUESTED.store(false, Ordering::Relaxed);
    UI_VISIBLE.store(false, Ordering::Relaxed);
}
/// Called by the existing tray timer. Unchanged state does not redraw the GPU window.
pub fn refresh() {
    if !UI_VISIBLE.load(Ordering::Relaxed) {
        return;
    }
    let shared = BRIDGE
        .lock()
        .ok()
        .and_then(|b| b.as_ref().map(|b| (b.shared.clone(), b.folder.clone())));
    let Some((shared, folder)) = shared else {
        return;
    };
    let Ok(s) = shared.lock().map(|s| s.clone()) else {
        return;
    };
    let fingerprint = fingerprint(&s) + &format!("|{:?}", crate::app::log_error(&folder));
    let ctx = BRIDGE.lock().ok().and_then(|mut b| {
        b.as_mut().and_then(|b| {
            if b.fingerprint == fingerprint {
                return None;
            }
            b.fingerprint = fingerprint;
            b.ctx.clone()
        })
    });
    if let Some(ctx) = ctx {
        ctx.request_repaint();
    }
}
pub(super) fn fingerprint(s: &crate::app::Shared) -> String {
    let summary = crate::presentation::summarize(s);
    format!(
        "{}|{}|{}|{:?}|{:?}|{}|{:?}|{:?}|{:?}|{:?}|{}|{}|{}|{}|{:?}",
        summary.games,
        summary.ai_text(),
        s.revision,
        s.commands.latest,
        s.running_apps,
        s.settings_error,
        s.restore_offer,
        s.verify_report,
        s.doctor_report,
        s.provider_statuses,
        s.commands.settings_pending,
        s.doctor_pending,
        s.verifying,
        s.discovery_ready,
        s.games
    ) + &format!(
        "|{:?}|{:?}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        s.suggestion,
        s.ask_prompt,
        s.freed_bytes,
        s.lm_missing,
        s.lm_running,
        s.ollama_running,
        s.ollama_installed,
        s.pending,
        s.manual_pause,
        s.coexistence,
        s.active_mode
    )
}
pub fn show(shared: SharedState, tx: Sender<Action>, folder: PathBuf) {
    if BRIDGE.lock().is_ok_and(|b| {
        b.as_ref()
            .is_some_and(|b| b.thread.as_ref().is_some_and(|t| t.is_finished()))
    }) {
        close();
    }
    if BRIDGE.lock().is_ok_and(|b| b.is_some()) {
        dispatch(UiRequest::Show);
        return;
    }
    let (ui_tx, rx) = mpsc::channel();
    UI_STOP.store(false, Ordering::Relaxed);
    let rx = Arc::new(Mutex::new(rx));
    let Ok(mut bridge) = BRIDGE.lock() else {
        return;
    };
    *bridge = Some(Bridge {
        tx: ui_tx,
        ctx: None,
        shared: shared.clone(),
        folder: folder.clone(),
        fingerprint: String::new(),
        thread: None,
        window: 0,
    });
    let thread = std::thread::Builder::new()
        .name("gamepause-ui".into())
        .spawn(move || {
            let mut initial = Some(UiRequest::Show);
            loop {
                let request = initial
                    .take()
                    .or_else(|| rx.lock().ok().and_then(|rx| rx.recv().ok()));
                let Some(request) = request else { break };
                if matches!(request, UiRequest::Stop) {
                    break;
                }
                if matches!(request, UiRequest::Theme) {
                    continue;
                }
                let error_state = shared.clone();
                let app_shared = shared.clone();
                let app_tx = tx.clone();
                let app_folder = folder.clone();
                let app_rx = rx.clone();
                let result = eframe::run_native(
                    "GamePause for Local AI",
                    native_options(),
                    Box::new(move |cc| {
                        design::fonts(&cc.egui_ctx);
                        if let Ok(mut b) = BRIDGE.lock()
                            && let Some(b) = b.as_mut()
                        {
                            b.ctx = Some(cc.egui_ctx.clone());
                        }
                        let mut app = Dashboard::new(app_shared, app_tx, app_folder, app_rx);
                        let s = app.shared.lock().map(|s| s.clone()).unwrap_or_default();
                        match request {
                            UiRequest::Resume => app.resume(&s),
                            UiRequest::Verify if crate::ui_commands::verify_available(&s) => {
                                app.modal = Some(Modal::Verify)
                            }
                            _ => (),
                        }
                        Ok(Box::new(app))
                    }),
                );
                if let Ok(mut b) = BRIDGE.lock()
                    && let Some(b) = b.as_mut()
                {
                    b.ctx = None;
                    b.window = 0;
                }
                UI_VISIBLE.store(false, Ordering::Relaxed);
                RUNNING_REQUESTED.store(false, Ordering::Relaxed);
                if let Err(error) = result {
                    crate::app::local_result(
                        &error_state,
                        Outcome::Failed,
                        format!("Could not open dashboard: {error}"),
                    );
                    crate::app::log(&folder, &format!("Dashboard renderer failed: {error}"));
                }
                if UI_STOP.load(Ordering::Relaxed) {
                    break;
                }
                // The same thread reuses eframe's thread-local Windows event loop.
                // A closed dashboard has no window or renderer and blocks on the mailbox.
            }
        });
    match thread {
        Ok(thread) => {
            if let Some(b) = bridge.as_mut() {
                b.thread = Some(thread);
            }
        }
        Err(error) => {
            *bridge = None;
            drop(bridge);
            tray::error(&format!("Could not start dashboard: {error}"));
        }
    }
}
/// The native tray routes gameplay confirmation into the same themed modal.
pub fn request_resume(shared: SharedState, tx: Sender<Action>, folder: PathBuf) {
    show(shared, tx, folder);
    dispatch(UiRequest::Resume);
}
pub fn request_verify_modal(shared: SharedState, tx: Sender<Action>, folder: PathBuf) {
    show(shared, tx, folder);
    dispatch(UiRequest::Verify);
}
