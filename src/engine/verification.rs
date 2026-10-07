//! The round-trip test: pause and restore the loaded models once, under the
//! same durable recovery as a real pause, and report each step.

use super::Engine;
use crate::{
    control::Activity, lmstudio::Backend, provider::Backend as ProviderBackend, recovery::Intent,
};
use anyhow::{Result, bail};
/// Per-step outcome of the P2-1 round-trip verify, so the dashboard and the
/// `gamepause verify` CLI can render exactly what happened at each stage
/// (capture / unload / verify-unloaded / restore / field-compare).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VerifyStep {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}
/// The result of one round-trip verify run.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VerifyReport {
    pub steps: Vec<VerifyStep>,
    pub ok: bool,
    pub summary: String,
}
impl<B: Backend> Engine<B> {
    /// Round-trip the live models through the same durable journal as gaming.
    /// The caller supplies a fail-closed game/quit guard checked between stages.
    pub fn verify_round_trip(&mut self, cancelled: &mut dyn FnMut() -> bool) -> VerifyReport {
        let mut steps = Vec::new();
        let mut phase = "guard";
        let result = (|| -> Result<()> {
            self.refresh_installed();
            let test_lm = self.config.lm_enabled() && !self.lm_missing && !self.lm_idle;
            let test_ollama = self.config.ollama_enabled();
            if self.config.mode != "active"
                || !(test_lm || test_ollama)
                || self.disabled
                || self.manual_pause
                || self.pending()
            {
                bail!(
                    "Verification unavailable in observe/disabled/paused mode or while recovery is pending"
                );
            }
            if cancelled() {
                bail!(
                    "Verification unavailable while a game is running or detection is unavailable"
                );
            }
            self.set_activity(Activity::Verifying);
            if !test_lm {
                return self.verify_ollama(&mut steps, cancelled);
            }
            phase = "capture";
            let snapshot = self.backend.capture()?;
            snapshot.validate_recovery()?;
            self.intent = Intent::Pause;
            // The adapter payload stays schema 2 inside the guarded schema-3 envelope.
            self.state = Some(snapshot);
            self.save()?;
            steps.push(VerifyStep {
                name: "capture".into(),
                ok: true,
                detail: "Snapshot persisted before unloading".into(),
            });
            let unloaded = self.pause_scoped(cancelled, true);
            steps.push(VerifyStep {
                name: "unload".into(),
                ok: unloaded.is_ok(),
                detail: unloaded
                    .as_ref()
                    .err()
                    .map(|e| format!("{e:#}"))
                    .unwrap_or_else(|| "Captured models unloaded".into()),
            });
            // Even a partial unload must be recovered; retain the original failure.
            let check = if unloaded.is_ok() {
                self.backend.resident_keys().and_then(|models| {
                    if !models.is_empty() {
                        bail!("Models still loaded after unload");
                    }
                    Ok(())
                })
            } else {
                Ok(())
            };
            if unloaded.is_ok() {
                steps.push(VerifyStep {
                    name: "verify-unloaded".into(),
                    ok: check.is_ok(),
                    detail: check
                        .as_ref()
                        .err()
                        .map(|e| format!("{e:#}"))
                        .unwrap_or_else(|| "Inventory empty".into()),
                });
            }
            let recovered = self.restore_checked(cancelled, true).and_then(|()| {
                if self.pending() {
                    bail!("Game detected; restoration deferred, recovery pending");
                }
                Ok(())
            });
            steps.push(VerifyStep {
                name: "restore".into(),
                ok: recovered.is_ok(),
                detail: recovered
                    .as_ref()
                    .err()
                    .map(|e| format!("{e:#}"))
                    .unwrap_or_else(|| "Models and original server state restored".into()),
            });
            steps.push(VerifyStep {
                name: "verify-fields".into(),
                ok: recovered.is_ok(),
                detail: recovered
                    .as_ref()
                    .err()
                    .map(|e| format!("{e:#}"))
                    .unwrap_or_else(|| {
                        "Load configuration verified before clearing recovery".into()
                    }),
            });
            unloaded?;
            check?;
            recovered?;
            if test_ollama {
                self.verify_ollama(&mut steps, cancelled)?;
            }
            Ok(())
        })();
        self.verify_scope = None;
        if let Err(error) = result {
            if steps.is_empty() {
                steps.push(VerifyStep {
                    name: phase.into(),
                    ok: false,
                    detail: format!("{error:#}"),
                });
            }
            self.last_error = format!("{error:#}");
            self.set_activity(Activity::PartialFailure);
        }
        let ok = !steps.is_empty() && steps.iter().all(|s| s.ok);
        if ok {
            self.last_error.clear();
        }
        let summary = if ok {
            "Round-trip verify passed".into()
        } else {
            format!(
                "Round-trip verify failed{}: {}",
                if self.pending() {
                    " — recovery pending"
                } else {
                    ""
                },
                self.last_error
            )
        };
        let report = VerifyReport { steps, ok, summary };
        self.verify_report = Some(report.clone());
        report
    }
    /// Ollama's half of the round-trip test: capture, unload with verified
    /// absence, then reload and verify identity, context and residency.
    fn verify_ollama(
        &mut self,
        steps: &mut Vec<VerifyStep>,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<()> {
        self.verify_scope = Some(crate::provider::Kind::Ollama);
        let unloaded = self.pause_scoped(cancelled, false);
        let captured = self.recovery.as_ref().and_then(|journal| {
            journal
                .providers
                .iter()
                .find_map(|entry| match &entry.payload {
                    crate::recovery::Payload::Ollama(snapshot) => Some(snapshot.clone()),
                    _ => None,
                })
        });
        steps.push(VerifyStep {
            name: "ollama-unload".into(),
            ok: unloaded.is_ok(),
            detail: match (&unloaded, &captured) {
                (Err(error), _) => format!("{error:#}"),
                (Ok(()), Some(snapshot)) if snapshot.absent => {
                    "Ollama is not running; nothing to test".into()
                }
                (Ok(()), Some(snapshot)) if snapshot.units() == 0 => {
                    "No Ollama models are loaded; nothing to test".into()
                }
                (Ok(()), Some(snapshot)) => format!(
                    "{} model(s) unloaded and verified absent. {}",
                    snapshot.units(),
                    snapshot.note()
                )
                .trim_end()
                .into(),
                (Ok(()), None) => "No Ollama capture was recorded".into(),
            },
        });
        // Even a partial unload must be recovered; retain the original failure.
        let recovered = self.restore_checked(cancelled, false).and_then(|()| {
            if self.pending() {
                bail!("Game detected; restoration deferred, recovery pending");
            }
            Ok(())
        });
        steps.push(VerifyStep {
            name: "ollama-restore".into(),
            ok: recovered.is_ok(),
            detail: recovered
                .as_ref()
                .err()
                .map(|e| format!("{e:#}"))
                .unwrap_or_else(|| {
                    "Restorable models reloaded; identity, context and residency verified".into()
                }),
        });
        unloaded?;
        recovered
    }
}
