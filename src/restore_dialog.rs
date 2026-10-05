//! Resume routing shared by the native tray and Rust dashboard.
use crate::{
    app::{Action, SharedState},
    commands::Outcome,
    control::CoreCommand,
};
use std::sync::mpsc::Sender;
use windows_sys::Win32::Foundation::HWND;

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
pub fn request(_parent: HWND, state: &SharedState, tx: &Sender<Action>) {
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
    crate::dashboard::request_resume(state.clone(), tx.clone());
}
