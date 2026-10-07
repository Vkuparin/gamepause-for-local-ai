//! State the watcher publishes for the tray and dashboard, the actions they send
//! back, and the helpers that track each request until the control thread answers.

use crate::{
    commands::{Commands, Outcome},
    config::Config,
    control::{Activity, ControlState, CoreCommand},
    discovery::Game,
    gameplay::{RestoreFeedback, RestoreOffer},
    processes::{ActiveGame, RunningApp},
};
use serde_json::Value;
use std::sync::{Arc, Mutex, mpsc};
#[derive(Clone, Debug)]
pub enum Action {
    Tracked {
        id: u64,
        action: Box<Action>,
    },
    RemoveCustom {
        path: String,
        name: String,
    },
    Pause,
    /// Answer to an "ask" game: pause for it as for any other game, and
    /// restore normally when it exits.
    PauseForGame {
        games: Vec<ActiveGame>,
    },
    Resume,
    Restore,
    ConfirmedRestore {
        offer_id: u64,
        ignored: Vec<String>,
    },
    RetryGameplayRestore,
    ReplaceLMLoadout(crate::lmstudio::LoadoutOffer),
    Disable,
    Refresh,
    Verify,
    Doctor,
    PowerChanged,
    Quit,
    Settings(Box<Config>),
    AdvancedSettings(Box<Config>),
    AdvancedVisibility(bool),
    NotificationPreferences {
        visual: bool,
        sound: bool,
    },
    Appearance(crate::config::Appearance),
}
impl Action {
    /// The user chose to pause for the running "ask" games.
    pub(super) fn ask_answer(&self) -> Option<&[ActiveGame]> {
        match self {
            Action::Tracked { action, .. } => action.ask_answer(),
            Action::PauseForGame { games } => Some(games),
            _ => None,
        }
    }
    pub(super) fn changes_settings(&self) -> bool {
        matches!(
            self,
            Action::Settings(_)
                | Action::AdvancedSettings(_)
                | Action::AdvancedVisibility(_)
                | Action::NotificationPreferences { .. }
                | Action::Appearance(_)
                | Action::Disable
                | Action::RemoveCustom { .. }
        ) || matches!(self, Action::ConfirmedRestore { ignored, .. } if !ignored.is_empty())
    }
}
#[derive(Clone, Default)]
pub struct Shared {
    pub commands: Commands,
    pub restore_offer: Option<RestoreOffer>,
    pub lm_loadout_offer: Option<crate::lmstudio::LoadoutOffer>,
    pub coexistence: bool,
    pub restore_feedback: Option<RestoreFeedback>,
    pub activity: Activity,
    pub detection_ok: bool,
    pub message: String,
    pub disabled: bool,
    pub manual_pause: bool,
    pub pending: bool,
    pub active_mode: bool,
    pub config: Config,
    pub games: Vec<Game>,
    pub active_games: Vec<ActiveGame>,
    pub running_apps: Vec<RunningApp>,
    pub discovery_errors: std::collections::BTreeMap<String, String>,
    pub settings_error: String,
    pub revision: u64,
    pub discovery_ready: bool,
    /// P2-1: the latest round-trip verify report, for the dashboard/CLI to
    /// render. `None` until a verify has been run.
    pub verify_report: Option<crate::engine::VerifyReport>,
    pub verifying: bool,
    pub pause_completions: u64,
    pub restore_completions: u64,
    pub provider_statuses: Vec<crate::coordinator::Report>,
    pub doctor_report: Option<Value>,
    pub doctor_pending: bool,
    /// LM Studio is enabled but its CLI was not found; shown as not installed.
    pub lm_missing: bool,
    /// LM Studio and Ollama processes seen by the last accepted scan. The
    /// services are not contacted while idle, so this is all "running" means.
    pub lm_running: bool,
    pub ollama_running: bool,
    /// Ollama's program file was found on this PC, running or not.
    pub ollama_installed: bool,
    /// Approximate model memory released by the current pause; 0 if unknown.
    pub freed_bytes: u64,
    /// Running games whose rule is "ask" and that have not been answered.
    pub ask_prompt: Vec<String>,
    /// Executable path of a program that looks like an unregistered game.
    pub suggestion: Option<String>,
    pub power: Arc<crate::power::Signal>,
}

pub fn local_result(state: &SharedState, outcome: Outcome, message: impl Into<String>) {
    if let Ok(mut shared) = state.lock() {
        shared.settings_error.clear();
        shared.commands.local(outcome, message);
    }
}

