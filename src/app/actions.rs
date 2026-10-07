//! Applies one queued action on the control thread: settings, game registration
//! and the manual pause/restore commands. Tracked requests are completed or
//! rejected in one place, around the action itself.

use super::{
    Action, OptionalBackend, SharedState, confirmed_restore,
    detection::{BackgroundDetection, action_evidence, recovery_scanner},
    recovery_games, restore_outcome,
};
use crate::{
    commands::{Outcome, Waiting},
    config::{Config, write_json},
    control::{ControlState, CoreCommand},
    discovery::Game,
    engine::Engine,
    gameplay::{GameEvidence, RestoreFeedback},
    processes::Scanner,
};
use anyhow::{Context, Result, bail};
#[cfg(test)]
pub(super) fn apply_action(
    action: Action,
    engine: &mut Engine<OptionalBackend>,
    folder: &std::path::Path,
    scanner: &mut Scanner,
    games: &[Game],
    now: f64,
    state: &SharedState,
) -> Result<bool> {
    apply_action_detected(action, engine, folder, scanner, games, now, state, None)
}
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_action_detected(
    action: Action,
    engine: &mut Engine<OptionalBackend>,
    folder: &std::path::Path,
    scanner: &mut Scanner,
    games: &[Game],
    now: f64,
    state: &SharedState,
    detection: Option<&BackgroundDetection>,
) -> Result<bool> {
    if let Action::Tracked { id, action } = action {
        let action = *action;
        let settings = action.changes_settings();
        let refresh = matches!(action, Action::Refresh);
        let before = engine.config.clone();
        let waiting = match action {
            Action::Pause => Some(Waiting::Pause),
            Action::Restore | Action::ConfirmedRestore { .. } | Action::RetryGameplayRestore => {
                Some(Waiting::Restore)
            }
            _ => None,
        };
        let success = match &action {
            Action::RemoveCustom { name, .. } => format!("Removed custom game \"{name}\"; saved."),
            Action::Settings(_)
            | Action::AdvancedSettings(_)
            | Action::AdvancedVisibility(_)
            | Action::NotificationPreferences { .. }
            | Action::Appearance(_)
            | Action::Disable => "Settings saved.".into(),
            Action::Resume => {
                "Manual hold released; normal recovery guards and delay apply.".into()
            }
            Action::Doctor => {
                "Read-only diagnostics finished; see Advanced provider details.".into()
            }
            Action::PauseForGame => "Pausing AI for this game.".into(),
            _ => "Command completed.".into(),
        };
        let gameplay = matches!(
            action,
            Action::ConfirmedRestore { .. } | Action::RetryGameplayRestore
        );
        let verify = matches!(action, Action::Verify);
        let doctor = matches!(action, Action::Doctor);
        if let Ok(mut shared) = state.lock() {
            shared.settings_error.clear();
            shared
                .commands
                .update(id, Outcome::Working, "Processing request...");
        }
        let result = apply_action_detected(
            action, engine, folder, scanner, games, now, state, detection,
        );
        if let Ok(mut shared) = state.lock() {
            if doctor {
                shared.doctor_pending = false;
            }
            if settings {
                shared.commands.settings_pending = false;
            }
            if verify && result.is_err() {
                shared.verifying = false;
            }
            let failure = result
                .as_ref()
                .err()
                .map(|e| format!("Command failed: {e:#}"))
                .or_else(|| {
                    (!shared.settings_error.is_empty()).then(|| shared.settings_error.clone())
                });
            if let Some(failure) = failure {
                shared.commands.update(id, Outcome::Failed, failure);
                shared.settings_error.clear();
            } else if refresh {
                // run() binds this request to the next dispatch generation.
            } else if gameplay {
                let text = shared
                    .restore_feedback
                    .as_ref()
                    .map(RestoreFeedback::text)
                    .unwrap_or_default();
                let failed = !engine.last_error.is_empty()
                    || shared
                        .restore_feedback
                        .as_ref()
                        .is_some_and(|feedback| feedback.preference_failure)
                    || !shared
                        .restore_feedback
                        .as_ref()
                        .is_some_and(|feedback| feedback.accepted);
                shared.commands.update(
                    id,
                    if failed {
                        Outcome::Failed
                    } else if engine.pending() {
                        Outcome::Working
                    } else {
                        Outcome::Completed
                    },
                    text,
                );
                if !failed && engine.pending() {
                    let exclusions = shared
                        .restore_feedback
                        .as_ref()
                        .map(|f| f.exclusions.clone())
                        .unwrap_or_default();
                    shared.commands.waiting = Some((id, Waiting::GameplayRestore(exclusions)));
                }
            } else if verify {
                if let Some(report) = &shared.verify_report {
                    let summary = crate::dashboard::render_verify_report(report);
                    let outcome = if report.ok {
                        Outcome::Completed
                    } else {
                        Outcome::Failed
                    };
                    shared.commands.update(id, outcome, summary);
                }
            } else if let Some(waiting) = waiting {
                shared.commands.waiting = Some((id, waiting));
            } else {
                let unchanged = settings && before == engine.config;
                shared.commands.update(
                    id,
                    if unchanged {
                        Outcome::NoChange
                    } else {
                        Outcome::Completed
                    },
                    if unchanged {
                        "Settings unchanged.".into()
                    } else {
                        success
                    },
                );
            }
        }
        return result;
    }
    let core = match action {
        Action::Pause => Some(CoreCommand::Pause),
        Action::Resume => Some(CoreCommand::Resume),
        Action::Restore => Some(CoreCommand::Restore),
        _ => None,
    };
    if let Some(command) = core {
        // Revalidate against engine evidence and a fresh, exclusion-free scan.
        // A queued UI click cannot create/release a hold by toggling stale state.
        let ready = state
            .lock()
            .map(|s| s.discovery_ready && s.detection_ok && s.discovery_errors.is_empty())
            .unwrap_or(false);
        let mut guard = recovery_scanner(&engine.config)?;
        let current = action_evidence(
            detection,
            &mut guard,
            &engine.config,
            &recovery_games(engine, games),
            ready,
        );
        let controls = ControlState {
            activity: engine.activity,
            active_mode: engine.config.mode == "active",
            provider_enabled: engine.config.any_provider_enabled(),
            detection_ready: ready && !engine.disabled && current.is_some(),
            gaming: current.as_ref().is_some_and(GameEvidence::gaming),
            pending: engine.pending(),
            manual_hold: engine.manual_pause,
            gameplay_restore: false,
            coexistence: engine.gameplay.active(),
        };
        let availability = controls.availability();
        if !availability.allows(command) {
            if let Ok(mut shared) = state.lock() {
                shared.settings_error = availability.reason.into();
            }
            return Ok(false);
        }
    }
    match action {
        Action::Tracked { .. } => unreachable!(),
        // Consumed by the watcher loop, which owns the answer's lifetime.
        Action::PauseForGame => (),
        Action::RemoveCustom { path, name } => {
            let updated = remove_custom(&engine.config, &path, &name)?;
            return apply_action_detected(
                Action::Settings(Box::new(updated)),
                engine,
                folder,
                scanner,
                games,
                now,
                state,
                detection,
            );
        }
        Action::AdvancedSettings(updated) => {
            if !engine.config.advanced_settings_visible {
                bail!("Advanced settings is hidden; stale settings command refused");
            }
            return apply_action_detected(
                Action::Settings(updated),
                engine,
                folder,
                scanner,
                games,
                now,
                state,
                detection,
            );
        }
        Action::AdvancedVisibility(visible) => {
            let mut updated = engine.config.clone();
            updated.advanced_settings_visible = visible;
            return apply_action_detected(
                Action::Settings(Box::new(updated)),
                engine,
                folder,
                scanner,
                games,
                now,
                state,
                detection,
            );
        }
        Action::Appearance(appearance) => {
            if !engine.config.advanced_settings_visible {
                bail!("Advanced settings is hidden; stale appearance preference refused");
            }
            let mut updated = engine.config.clone();
            updated.appearance = appearance;
            write_json(&folder.join("config.json"), &updated)?;
            engine.config.appearance = appearance;
            engine.backend.config.appearance = appearance;
            if let Ok(mut shared) = state.lock() {
                shared.config.appearance = appearance;
                shared.settings_error.clear();
                shared.revision += 1;
            }
        }
        Action::NotificationPreferences { visual, sound } => {
            if !engine.config.advanced_settings_visible {
                bail!("Advanced settings is hidden; stale notification preference refused");
            }
            let mut updated = engine.config.clone();
            updated.notifications_enabled = visual;
            updated.sound_enabled = sound;
            return apply_action_detected(
                Action::Settings(Box::new(updated)),
                engine,
                folder,
                scanner,
                games,
                now,
                state,
                detection,
            );
        }
        Action::Settings(updated) => {
            engine.gameplay.invalidate_offer();
            let result = (|| -> Result<()> {
                updated.validate()?;
                if engine.pending() {
                    engine.validate_recovery_edit(&updated)?;
                }
                let replacement = Scanner::new((*updated).clone())?;
                write_json(&folder.join("config.json"), updated.as_ref())?;
                if !engine.provider_pending(crate::provider::Kind::LMStudio)
                    && updated.providers != engine.config.providers
                {
                    engine.backend.claims = Default::default();
                }
                engine.backend.reset((*updated).clone());
                engine.config = *updated;
                engine.settings_changed();
                *scanner = replacement;
                Ok(())
            })();
            if let Ok(mut shared) = state.lock() {
                shared.settings_error = result
                    .as_ref()
                    .err()
                    .map(|e| format!("Could not save settings: {e:#}"))
                    .unwrap_or_default();
                shared.config = engine.config.clone();
                shared.revision += 1;
            }
            return Ok(result.is_ok());
        }
        Action::Disable => {
            let mut updated = engine.config.clone();
            updated.automation_enabled = !updated.automation_enabled;
            return apply_action_detected(
                Action::Settings(Box::new(updated)),
                engine,
                folder,
                scanner,
                games,
                now,
                state,
                detection,
            );
        }
        Action::Pause => {
            engine.remember_games(engine.gameplay.remembered_games())?;
            engine.gameplay.revoke();
            engine.request_manual_pause();
        }
        Action::Resume => {
            engine.manual_pause = false;
        }
        Action::Restore => {
            if engine.config.mode == "observe" {
                return Ok(false);
            }
            if !state.lock().map(|s| s.discovery_ready).unwrap_or(false) {
                return Ok(false);
            }
            engine.manual_pause = false;
            let mut guard = recovery_scanner(&engine.config)?;
            let guard_games = recovery_games(engine, games);
            let config = engine.config.clone();
            let result = engine.restore(&mut || {
                action_evidence(detection, &mut guard, &config, &guard_games, true)
                    .is_none_or(|evidence| evidence.gaming())
            });
            engine.restore_failed = true;
            engine.attempt(result, now);
        }
        Action::ConfirmedRestore { offer_id, ignored } => {
            let ready = state
                .lock()
                .map(|s| s.discovery_ready && s.detection_ok && s.discovery_errors.is_empty())
                .unwrap_or(false);
            let known = recovery_games(engine, games);
            let mut guard = recovery_scanner(&engine.config)?;
            let feedback = confirmed_restore(
                engine,
                offer_id,
                &ignored,
                now,
                ready,
                &mut |config| action_evidence(detection, &mut guard, config, &known, ready),
                &mut |config| {
                    write_json(&folder.join("config.json"), config)?;
                    if let Ok(mut shared) = state.lock() {
                        shared.config = config.clone();
                        shared.revision += 1;
                    }
                    Ok(())
                },
            );
            engine.backend.config = engine.config.clone();
            *scanner = Scanner::new(engine.config.clone())?;
            if let Ok(mut shared) = state.lock() {
                shared.restore_feedback = Some(feedback);
                shared.restore_offer = engine.gameplay.offer();
                shared.coexistence = engine.gameplay.active();
                shared.config = engine.config.clone();
                shared.revision += 1;
            }
        }
        Action::RetryGameplayRestore => {
            let ready = state
                .lock()
                .map(|s| s.discovery_ready && s.detection_ok && s.discovery_errors.is_empty())
                .unwrap_or(false);
            if !ready || !engine.pending() || !engine.gameplay.active() || engine.activity.busy() {
                if !ready {
                    engine.gameplay.revoke();
                }
                if let Ok(mut shared) = state.lock() {
                    let feedback = shared.restore_feedback.get_or_insert_with(Default::default);
                    feedback.accepted = false;
                    feedback.restoration =
                        "Gameplay retry is unavailable; no model operation performed.".into();
                }
                return Ok(false);
            }
            let known = recovery_games(engine, games);
            let mut guard = recovery_scanner(&engine.config)?;
            let config = engine.config.clone();
            let result = engine.restore_gameplay(&mut || {
                action_evidence(detection, &mut guard, &config, &known, ready)
            });
            let outcome = restore_outcome(engine, &result);
            engine.restore_failed = true;
            engine.attempt(result, now);
            if let Ok(mut shared) = state.lock() {
                let feedback = shared.restore_feedback.get_or_insert_with(Default::default);
                feedback.accepted = true;
                feedback.restoration = outcome;
            }
        }
        Action::Doctor => {
            if !engine.config.advanced_settings_visible {
                bail!("Advanced settings is hidden; stale diagnostics command refused");
            }
            let report = crate::diagnostics::doctor(&engine.config, folder);
            if let Ok(mut shared) = state.lock() {
                shared.doctor_report = Some(report);
                shared.revision += 1;
            }
        }
        Action::Verify => {
            if !engine.config.advanced_settings_visible {
                bail!("Advanced settings is hidden; stale test command refused");
            }
            let ready = state
                .lock()
                .map(|s| s.discovery_ready && s.discovery_errors.is_empty())
                .unwrap_or(false);
            let guard_games = recovery_games(engine, games);
            let mut guard = recovery_scanner(&engine.config)?;
            let config = engine.config.clone();
            let report = engine.verify_round_trip(&mut || {
                !ready
                    || action_evidence(detection, &mut guard, &config, &guard_games, ready)
                        .is_none_or(|evidence| evidence.gaming())
            });
            if let Ok(mut shared) = state.lock() {
                shared.verifying = false;
                shared.pending = engine.pending();
                shared.verify_report = Some(report.clone());
                shared.message = report.summary.clone();
                shared.revision += 1;
            }
            return Ok(true);
        }
        Action::Refresh => return Ok(true),
        Action::Quit | Action::PowerChanged => (),
    }
    Ok(false)
}
pub fn remove_custom(config: &Config, path: &str, name: &str) -> Result<Config> {
    let index = config
        .extra_games
        .iter()
        .position(|game| crate::discovery::same_path(&game.path, path) && game.name == name)
        .context("Selected custom game changed or was removed; select it again")?;
    let mut updated = config.clone();
    updated.extra_games.remove(index);
    updated.validate()?;
    Ok(updated)
}
