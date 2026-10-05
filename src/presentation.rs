//! Factual core summaries shared by the dashboard and tray. No provider probing.
use crate::{app::Shared, control::Activity};

pub struct Summary {
    pub games: String,
    pub provider: String,
    pub next: String,
    pub reason: String,
}
/// The four visual states. Every worker activity folds into one of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Look {
    #[default]
    Running,
    Paused,
    Loading,
    Attention,
}
/// The short, plain state shown by the dashboard hero and the tray menu header.
pub struct Status {
    pub title: &'static str,
    pub game: Option<String>,
    pub hint: &'static str,
    pub look: Look,
}
impl Status {
    /// Two tray header rows: the state, then the running game or the hint.
    pub fn tray_lines(&self) -> [String; 2] {
        let title = match self.look {
            Look::Running => "AI running",
            Look::Paused => "AI paused",
            Look::Loading => "Loading",
            Look::Attention => "AI needs attention",
        };
        let detail = match &self.game {
            Some(game) if self.look == Look::Paused => game.clone(),
            _ => self.hint.into(),
        };
        [title.into(), detail]
    }
}
/// "AI RUNNING" means GamePause is not holding AI paused. Idle residency is not polled.
pub fn status(s: &Shared) -> Status {
    use Activity::*;
    let activity = if s.verifying { Verifying } else { s.activity };
    let (title, hint, look) = match activity {
        Unknown => ("LOADING", "Checking for running games...", Look::Loading),
        Watching if s.config.automation_enabled => {
            ("AI RUNNING", "Watching for game launches", Look::Running)
        }
        Watching => ("AI RUNNING", "Automatic pausing is off", Look::Running),
        Observation => (
            "AI RUNNING",
            "Observation mode. Games are detected and AI is left alone.",
            Look::Running,
        ),
        Coexistence => (
            "AI RUNNING",
            "Resumed while a game is running. Pause AI to free memory.",
            Look::Running,
        ),
        Paused => (
            "AI PAUSED",
            "AI paused for gaming. Resume to load your model.",
            Look::Paused,
        ),
        ManualHold => (
            "AI PAUSED",
            "You paused AI. Resume when you are ready.",
            Look::Paused,
        ),
        Countdown => (
            "AI PAUSED",
            "Game closed. AI resumes shortly.",
            Look::Paused,
        ),
        Capturing | Unloading | Restoring => ("LOADING", "Processing request...", Look::Loading),
        WaitingForInference => (
            "LOADING",
            "Waiting for the current response to finish.",
            Look::Loading,
        ),
        Verifying => ("LOADING", "Testing pause and resume...", Look::Loading),
        Unavailable => (
            "AI NEEDS ATTENTION",
            "AI is not reachable. Open your AI app or check Advanced.",
            Look::Attention,
        ),
        DetectionUnavailable => (
            "AI NEEDS ATTENTION",
            "Game detection is not working. AI is left alone until it recovers.",
            Look::Attention,
        ),
        Recovery => (
            "AI NEEDS ATTENTION",
            "Your models are saved but not loaded. Resume to try again.",
            Look::Attention,
        ),
        PartialFailure => (
            "AI NEEDS ATTENTION",
            "Something did not finish. Open Activity for details.",
            Look::Attention,
        ),
    };
    let reliable =
        s.discovery_ready && s.detection_ok && !s.disabled && s.discovery_errors.is_empty();
    let game = s.active_games.first().filter(|_| reliable).map(|first| {
        if s.active_games.len() == 1 {
            format!("{} is running", first.game)
        } else {
            format!(
                "{} and {} more are running",
                first.game,
                s.active_games.len() - 1
            )
        }
    });
    Status {
        title,
        game,
        hint,
        look,
    }
}
impl Summary {
    pub fn ai_text(&self) -> String {
        format!(
            "{}\r\nNext: {}{}",
            self.provider,
            self.next,
            if self.reason.is_empty() {
                String::new()
            } else {
                format!("\r\n{}", self.reason)
            }
        )
    }
}
pub fn summarize(shared: &Shared) -> Summary {
    let detected = shared.discovery_ready
        && shared.detection_ok
        && !shared.disabled
        && shared.discovery_errors.is_empty()
        && !matches!(
            shared.activity,
            Activity::Unknown | Activity::DetectionUnavailable
        );
    let mut games: String = if detected {
        if shared.active_games.is_empty() {
            "Games: no recognized games running.".into()
        } else {
            "Games detected running:".into()
        }
    } else if shared.active_games.is_empty() {
        "Games: detection is not ready; running games are unknown.".into()
    } else {
        "Games: detection is not ready. Last seen, not confirmed current:".into()
    };
    for game in &shared.active_games {
        games.push_str(&format!(
            "\r\n{} ({}, PID {})",
            game.game, game.launcher, game.pid
        ));
    }
    let activity = if shared.verifying {
        Activity::Verifying
    } else {
        shared.activity
    };
    let headline = match activity {
        Activity::Unknown => "waiting for game detection; AI state unknown.",
        Activity::Watching => "watching games; idle loaded-model state is not probed.",
        Activity::Observation => "observation only; AI is unchanged.",
        Activity::Unavailable => "unavailable; AI control cannot proceed.",
        Activity::DetectionUnavailable => "game detection unavailable; AI control is held.",
        Activity::Capturing => "capturing settings before unloading; pause is not complete.",
        Activity::WaitingForInference => "waiting for inference to finish; pause is not complete.",
        Activity::Unloading => "unloading captured AI; pause is not complete.",
        Activity::Paused => "paused for gaming; captured-model unload verified.",
        Activity::ManualHold => "manually paused; captured-model unload verified.",
        Activity::Countdown => "waiting to restore captured AI.",
        Activity::Restoring => "restoring captured AI; completion is not yet verified.",
        Activity::Recovery => "recovery pending; a completed pause or restore is not established.",
        Activity::PartialFailure => {
            "operation failed or incomplete; saved recovery is retained if pending."
        }
        Activity::Verifying => "testing capture, unload and restoration.",
        Activity::Coexistence => {
            "AI restoration verified during gameplay by your choice. Automatic pausing is temporarily overridden."
        }
    };
    let availability = shared.controls().availability();
    let next = if !shared.active_mode {
        "Observation mode does not change AI or execute recovery."
    } else if !detected {
        "Wait for successful game detection. Keep pending recovery intact."
    } else if activity.busy() {
        "Wait for this operation to finish; incompatible actions are disabled."
    } else if shared.coexistence {
        if shared.pending {
            "Retry resume uses the current gameplay approval. Pause AI ends approval."
        } else {
            "Pause AI ends approval. A new nonignored game, approved game exit or restart ends it too."
        }
    } else if shared.manual_pause {
        if availability.resume {
            "Resume AI releases the manual hold and resumes captured AI immediately."
        } else {
            "Close the recognized games to release the manual hold, or confirm Resume during gameplay."
        }
    } else if availability.restore {
        if !shared.active_games.is_empty() {
            "Resume AI opens a Cancel-default gameplay warning. Otherwise wait for games to exit."
        } else {
            "Resume AI retries recovery immediately; normal guards still apply."
        }
    } else if activity == Activity::WaitingForInference {
        "Wait for current inference to finish; GamePause retries automatically."
    } else if activity == Activity::Unavailable {
        "Open the enabled provider or check its connection. GamePause retries automatically."
    } else if !shared.config.automation_enabled {
        "Automatic pausing is off. Enable it to pause AI when recognized games start."
    } else {
        "A recognized game starts automatic pausing. Pause AI creates a manual hold."
    };
    Summary {
        games,
        provider: provider_lines(shared, headline),
        next: if shared.config.any_provider_enabled() || shared.pending {
            next.into()
        } else {
            "Enable a provider in Advanced settings before controlling AI.".into()
        },
        reason: shared.message.clone(),
    }
}
fn provider_lines(shared: &Shared, fallback: &str) -> String {
    use crate::{coordinator::State, provider::Kind};
    let lines = shared
        .config
        .providers
        .iter()
        .filter(|provider| provider.enabled())
        .map(|provider| {
            let Some(report) = shared
                .provider_statuses
                .iter()
                .find(|report| report.id == provider.id() && report.kind == provider.kind())
            else {
                return match provider.kind() {
                    Kind::LMStudio => format!("{}: {fallback}", provider.kind().name()),
                    Kind::Ollama => {
                        "Ollama: experimental, not live-tested; AI state unknown until capture."
                            .into()
                    }
                };
            };
            let state = match report.state {
                State::Uncaptured if report.pending => {
                    "saved recovery awaits reconciliation; AI state unverified."
                }
                State::Uncaptured => "not captured; AI state unknown.",
                State::Pausing => "pausing captured AI; unload not yet verified.",
                State::Paused if shared.activity == Activity::ManualHold => {
                    "manually paused; captured-model unload verified."
                }
                State::Paused => "captured-model unload verified; recovery retained.",
                State::Restoring => "restoring captured AI; completion not yet verified.",
                State::Restored => "restoration verified; current residency is not polled.",
                State::Deferred if report.pending => "operation deferred; recovery pending.",
                State::Deferred => "waiting for inference; pause is not complete.",
                State::Failed if report.pending => "operation failed; recovery retained.",
                State::Failed => "operation failed; completed protection is not established.",
            };
            let mut text = format!("{}: {state}", report.kind.name());
            if report.kind == Kind::Ollama {
                text.push_str(" Experimental; not tested with a live Ollama installation.");
            }
            if !report.error.is_empty() {
                // One bounded native menu row per provider; full detail stays in worker status.
                let mut error = report.error.chars();
                let excerpt = error
                    .by_ref()
                    .take(300)
                    .map(|ch| if ch.is_whitespace() { ' ' } else { ch })
                    .collect::<String>();
                text.push_str(&format!(" {excerpt}"));
                if error.next().is_some() {
                    text.push_str("...");
                }
            }
            if let Some(seconds) = report.retry_seconds {
                text.push_str(&format!(" Retry backoff at last update: {seconds}s."));
            }
            text
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        "AI: no enabled providers; AI is unchanged.".into()
    } else {
        lines.join("\r\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gameplay::fixtures::game;
    #[test]
    fn independent_provider_outcomes_show_partial_results_and_hide_disabled_rows() {
        use crate::{
            coordinator::{Report, State},
            provider::{Guarantee, Kind},
        };
        let mut shared = Shared {
            activity: Activity::PartialFailure,
            ..Default::default()
        };
        for provider in &mut shared.config.providers {
            if let crate::config::Provider::Ollama { enabled, .. } = provider {
                *enabled = true;
            }
        }
        shared.provider_statuses = shared
            .config
            .providers
            .iter()
            .map(|provider| Report {
                id: provider.id().into(),
                kind: provider.kind(),
                guarantee: if provider.kind() == Kind::LMStudio {
                    Guarantee::CapturedConfiguration
                } else {
                    Guarantee::SupportedFields
                },
                state: if provider.kind() == Kind::LMStudio {
                    State::Restored
                } else {
                    State::Failed
                },
                pending: provider.kind() == Kind::Ollama,
                error: if provider.kind() == Kind::Ollama {
                    "fixture\ndigest mismatch".into()
                } else {
                    String::new()
                },
                retry_seconds: (provider.kind() == Kind::Ollama).then_some(10),
            })
            .collect();
        let summary = summarize(&shared);
        assert!(
            summary
                .provider
                .contains("LM Studio: restoration verified; current residency is not polled.")
        );
        assert!(
            summary
                .provider
                .contains("Ollama: operation failed; recovery retained.")
        );
        assert!(summary.provider.contains("fixture digest mismatch"));
        assert!(
            summary
                .provider
                .contains("Retry backoff at last update: 10s")
        );
        for provider in &mut shared.config.providers {
            if let crate::config::Provider::Ollama { enabled, .. } = provider {
                *enabled = false;
            }
        }
        assert!(!summarize(&shared).provider.contains("Ollama"));
    }
    #[test]
    fn no_enabled_provider_has_no_control_or_loaded_state_claim() {
        let mut shared = Shared::default();
        shared.config.providers.clear();
        shared.activity = Activity::Watching;
        shared.active_mode = true;
        shared.discovery_ready = true;
        shared.detection_ok = true;
        let availability = shared.controls().availability();
        assert!(!availability.pause && !availability.resume && !availability.restore);
        assert!(availability.reason.contains("No AI provider"));
        let summary = summarize(&shared);
        assert!(summary.provider.contains("no enabled providers"));
        assert!(!summary.provider.contains("LM Studio:"));
    }
    #[test]
    fn all_games_are_visible_and_unknown_evidence_is_labelled() {
        let mut shared = Shared {
            active_games: vec![game(42, 1), game(7, 2)],
            ..Default::default()
        };
        let unknown = summarize(&shared);
        assert!(unknown.games.contains("not confirmed current"));
        assert!(unknown.games.contains("Fixture game 42"));
        assert!(unknown.games.contains("Fixture game 7"));
        shared.discovery_ready = true;
        shared.detection_ok = true;
        shared.activity = Activity::Watching;
        assert!(
            summarize(&shared)
                .games
                .starts_with("Games detected running")
        );
    }
    #[test]
    fn completion_claims_follow_activity_not_a_pending_journal() {
        let mut shared = Shared {
            active_mode: true,
            discovery_ready: true,
            detection_ok: true,
            pending: true,
            ..Default::default()
        };
        for activity in [
            Activity::Recovery,
            Activity::Capturing,
            Activity::Unloading,
            Activity::PartialFailure,
        ] {
            shared.activity = activity;
            assert!(!summarize(&shared).provider.contains("unload verified"));
            assert!(!summarize(&shared).provider.contains("Server stopped"));
        }
        shared.activity = Activity::Paused;
        assert!(summarize(&shared).provider.contains("unload verified"));
        shared.activity = Activity::Coexistence;
        shared.coexistence = true;
        shared.pending = false;
        let summary = summarize(&shared);
        assert!(summary.provider.contains("temporarily overridden"));
        assert!(summary.next.contains("Pause AI ends approval"));
    }
    #[test]
    fn every_activity_has_named_provider_and_next_step_without_disabled_ollama() {
        let mut shared = Shared {
            active_mode: true,
            discovery_ready: true,
            detection_ok: true,
            ..Default::default()
        };
        for activity in [
            Activity::Unknown,
            Activity::Watching,
            Activity::Observation,
            Activity::Unavailable,
            Activity::DetectionUnavailable,
            Activity::Capturing,
            Activity::WaitingForInference,
            Activity::Unloading,
            Activity::Paused,
            Activity::ManualHold,
            Activity::Countdown,
            Activity::Restoring,
            Activity::Recovery,
            Activity::PartialFailure,
            Activity::Verifying,
            Activity::Coexistence,
        ] {
            shared.activity = activity;
            let summary = summarize(&shared);
            assert!(summary.provider.starts_with("LM Studio:"));
            assert!(!summary.next.is_empty());
            assert!(!summary.ai_text().contains("Ollama"));
        }
    }
    #[test]
    fn tray_header_is_two_short_rows_for_every_activity() {
        let mut shared = Shared {
            active_mode: true,
            discovery_ready: true,
            detection_ok: true,
            active_games: vec![game(42, 1)],
            ..Default::default()
        };
        for activity in [
            Activity::Unknown,
            Activity::Watching,
            Activity::Observation,
            Activity::Unavailable,
            Activity::DetectionUnavailable,
            Activity::Capturing,
            Activity::WaitingForInference,
            Activity::Unloading,
            Activity::Paused,
            Activity::ManualHold,
            Activity::Countdown,
            Activity::Restoring,
            Activity::Recovery,
            Activity::PartialFailure,
            Activity::Verifying,
            Activity::Coexistence,
        ] {
            shared.activity = activity;
            let status = status(&shared);
            let [title, detail] = status.tray_lines();
            assert!(title.eq_ignore_ascii_case(status.title), "{activity:?}");
            assert!(!detail.is_empty() && !detail.contains('\n'));
            // The running game replaces the hint only while AI is paused.
            assert_eq!(
                detail == "Fixture game 42 is running",
                status.look == Look::Paused,
                "{activity:?}"
            );
        }
        shared.activity = Activity::Paused;
        shared.detection_ok = false;
        assert_eq!(status(&shared).tray_lines()[1], status(&shared).hint);
    }
}