pub fn request_action(state: &SharedState, tx: &mpsc::Sender<Action>, action: Action, label: &str) {
    if let Ok(mut shared) = state.lock() {
        let settings = action.changes_settings();
        let doctor = matches!(action, Action::Doctor);
        if doctor {
            if !shared.config.advanced_settings_visible {
                shared.commands.local(
                    Outcome::Failed,
                    "Advanced settings is hidden; show it before using diagnostics.",
                );
                return;
            }
            if shared.doctor_pending {
                shared.commands.local(
                    Outcome::NoChange,
                    "Read-only diagnostics are already running.",
                );
                return;
            }
            shared.doctor_pending = true;
        }
        if settings && shared.commands.settings_pending {
            shared.commands.local(
                Outcome::NoChange,
                "A settings change is still being saved. Wait before changing another preference.",
            );
            return;
        }
        shared.settings_error.clear();
        let refresh = matches!(action, Action::Refresh);
        let id = if refresh {
            let Some(id) = shared.commands.request_refresh() else {
                return;
            };
            id
        } else {
            shared.commands.begin(format!("{label} requested."))
        };
        shared.commands.settings_pending |= settings;
        if tx
            .send(Action::Tracked {
                id,
                action: Box::new(action),
            })
            .is_err()
        {
            shared.doctor_pending &= !doctor;
            shared.commands.settings_pending &= !settings;
            if refresh {
                shared
                    .commands
                    .refresh_failed(id, "The discovery worker is unavailable.".into());
            } else {
                shared
                    .commands
                    .update(id, Outcome::Failed, "The control worker is unavailable.");
            }
        }
    }
}
pub type SharedState = Arc<Mutex<Shared>>;
pub fn request_quit(state: &SharedState, tx: &mpsc::Sender<Action>) {
    if tx.send(Action::Quit).is_err() {
        local_result(
            state,
            Outcome::Failed,
            "The control worker is unavailable; Quit could not be delivered.",
        );
    }
}
impl Shared {
    pub fn provider_pending(&self, kind: crate::provider::Kind) -> bool {
        (self.pending && self.provider_statuses.is_empty())
            || self
                .provider_statuses
                .iter()
                .any(|report| report.kind == kind && report.pending)
    }
    pub fn controls(&self) -> ControlState {
        ControlState {
            activity: if self.verifying {
                Activity::Verifying
            } else {
                self.activity
            },
            active_mode: self.active_mode,
            provider_enabled: self.config.any_provider_enabled(),
            detection_ready: self.discovery_ready
                && self.detection_ok
                && !self.disabled
                && self.discovery_errors.is_empty(),
            gaming: !self.active_games.is_empty(),
            pending: self.pending,
            manual_hold: self.manual_pause,
            gameplay_restore: self.restore_offer.is_some() || self.coexistence,
            coexistence: self.coexistence,
        }
    }
}

pub fn request_core(state: &SharedState, tx: &mpsc::Sender<Action>, command: CoreCommand) {
    if let Ok(mut shared) = state.lock() {
        let availability = shared.controls().availability();
        if !availability.allows(command) {
            shared.settings_error.clear();
            shared.commands.local(Outcome::Failed, availability.reason);
            return;
        }
        let action = match command {
            CoreCommand::Pause => Action::Pause,
            CoreCommand::Resume => Action::Resume,
            CoreCommand::Restore => Action::Restore,
        };
        drop(shared);
        request_action(
            state,
            tx,
            action,
            match command {
                CoreCommand::Pause => "Pause AI",
                CoreCommand::Resume => "Resume AI",
                CoreCommand::Restore => "Resume AI",
            },
        );
    }
}
pub fn request_verify(state: &SharedState, tx: &mpsc::Sender<Action>) {
    if let Ok(mut shared) = state.lock() {
        if shared.verifying
            || !shared.config.advanced_settings_visible
            || !shared.active_mode
            || !shared.config.any_provider_enabled()
            || shared.disabled
            || shared.pending
            || shared.manual_pause
            || !shared.discovery_ready
            || !shared.detection_ok
            || shared.activity.busy()
            || !shared.active_games.is_empty()
            || !shared.discovery_errors.is_empty()
        {
            shared.settings_error.clear();
            shared.commands.local(Outcome::Failed, "Round-trip unavailable: finish recovery, close games, enable active detection, and wait for discovery.");
            return;
        }
        shared.settings_error.clear();
        shared.verifying = true;
        let id = shared.commands.begin("Test round-trip requested.");
        if tx
            .send(Action::Tracked {
                id,
                action: Box::new(Action::Verify),
            })
            .is_err()
        {
            shared.verifying = false;
            shared
                .commands
                .update(id, Outcome::Failed, "The control worker is unavailable.");
        }
    }
}
