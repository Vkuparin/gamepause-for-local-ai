#[cfg(test)]
use crate::config::write_json;
use crate::{
    config::Config,
    control::Activity,
    gameplay::{GameEvidence, GameplayControl},
    lmstudio::{Backend, Snapshot},
    provider::Backend as ProviderBackend,
    recovery::{self, Binding, Intent, Journal},
};
use anyhow::{Result, bail};
#[cfg(test)]
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};
type ProviderObserver = Box<dyn FnMut(&[crate::coordinator::Report]) + Send>;

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

pub struct Engine<B: Backend> {
    pub backend: B,
    pub config: Config,
    pub state: Option<Snapshot>,
    recovery: Option<Journal>,
    pub path: PathBuf,
    binding: Option<Binding>,
    intent: Intent,
    pub message: String,
    pub last_error: String,
    pub disabled: bool,
    pub manual_pause: bool,
    pub remembered_games: Vec<crate::discovery::Game>,
    /// Set by the two restore call sites just before `attempt`; consumed by
    /// `attempt` to emit the explicit "Restore failed" message (P0-4). Kept
    /// Separate from `last_error` so a *pause* failure is never mislabelled as
    /// a restore failure.
    pub restore_failed: bool,
    /// Latest report from the journalled, disruptive round-trip operation.
    pub verify_report: Option<crate::engine::VerifyReport>,
    pub pause_completions: u64,
    pub restore_completions: u64,
    pub activity: Activity,
    pub gameplay: GameplayControl,
    pub provider_statuses: Vec<crate::coordinator::Report>,
    /// LM Studio is enabled but not installed and owes no recovery, so it
    /// takes no part in control until its CLI appears.
    pub lm_missing: bool,
    /// Round-trip verification drives one provider at a time.
    verify_scope: Option<crate::provider::Kind>,
    provider_progress: Option<ProviderObserver>,
    coordinator_memory: Option<crate::coordinator::Continuation<crate::recovery::Payload>>,
    adapter_progress: Option<crate::lm_session::Progress>,
    ollama_runtime: crate::provider_runtime::OllamaRuntime,
    provider_now: Duration,
    progress: Option<Box<dyn FnMut(Activity) + Send>>,
    quiet_since: Option<f64>,
    retry_at: f64,
    resume_pending: bool,
    resume_grace: bool,
    power_guard: Option<Box<dyn Fn() -> bool + Send>>,
}

impl<B: Backend> Engine<B> {
    pub fn new(config: Config, backend: B, path: PathBuf) -> Result<Self> {
        let recovery = recovery::load(&path, &config)?;
        let lm = recovery.as_ref().and_then(|journal| {
            journal
                .providers
                .iter()
                .find(|entry| entry.binding.kind == crate::provider::Kind::LMStudio)
        });
        let binding = lm.map(|entry| entry.binding.clone());
        let state = lm.map(|entry| entry.payload.lm().cloned()).transpose()?;
        let intent = recovery
            .as_ref()
            .map_or(Intent::Reconcile, |journal| journal.session.intent);
        let remembered_games = recovery
            .as_ref()
            .map(|journal| journal.session.games.clone())
            .unwrap_or_default();
        let activity = if recovery.is_some() {
            Activity::Recovery
        } else {
            Activity::Unknown
        };
        Ok(Self {
            remembered_games,
            backend,
            config,
            state,
            recovery,
            path,
            binding,
            intent,
            message: "Observing games".into(),
            last_error: String::new(),
            disabled: false,
            manual_pause: false,
            restore_failed: false,
            verify_report: None,
            pause_completions: 0,
            restore_completions: 0,
            activity,
            gameplay: GameplayControl::default(),
            provider_statuses: vec![],
            lm_missing: false,
            verify_scope: None,
            provider_progress: None,
            coordinator_memory: None,
            adapter_progress: None,
            ollama_runtime: Default::default(),
            provider_now: Duration::ZERO,
            progress: None,
            quiet_since: None,
            retry_at: 0.,
            resume_pending: false,
            resume_grace: false,
            power_guard: None,
        })
    }
    fn save(&mut self) -> Result<()> {
        // A separate remembered-game/verification save changes the projected view.
        // Reconcile again before control; the original capture remains in state.
        self.coordinator_memory = None;
        self.adapter_progress = None;
        let journal = self.projected_journal()?;
        if let Some(journal) = &journal {
            journal.validate()?;
        }
        self.recovery = journal;
        recovery::save(&self.path, self.recovery.as_ref())
    }
    pub fn pending(&self) -> bool {
        self.recovery.is_some() || self.state.is_some()
    }
    pub fn pause_verified(&self) -> bool {
        if self.resume_pending
            || self
                .coordinator_memory
                .as_ref()
                .is_some_and(|memory| memory.persistence_pending())
            || self
                .state
                .as_ref()
                .is_some_and(|snapshot| !snapshot.pause_complete)
        {
            return false;
        }
        self.providers_paused()
            && self.recovery.as_ref().map_or_else(
                || {
                    self.state
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.pause_complete)
                },
                |journal| {
                    journal.providers.iter().all(|entry| match &entry.payload {
                        crate::recovery::Payload::LMStudio(snapshot) => snapshot.pause_complete,
                        crate::recovery::Payload::Ollama(snapshot) => snapshot.pause_complete,
                    })
                },
            )
    }
    /// Enabled provider names for status text; LM Studio when none is enabled.
    fn provider_names(&self) -> String {
        let names = self
            .control_config()
            .providers
            .iter()
            .filter(|provider| provider.enabled())
            .map(|provider| provider.kind().name())
            .collect::<Vec<_>>();
        if names.is_empty() {
            crate::provider::Kind::LMStudio.name().into()
        } else {
            names.join(" and ")
        }
    }
    pub fn validate_recovery_edit(&self, updated: &Config) -> Result<()> {
        let Some(journal) = &self.recovery else {
            return self.config.validate_recovery_edit(updated);
        };
        for entry in journal
            .providers
            .iter()
            .filter(|entry| !entry.restore_complete)
        {
            let original = self
                .config
                .providers
                .iter()
                .find(|provider| provider.id() == entry.binding.id);
            let replacement = updated
                .providers
                .iter()
                .find(|provider| provider.id() == entry.binding.id);
            if original != replacement {
                entry.binding.validate_route(updated).map_err(|error| {
                    anyhow::anyhow!("{} recovery is pending; {error}", entry.binding.kind.name())
                })?;
            }
        }
        Ok(())
    }
    fn projected_journal(&self) -> Result<Option<Journal>> {
        let mut journal = self.recovery.clone();
        if let Some(snapshot) = &self.state {
            let binding = self
                .binding
                .clone()
                .map(Ok)
                .unwrap_or_else(|| Binding::capture(&self.config, snapshot))?;
            if let Some(journal) = &mut journal {
                let entry = journal
                    .providers
                    .iter_mut()
                    .find(|entry| entry.binding.kind == crate::provider::Kind::LMStudio)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "LM projection has no recovery provider; all recovery retained"
                        )
                    })?;
                if entry.binding != binding {
                    bail!("LM recovery projection binding changed; all recovery retained");
                }
                crate::lm_session::validate_original(entry.payload.lm()?, snapshot)?;
                entry.payload = crate::recovery::Payload::LMStudio(snapshot.clone());
                journal.session.games = snapshot.games.clone();
            } else {
                journal = Some(Journal::lm(binding, snapshot.clone(), self.intent));
            }
        } else if let Some(journal) = &mut journal {
            journal.session.games = self.remembered_games.clone();
        }
        if let Some(journal) = &mut journal {
            journal.session.intent = self.intent;
        }
        Ok(journal)
    }
    pub fn observe_progress(&mut self, observer: impl FnMut(Activity) + Send + 'static) {
        self.progress = Some(Box::new(observer));
    }
    pub fn observe_providers(
        &mut self,
        observer: impl FnMut(&[crate::coordinator::Report]) + Send + 'static,
    ) {
        self.provider_progress = Some(Box::new(observer));
    }
    /// Settings as control sees them: a provider that is not installed and
    /// owes no recovery is left out, exactly as if it were turned off.
    fn control_config(&self) -> Config {
        let mut config = self.config.clone();
        if self.lm_missing {
            for provider in &mut config.providers {
                if let crate::config::Provider::LMStudio { enabled, .. } = provider {
                    *enabled = false;
                }
            }
        }
        config
    }
    fn refresh_installed(&mut self) {
        let owed = self.state.is_some()
            || self.recovery.as_ref().is_some_and(|journal| {
                journal
                    .providers
                    .iter()
                    .any(|entry| entry.binding.kind == crate::provider::Kind::LMStudio)
            });
        self.lm_missing = self.config.lm_enabled() && !owed && !self.backend.installed();
    }
    /// The session's journal holds no model and no stopped service: every
    /// provider was absent or had nothing loaded.
    fn nothing_held(&self) -> bool {
        self.recovery.as_ref().is_some_and(|journal| {
            journal.providers.iter().all(|entry| match &entry.payload {
                crate::recovery::Payload::LMStudio(snapshot) => {
                    snapshot.models.is_empty() && !snapshot.server_stopped
                }
                crate::recovery::Payload::Ollama(snapshot) => snapshot.units() == 0,
            })
        })
    }
    fn paused_message(&self) -> &'static str {
        match (self.manual_pause, self.nothing_held()) {
            (true, true) => "No AI models were loaded; Resume AI releases the hold",
            (true, false) => "AI paused by you; choose Resume AI to release the hold",
            (false, true) => "Game running; no AI models were loaded, so nothing was paused",
            (false, false) => "AI paused for gaming",
        }
    }
    fn providers_paused(&self) -> bool {
        let config = self.control_config();
        let enabled = config
            .providers
            .iter()
            .filter(|provider| provider.enabled())
            .collect::<Vec<_>>();
        // Retain compatibility with pre-coordinator in-memory LM evidence.
        if self.provider_statuses.is_empty() {
            return enabled.len() == 1 && enabled[0].kind() == crate::provider::Kind::LMStudio;
        }
        !enabled.is_empty()
            && enabled.iter().all(|provider| {
                self.provider_statuses.iter().any(|report| {
                    report.id == provider.id()
                        && report.kind == provider.kind()
                        && report.state == crate::coordinator::State::Paused
                })
            })
    }
    fn set_activity(&mut self, activity: Activity) {
        self.activity = activity;
        if activity.busy() {
            self.message = activity.progress_message().into();
        }
        if let Some(observer) = &mut self.progress {
            observer(activity);
        }
    }
    pub fn provider_pending(&self, kind: crate::provider::Kind) -> bool {
        self.recovery.as_ref().map_or(
            kind == crate::provider::Kind::LMStudio && self.state.is_some(),
            |journal| {
                journal
                    .providers
                    .iter()
                    .any(|entry| entry.binding.kind == kind && !entry.restore_complete)
            },
        )
    }
    pub fn settings_changed(&mut self) {
        if !self.provider_pending(crate::provider::Kind::Ollama) {
            self.ollama_runtime = Default::default();
        }
        self.retry_providers();
        self.retry_at = 0.;
        self.disabled = false;
    }
    /// Power events carry no game/provider evidence and authorize no mutation.
    pub fn resume_detected(&mut self) {
        self.resume_pending = true;
        self.resume_grace = true;
        self.retry_at = 0.;
        self.quiet_since = None;
        self.gameplay.invalidate_offer();
        self.provider_statuses.clear();
        self.set_activity(Activity::DetectionUnavailable);
        self.message =
            "Windows resumed; waiting for fresh game detection before reconciling saved AI.".into();
    }
    pub fn observe_power(&mut self, interrupted: impl Fn() -> bool + Send + 'static) {
        self.power_guard = Some(Box::new(interrupted));
    }
    pub fn awaiting_resume_detection(&self) -> bool {
        self.resume_pending
    }
    fn power_interrupted(&self) -> bool {
        self.power_guard.as_ref().is_some_and(|guard| guard())
    }
    fn reconcile_resumed(&mut self) -> Result<()> {
        let mut journal = self.projected_journal()?;
        if let Some(journal) = &mut journal {
            for entry in journal
                .providers
                .iter_mut()
                .filter(|entry| !entry.restore_complete)
            {
                match &mut entry.payload {
                    crate::recovery::Payload::LMStudio(snapshot) => snapshot.pause_complete = false,
                    crate::recovery::Payload::Ollama(snapshot) => {
                        if journal.session.intent == Intent::Pause {
                            snapshot.begin();
                        } else {
                            snapshot.pause_complete = false;
                        }
                    }
                }
            }
            journal.validate()?;
            // Invalidate completion evidence durably before any further control.
            // A failed save leaves the original authority and resume gate intact.
            recovery::save(&self.path, Some(journal))?;
        }
        self.recovery = journal;
        let lm = self.recovery.as_ref().and_then(|journal| {
            journal
                .providers
                .iter()
                .find(|entry| entry.binding.kind == crate::provider::Kind::LMStudio)
        });
        self.binding = lm.map(|entry| entry.binding.clone());
        self.state = lm.map(|entry| entry.payload.lm().cloned()).transpose()?;
        self.coordinator_memory = None;
        self.adapter_progress = None;
        self.retry_at = 0.;
        self.resume_pending = false;
        Ok(())
    }
    pub fn request_manual_pause(&mut self) {
        self.retry_providers();
        self.manual_pause = true;
        self.retry_at = 0.;
    }
    pub fn remember_games(&mut self, games: Vec<crate::discovery::Game>) -> Result<()> {
        if !self.pending() {
            self.remembered_games = games;
            return Ok(());
        }
        let mut changed = false;
        for game in games {
            if !self.remembered_games.iter().any(|g| {
                crate::discovery::canonical(&g.path) == crate::discovery::canonical(&game.path)
            }) {
                self.remembered_games.push(game);
                changed = true;
            }
        }
        if changed {
            if let Some(snapshot) = &mut self.state {
                snapshot.games = self.remembered_games.clone();
            }
            self.save()?;
        }
        Ok(())
    }
    pub fn step(&mut self, gaming: bool, now: f64, cancelled: &mut dyn FnMut() -> bool) {
        if self.resume_pending {
            self.set_activity(Activity::DetectionUnavailable);
            self.message =
                "Windows resumed; fresh game detection is required before AI control.".into();
            return;
        }
        if now.is_finite() && now >= 0. {
            self.provider_now = Duration::from_secs_f64(now);
        }
        self.refresh_installed();
        if !self.control_config().any_provider_enabled() && !self.pending() {
            self.set_activity(Activity::Watching);
            self.message = if self.lm_missing {
                "Watching games; no AI app was found to pause".into()
            } else {
                "No AI provider is enabled; choose one in Advanced settings".into()
            };
            return;
        }
        if self.disabled {
            self.set_activity(Activity::DetectionUnavailable);
            self.message = "Detection disabled; recovery retained".into();
            return;
        }
        if self.config.mode == "observe" {
            self.set_activity(Activity::Observation);
            self.message = if gaming {
                "Would pause AI: game detected"
            } else {
                "Observing games; AI unchanged"
            }
            .into();
            return;
        }
        if !self.config.automation_enabled && !self.manual_pause {
            if !self.pending() {
                self.set_activity(Activity::Watching);
                self.message = "Automatic pausing is off".into();
                return;
            }
            if gaming {
                self.set_activity(Activity::Recovery);
                self.quiet_since = None;
                self.message =
                    "Automatic pausing is off; saved AI will return after the game exits".into();
                return;
            }
        }
        if gaming || self.manual_pause {
            self.resume_grace = false;
            self.quiet_since = None;
            if self.provider_work_ready(Intent::Pause, now) || self.pause_verified() {
                let result = self.pause_guarded(&mut || false);
                self.attempt(result, now);
            }
        } else if self.pending() {
            let since = *self.quiet_since.get_or_insert(now);
            let remaining = self.config.restore_delay_seconds - (now - since);
            if remaining > 0. {
                if self.resume_grace {
                    self.set_activity(Activity::Recovery);
                    self.message = format!(
                        "Windows resumed; saved AI recovery in {}s after fresh game detection.",
                        remaining.ceil() as u64
                    );
                } else if self.pause_verified() {
                    self.set_activity(Activity::Countdown);
                    self.message = format!("Restoring AI in {}s", remaining.ceil() as u64);
                } else {
                    self.set_activity(Activity::Recovery);
                    self.message = format!(
                        "AI recovery in {}s; pause was incomplete. {}",
                        remaining.ceil() as u64,
                        self.last_error
                    );
                }
            } else if self.provider_work_ready(Intent::Restore, now) {
                let result = self.restore_checked(cancelled, false);
                self.restore_failed = true;
                self.attempt(result, now);
            }
        } else {
            self.resume_grace = false;
            self.set_activity(Activity::Watching);
            self.last_error.clear();
            self.provider_statuses.clear();
            if let Some(observer) = &mut self.provider_progress {
                observer(&self.provider_statuses);
            }
            self.retry_at = 0.;
            self.message = format!(
                "Watching games; {} state has not been probed",
                self.provider_names()
            );
        }
    }
    /// The explicit gameplay path is the only caller that can use transient
    /// authority. Ordinary restore and verification retain their supplied guards.
    pub fn restore_gameplay(
        &mut self,
        scan: &mut dyn FnMut() -> Option<GameEvidence>,
    ) -> Result<()> {
        self.restore_gameplay_checked(scan, true)
    }
    fn restore_gameplay_checked(
        &mut self,
        scan: &mut dyn FnMut() -> Option<GameEvidence>,
        explicit: bool,
    ) -> Result<()> {
        if explicit {
            self.retry_providers();
        }
        let mut policy = std::mem::take(&mut self.gameplay);
        if self.config.mode != "active" || self.disabled {
            policy.revoke();
        }
        policy.reconcile(scan().as_ref());
        let result = if policy.active() && self.config.mode == "active" && !self.disabled {
            self.manual_pause = false;
            self.restore_checked(
                &mut || {
                    policy.reconcile(scan().as_ref());
                    !policy.active()
                },
                false,
            )
        } else {
            bail_gameplay_refused()
        };
        self.gameplay = policy;
        if self.gameplay.active() && !self.pending() && result.is_ok() {
            self.set_activity(Activity::Coexistence);
            self.message = format!(
                "{}: AI restored during gameplay by your choice; automatic pausing is temporarily overridden. Pause AI ends the override.",
                self.provider_names()
            );
        }
        result
    }
    pub fn step_games(
        &mut self,
        evidence: Option<GameEvidence>,
        now: f64,
        scan: &mut dyn FnMut() -> Option<GameEvidence>,
    ) {
        if now.is_finite() && now >= 0. {
            self.provider_now = Duration::from_secs_f64(now);
        }
        if self.config.mode != "active" || self.disabled {
            self.gameplay.revoke();
        }
        self.gameplay.reconcile(evidence.as_ref());
        let Some(evidence) = evidence.filter(|e| e.reliable()) else {
            self.quiet_since = None;
            self.set_activity(Activity::DetectionUnavailable);
            self.message =
                "Game detection is unknown; gameplay approval revoked and recovery held".into();
            return;
        };
        if self.resume_pending && self.config.mode == "observe" {
            self.set_activity(Activity::Observation);
            self.message =
                "Windows resumed; observing fresh games. AI and saved recovery are unchanged."
                    .into();
            return;
        }
        if self.resume_pending && now < self.retry_at {
            return;
        }
        if self.resume_pending
            && let Err(error) = self.reconcile_resumed()
        {
            self.set_activity(Activity::PartialFailure);
            self.last_error = format!("{error:#}");
            self.message = format!(
                "Resume reconciliation could not save recovery; AI control held. {}",
                self.last_error
            );
            self.retry_at = now + self.config.retry_seconds;
            return;
        }
        if self.gameplay.active() {
            self.resume_grace = false;
            if !self.pending() {
                self.set_activity(Activity::Coexistence);
                self.message = format!(
                    "{}: AI restored during gameplay by your choice; automatic pausing is temporarily overridden. Pause AI ends the override.",
                    self.provider_names()
                );
            } else if self.provider_work_ready(Intent::Restore, now) {
                let result = self.restore_gameplay_checked(scan, false);
                self.restore_failed = true;
                self.attempt(result, now);
            }
        } else {
            let gaming = !evidence.triggers.is_empty() || (self.pending() && evidence.gaming());
            self.step(gaming, now, &mut || {
                scan().is_none_or(|e| !e.reliable() || e.gaming())
            });
        }
    }
    pub fn attempt(&mut self, result: Result<()>, now: f64) {
        // Consume the flag so it reflects "the last operation was a restore
        // attempt" and never lingers into a later pause.
        let was_restore = self.restore_failed;
        self.restore_failed = false;
        match result {
            Ok(()) => {
                self.last_error.clear();
                self.retry_at = 0.;
            }
            Err(e) => {
                let busy = e.downcast_ref::<crate::provider::InferenceBusy>().is_some();
                self.set_activity(if busy {
                    Activity::WaitingForInference
                } else {
                    Activity::PartialFailure
                });
                self.last_error = format!("{e:#}");
                self.message = if busy {
                    format!(
                        "{}: waiting for active inference to finish; AI has not been paused",
                        self.provider_names()
                    )
                } else if was_restore {
                    format!("Restore failed — AI not restored: {}", self.last_error)
                } else {
                    format!("Needs attention: {}", self.last_error)
                };
                self.retry_at = now + self.config.retry_seconds;
            }
        }
    }
    pub fn pause(&mut self) -> Result<()> {
        self.retry_providers();
        self.pause_guarded(&mut || false)
    }
    fn pause_guarded(&mut self, cancelled: &mut dyn FnMut() -> bool) -> Result<()> {
        self.pause_scoped(cancelled, false)
    }
    fn pause_scoped(&mut self, cancelled: &mut dyn FnMut() -> bool, only_lm: bool) -> Result<()> {
        if self.resume_pending || self.power_interrupted() {
            bail!("Power state changed; pause held until fresh resume reconciliation");
        }
        if !self.control_config().any_provider_enabled() {
            bail!("No AI provider is enabled; no pause was started");
        }
        if self.pause_verified() && self.activity != Activity::Recovery {
            self.set_activity(if self.manual_pause {
                Activity::ManualHold
            } else {
                Activity::Paused
            });
            self.message = self.paused_message().into();
            return Ok(());
        }
        self.pause_units(cancelled, only_lm)?;
        // A pause that found nothing loaded is not a success to announce.
        if !self.nothing_held() {
            self.pause_completions += 1;
        }
        self.set_activity(if self.manual_pause {
            Activity::ManualHold
        } else {
            Activity::Paused
        });
        self.message = self.paused_message().into();
        Ok(())
    }
    fn pause_units(&mut self, cancelled: &mut dyn FnMut() -> bool, only_lm: bool) -> Result<()> {
        use crate::coordinator::{Coordinator, JournalFile, State, Step};
        let journal = self.projected_journal()?;
        let memory = self.take_continuation(&journal)?;
        let progress = if memory.is_some() {
            self.adapter_progress.take().unwrap_or_default()
        } else {
            Default::default()
        };
        self.set_activity(if !self.pending() {
            Activity::Capturing
        } else {
            Activity::Unloading
        });
        let power_guard = &self.power_guard;
        let control = self.control_config();
        let runtime = crate::provider_runtime::Providers::new(
            &mut self.backend,
            control,
            false,
            &mut self.ollama_runtime,
        )
        .with_progress(progress);
        let mut bindings = runtime.bindings()?;
        if let Some(kind) = self
            .verify_scope
            .or(only_lm.then_some(crate::provider::Kind::LMStudio))
        {
            bindings.retain(|binding| binding.kind == kind);
        }
        let overhead = bindings.len().saturating_mul(2).saturating_add(4);
        let mut coordinator = if let Some(memory) = memory {
            Coordinator::resume(
                runtime,
                JournalFile(self.path.clone()),
                bindings,
                memory,
                Duration::from_secs_f64(self.config.retry_seconds),
            )?
        } else {
            Coordinator::new(
                runtime,
                JournalFile(self.path.clone()),
                bindings,
                journal,
                Duration::from_secs_f64(self.config.retry_seconds),
            )?
        };
        let began = Instant::now();
        let mut units = 0usize;
        let result = (|| {
            loop {
                let now = self.provider_now.saturating_add(began.elapsed());
                let result =
                    coordinator.advance(Intent::Pause, &self.remembered_games, now, &mut || {
                        power_guard.as_ref().is_some_and(|guard| guard()) || cancelled()
                    });
                if let Some(journal) = coordinator.journal() {
                    self.recovery = Some(journal.clone());
                    let lm = journal
                        .providers
                        .iter()
                        .find(|entry| entry.binding.kind == crate::provider::Kind::LMStudio);
                    self.binding = lm.map(|entry| entry.binding.clone());
                    self.state = lm.map(|entry| entry.payload.lm().cloned()).transpose()?;
                    self.intent = journal.session.intent;
                    if coordinator.persistence_pending()
                        && let Some(snapshot) = &mut self.state
                    {
                        snapshot.pause_complete = false;
                    }
                } else {
                    // A verified retired session can clear before a new capture.
                    self.recovery = None;
                    self.state = None;
                    self.binding = None;
                }
                self.provider_statuses = coordinator.reports(now);
                if let Some(observer) = &mut self.provider_progress {
                    observer(&self.provider_statuses);
                }
                let step = result?;
                if step == Step::Interrupted {
                    if power_guard.as_ref().is_some_and(|guard| guard()) {
                        bail!("Power state changed; pause interrupted and recovery retained");
                    }
                    bail!("Game detected; recovery retained");
                }
                if step == Step::Idle {
                    let failures = coordinator
                        .reports(now)
                        .into_iter()
                        .filter(|status| status.state == State::Failed)
                        .map(|status| format!("{}: {}", status.kind.name(), status.error))
                        .collect::<Vec<_>>();
                    if !failures.is_empty() {
                        bail!("{}", failures.join("; "));
                    }
                    if coordinator
                        .statuses()
                        .values()
                        .any(|status| status.state == State::Deferred)
                    {
                        return Err(crate::provider::InferenceBusy.into());
                    }
                    bail!("Provider pause is incomplete; recovery retained");
                }
                if coordinator.pause_complete() {
                    return Ok(());
                }
                if step == Step::Captured {
                    self.activity = Activity::Unloading;
                    self.message = Activity::Unloading.progress_message().into();
                    if let Some(observer) = &mut self.progress {
                        observer(Activity::Unloading);
                    }
                }
                units += 1;
                if units
                    > self
                        .recovery
                        .as_ref()
                        .map_or(0, |journal| {
                            journal
                                .providers
                                .iter()
                                .map(|entry| match &entry.payload {
                                    crate::recovery::Payload::LMStudio(snapshot) => {
                                        snapshot.models.len()
                                    }
                                    crate::recovery::Payload::Ollama(snapshot) => snapshot.units(),
                                })
                                .sum::<usize>()
                        })
                        .saturating_add(overhead)
                {
                    bail!("Provider pause did not converge; recovery retained");
                }
            }
        })();
        let (runtime, _, memory) = coordinator.into_parts();
        self.adapter_progress = Some(runtime.router.lm.progress());
        drop(runtime);
        self.coordinator_memory = Some(memory);
        result
    }
    pub fn restore(&mut self, cancelled: &mut dyn FnMut() -> bool) -> Result<()> {
        self.retry_providers();
        self.restore_checked(cancelled, false)
    }
    fn restore_checked(
        &mut self,
        cancelled: &mut dyn FnMut() -> bool,
        compare: bool,
    ) -> Result<()> {
        if self.resume_pending || self.power_interrupted() {
            bail!("Power state changed; restore held until fresh resume reconciliation");
        }
        if !self.pending() {
            return Ok(());
        }
        if !self
            .config
            .providers
            .iter()
            .any(|provider| provider.enabled())
            && !self
                .recovery
                .as_ref()
                .is_some_and(|journal| journal.providers.iter().all(|entry| entry.restore_complete))
        {
            bail!("LM Studio recovery is pending; re-enable its provider. Recovery retained");
        }
        if cancelled() {
            self.set_activity(Activity::Recovery);
            self.message = "Game restarted; restoration deferred".into();
            return Ok(());
        }
        self.set_activity(Activity::Restoring);
        if !self.restore_units(cancelled, compare)? {
            if self.power_interrupted() {
                self.set_activity(Activity::DetectionUnavailable);
                self.message =
                    "Power state changed; restoration held and saved recovery retained.".into();
            } else {
                self.set_activity(Activity::Recovery);
                self.message = "Game restarted; restoration interrupted".into();
            }
            return Ok(());
        }
        let nothing_held = self.nothing_held();
        self.state = None;
        self.recovery = None;
        self.binding = None;
        self.coordinator_memory = None;
        self.adapter_progress = None;
        self.intent = Intent::Reconcile;
        self.remembered_games.clear();
        self.quiet_since = None;
        self.set_activity(Activity::Watching);
        if nothing_held {
            self.message = "Nothing needed restoring".into();
        } else {
            self.restore_completions += 1;
            self.message = "AI restored".into();
        }
        Ok(())
    }
    /// Drain serial restore units, including healthy models after a local failure.
    /// A fresh game guard still runs at every coordinator boundary.
    fn restore_units(
        &mut self,
        cancelled: &mut dyn FnMut() -> bool,
        compare: bool,
    ) -> Result<bool> {
        use crate::coordinator::{Coordinator, JournalFile, Step};
        let journal = self.projected_journal()?;
        let limit = journal
            .as_ref()
            .map_or(0, |journal| {
                journal
                    .providers
                    .iter()
                    .map(|entry| match &entry.payload {
                        crate::recovery::Payload::LMStudio(snapshot) => snapshot.models.len(),
                        crate::recovery::Payload::Ollama(snapshot) => snapshot.units(),
                    })
                    .sum::<usize>()
                    .saturating_mul(2)
                    .saturating_add(journal.providers.len().saturating_mul(4))
            })
            .saturating_add(6);
        let memory = self.take_continuation(&journal)?;
        let progress = if memory.is_some() {
            self.adapter_progress.take().unwrap_or_default()
        } else {
            Default::default()
        };
        let power_guard = &self.power_guard;
        let control = self.control_config();
        let runtime = crate::provider_runtime::Providers::new(
            &mut self.backend,
            control,
            compare,
            &mut self.ollama_runtime,
        )
        .with_progress(progress);
        let mut bindings = runtime.bindings()?;
        // Raw field comparison is an LM capability; verification scopes each
        // phase to the provider it is testing.
        if let Some(kind) = self
            .verify_scope
            .or(compare.then_some(crate::provider::Kind::LMStudio))
        {
            bindings.retain(|binding| binding.kind == kind);
        }
        let mut coordinator = if let Some(memory) = memory {
            Coordinator::resume(
                runtime,
                JournalFile(self.path.clone()),
                bindings,
                memory,
                Duration::from_secs_f64(self.config.retry_seconds),
            )?
        } else {
            Coordinator::new(
                runtime,
                JournalFile(self.path.clone()),
                bindings,
                journal,
                Duration::from_secs_f64(self.config.retry_seconds),
            )?
        };
        let began = Instant::now();
        let result = (|| {
            for _ in 0..limit {
                let now = self.provider_now.saturating_add(began.elapsed());
                let result =
                    coordinator.advance(Intent::Restore, &self.remembered_games, now, &mut || {
                        power_guard.as_ref().is_some_and(|guard| guard()) || cancelled()
                    });
                // Preserve durable/dirty progress on every result, including failed writes.
                if let Some(journal) = coordinator.journal() {
                    self.recovery = Some(journal.clone());
                    let lm = journal
                        .providers
                        .iter()
                        .find(|entry| entry.binding.kind == crate::provider::Kind::LMStudio);
                    self.binding = lm.map(|entry| entry.binding.clone());
                    self.state = lm.map(|entry| entry.payload.lm().cloned()).transpose()?;
                    self.intent = journal.session.intent;
                    if coordinator.persistence_pending()
                        && let Some(snapshot) = &mut self.state
                    {
                        snapshot.pause_complete = false;
                    }
                }
                self.provider_statuses = coordinator.reports(now);
                if let Some(observer) = &mut self.provider_progress {
                    observer(&self.provider_statuses);
                }
                match result? {
                    Step::Completed => return Ok(true),
                    Step::Interrupted => return Ok(false),
                    Step::Idle => {
                        let failures = coordinator
                            .reports(now)
                            .into_iter()
                            .filter(|status| !status.error.is_empty())
                            .map(|status| format!("{}: {}", status.kind.name(), status.error))
                            .collect::<Vec<_>>();
                        bail!("{}; recovery retained", failures.join("; "));
                    }
                    _ => {}
                }
            }
            bail!("Provider restoration did not converge; recovery retained")
        })();
        let (runtime, _, memory) = coordinator.into_parts();
        self.adapter_progress = Some(runtime.router.lm.progress());
        drop(runtime);
        self.coordinator_memory = Some(memory);
        result
    }
    fn provider_work_ready(&self, intent: Intent, now: f64) -> bool {
        self.coordinator_memory
            .as_ref()
            .map_or(now >= self.retry_at, |memory| {
                if memory.persistence_pending() {
                    now >= self.retry_at
                } else {
                    memory.ready(intent, self.provider_now)
                }
            })
    }
    fn retry_providers(&mut self) {
        if let Some(memory) = &mut self.coordinator_memory {
            memory.request_retry();
        }
    }
    fn take_continuation(
        &mut self,
        journal: &Option<Journal>,
    ) -> Result<Option<crate::coordinator::Continuation<crate::recovery::Payload>>> {
        if let Some(memory) = &self.coordinator_memory {
            let mut expected = memory.journal().cloned();
            if let Some(expected) = &mut expected {
                for entry in &mut expected.providers {
                    if memory.persistence_pending()
                        && let crate::recovery::Payload::LMStudio(snapshot) = &mut entry.payload
                    {
                        snapshot.pause_complete = false;
                    }
                }
            }
            if &expected != journal {
                bail!(
                    "Recovery view changed outside the coordinator; control held and recovery retained"
                );
            }
        }
        Ok(self.coordinator_memory.take())
    }
    /// Round-trip the live models through the same durable journal as gaming.
    /// The caller supplies a fail-closed game/quit guard checked between stages.
    pub fn verify_round_trip(&mut self, cancelled: &mut dyn FnMut() -> bool) -> VerifyReport {
        let mut steps = Vec::new();
        let mut phase = "guard";
        let result = (|| -> Result<()> {
            self.refresh_installed();
            let test_lm = self.config.lm_enabled() && !self.lm_missing;
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
}

impl<B: Backend> Engine<B> {
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

fn bail_gameplay_refused() -> Result<()> {
    bail!("Gameplay Restore approval is no longer valid; recovery retained")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lmstudio::Model;
    use serde_json::{Value, json};
    use std::{
        collections::BTreeSet,
        sync::atomic::{AtomicU64, Ordering},
    };
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    #[derive(Clone)]
    struct Fake {
        current: BTreeSet<String>,
        running: bool,
        port: u16,
        fail_start: bool,
        events: Vec<String>,
        fail_unload: Option<String>,
        delayed_unload: bool,
        fail_restore: Option<String>,
        fail_stop: bool,
        fail_read: Option<String>,
        fail_loaded_on: Option<usize>,
        lock_loaded_on: Option<usize>,
        loaded_calls: usize,
        lock_after_unload: bool,
        lock_after_restore: bool,
        lock_after_stop: bool,
        held_lock: Option<std::sync::Arc<std::fs::File>>,
        journal: PathBuf,
        /// When set, `read_config` returns a value that differs from the
        /// captured `load_config` (temperature 0.999 vs 0.7), so the P2-1
        /// round-trip verify reports the exact failing field.
        mutate_read: bool,
        power_on: Option<(String, std::sync::Arc<crate::power::Signal>)>,
    }
    impl Fake {
        fn new() -> Self {
            Self {
                current: ["chat".into(), "embed".into()].into_iter().collect(),
                running: true,
                port: 1234,
                fail_start: false,
                events: vec![],
                fail_unload: None,
                delayed_unload: false,
                fail_restore: None,
                fail_stop: false,
                fail_read: None,
                fail_loaded_on: None,
                lock_loaded_on: None,
                loaded_calls: 0,
                lock_after_unload: false,
                lock_after_restore: false,
                lock_after_stop: false,
                held_lock: None,
                journal: PathBuf::new(),
                mutate_read: false,
                power_on: None,
            }
        }
    }
    impl Fake {
        fn lock_journal(&mut self) {
            use std::os::windows::fs::OpenOptionsExt;
            self.held_lock = Some(std::sync::Arc::new(
                std::fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(&self.journal)
                    .unwrap(),
            ));
        }
    }
    impl Backend for Fake {
        fn verify_restored(&mut self, model: &Model) -> Result<()> {
            if !self.running || !self.current.contains(&model.identifier) {
                bail!("Restored model is missing");
            }
            Ok(())
        }
        fn server_state(&mut self) -> Result<Value> {
            Ok(json!({"running":self.running,"port":self.port}))
        }
        fn snapshot(&mut self) -> Result<Snapshot> {
            if self
                .power_on
                .as_ref()
                .is_some_and(|(event, _)| event == "snapshot")
            {
                self.power_on.take().unwrap().1.notify(4);
            }
            self.events.push("snapshot".into());
            Ok(Snapshot {
                games: vec![],
                schema: 2,
                server: json!({"running":self.running,"port":self.port}),
                server_stopped: false,
                pause_complete: false,
                models: self
                    .current
                    .iter()
                    .map(|id| Model {
                        identifier: id.clone(),
                        base_key: id.clone(),
                        model_key: id.clone(),
                        namespace: "llm".into(),
                        ttl_ms: None,
                        load_config: json!({"fields":[{"key":"temperature","value":0.7}]}),
                        native_config: json!({}),
                        stage: "planned".into(),
                    })
                    .collect(),
            })
        }
        fn read_config(&mut self, model: &Model) -> Result<Value> {
            if !self.running || self.fail_read.as_deref() == Some(&model.identifier) {
                bail!("Configuration read failed");
            }
            // Round-trip read-back: normally identical to the captured
            // load_config; with `mutate_read` the temperature drifts so the
            // verify field-compare reports exactly that field.
            let value = if self.mutate_read { 0.999 } else { 0.7 };
            Ok(json!({"fields":[{"key":"temperature","value":value}]}))
        }
        fn loaded(&mut self) -> Result<Vec<Value>> {
            self.loaded_calls += 1;
            if self.fail_loaded_on == Some(self.loaded_calls) {
                bail!("Inventory read failed");
            }
            if self.lock_loaded_on == Some(self.loaded_calls) {
                self.lock_journal();
            }
            Ok(self
                .current
                .iter()
                .map(|id| json!({"identifier":id}))
                .collect())
        }
        fn stop_server(&mut self) -> Result<()> {
            self.events.push("stop".into());
            if self.fail_stop {
                bail!("stop failed");
            }
            self.running = false;
            if self.lock_after_stop {
                self.lock_journal();
            }
            Ok(())
        }
        fn start_server(&mut self, port: u16) -> Result<()> {
            self.events.push("start".into());
            if self.fail_start {
                bail!("Server start failed");
            }
            self.port = port;
            self.running = true;
            Ok(())
        }
        fn ensure_server(&mut self, port: u16) -> Result<()> {
            if self.running && self.port != port {
                bail!("Server port changed");
            }
            if !self.running {
                self.start_server(port)?;
            }
            Ok(())
        }
        fn unload(&mut self, id: &str) -> Result<()> {
            if self
                .power_on
                .as_ref()
                .is_some_and(|(event, _)| event == &format!("unload:{id}"))
            {
                self.power_on.take().unwrap().1.notify(4);
            }
            // The "unloading" journal-entry assertion documents the pause-flow
            // invariant. It only applies when a recovery journal actually
            // exists; a self-contained verify (P2-1) unloads without one.
            if self.journal.as_os_str().is_empty() || self.journal.exists() {
                let disk: Option<Journal> =
                    serde_json::from_str(&fs::read_to_string(&self.journal)?).unwrap();
                let journal = disk.unwrap();
                let intent = journal.session.intent;
                let disk = journal
                    .providers
                    .iter()
                    .find_map(|entry| entry.payload.lm().ok())
                    .unwrap();
                assert_eq!(intent, Intent::Pause);
                assert!(
                    disk.models
                        .iter()
                        .any(|m| m.identifier == id && m.stage == "unloading")
                );
            }
            self.events.push(format!("unload:{id}"));
            if self.fail_unload.as_deref() == Some(id) {
                bail!("unload failed");
            }
            if !self.delayed_unload {
                self.current.remove(id);
            }
            if self.lock_after_unload {
                self.lock_journal();
            }
            Ok(())
        }
        fn restore(&mut self, m: &Model) -> Result<()> {
            if self
                .power_on
                .as_ref()
                .is_some_and(|(event, _)| event == &format!("restore:{}", m.identifier))
            {
                self.power_on.take().unwrap().1.notify(4);
            }
            let disk: Journal = serde_json::from_str(&fs::read_to_string(&self.journal)?).unwrap();
            let intent = disk.session.intent;
            let snapshot = disk
                .providers
                .iter()
                .find_map(|entry| entry.payload.lm().ok())
                .unwrap();
            assert_eq!(intent, Intent::Restore);
            assert!(
                snapshot
                    .models
                    .iter()
                    .any(|model| model.identifier == m.identifier && model.stage == "restoring")
            );
            if !self.running {
                bail!("REST/WS server is stopped");
            }
            self.events.push(format!("restore:{}", m.identifier));
            if self.fail_restore.as_deref() == Some(&m.identifier) {
                bail!("restore failed");
            }
            self.current.insert(m.identifier.clone());
            if self.lock_after_restore {
                self.lock_journal();
            }
            Ok(())
        }
    }
    fn engine(mut backend: Fake) -> Engine<Fake> {
        let folder = std::env::temp_dir().join(format!(
            "gamepause-engine-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        // Windows can reuse a PID from an earlier test run whose journal remains.
        // Require a fresh directory rather than accepting an existing recovery file.
        fs::create_dir(&folder).unwrap();
        backend.journal = folder.join("state.json");
        Engine::new(
            Config {
                mode: "active".into(),
                ..Default::default()
            },
            backend,
            folder.join("state.json"),
        )
        .unwrap()
    }
    fn has_restored_model(path: &std::path::Path) -> bool {
        let journal: Journal = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let snapshot = journal.providers[0].payload.lm().unwrap();
        journal.session.intent == Intent::Restore
            && snapshot
                .models
                .iter()
                .any(|model| model.stage == "restored")
    }
    fn prepare_gameplay(e: &mut Engine<Fake>, evidence: &GameEvidence) -> u64 {
        e.remember_games(
            evidence
                .all
                .iter()
                .map(|game| {
                    crate::discovery::Game::new(&game.launcher, "fixture", &game.game, &game.path)
                })
                .collect(),
        )
        .unwrap();
        e.pause().unwrap();
        e.gameplay.refresh_offer(Some(evidence), true);
        e.gameplay.offer().unwrap().id
    }
    #[test]
    fn resume_restarts_grace_after_reliable_detection_and_preserves_original_capture() {
        use crate::gameplay::fixtures::evidence;
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        let original = e.state.clone().unwrap();
        e.step_games(Some(evidence(vec![])), 0., &mut || Some(evidence(vec![])));
        e.resume_detected();
        assert!(!e.pause_verified());
        let events = e.backend.events.len();
        e.step(false, 1000., &mut || false);
        assert_eq!(e.backend.events.len(), events);
        assert!(e.restore(&mut || false).is_err());
        e.step_games(None, 1001., &mut || None);
        assert!(e.resume_pending);
        e.step_games(Some(evidence(vec![])), 1100., &mut || {
            Some(evidence(vec![]))
        });
        assert!(!e.resume_pending);
        assert!(
            e.message
                .contains("Windows resumed; saved AI recovery in 30s")
        );
        assert_eq!(e.backend.events.len(), events);
        let saved = e.state.as_ref().unwrap();
        assert_eq!(saved.server, original.server);
        for (saved, original) in saved.models.iter().zip(&original.models) {
            assert_eq!(saved.load_config, original.load_config);
            assert_eq!(saved.identifier, original.identifier);
        }
        e.step_games(Some(evidence(vec![])), 1129.9, &mut || {
            Some(evidence(vec![]))
        });
        assert!(e.pending());
        e.step_games(Some(evidence(vec![])), 1130., &mut || {
            Some(evidence(vec![]))
        });
        assert!(!e.pending());
        assert_eq!(e.backend.current.len(), 2);
    }
    #[test]
    fn resume_while_gaming_rechecks_provider_residency_without_recapturing() {
        use crate::gameplay::fixtures::{evidence, game};
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        let original = e.state.clone().unwrap();
        // A provider/client restarted and made the captured models resident again.
        e.backend.current = ["chat".into(), "embed".into()].into_iter().collect();
        e.backend.running = true;
        let captures = e
            .backend
            .events
            .iter()
            .filter(|event| *event == "snapshot")
            .count();
        e.resume_detected();
        let current = evidence(vec![game(42, 10)]);
        e.step_games(Some(current.clone()), 1000., &mut || Some(current.clone()));
        assert!(e.pause_verified());
        assert!(e.backend.current.is_empty());
        assert_eq!(
            e.backend
                .events
                .iter()
                .filter(|event| *event == "snapshot")
                .count(),
            captures
        );
        assert_eq!(
            e.state.as_ref().unwrap().models[0].load_config,
            original.models[0].load_config
        );
    }
    #[test]
    fn resume_revalidates_coexistence_and_unknown_detection_revokes_it() {
        use crate::gameplay::fixtures::{evidence, game};
        let mut e = engine(Fake::new());
        let current = evidence(vec![game(42, 10)]);
        let id = prepare_gameplay(&mut e, &current);
        e.gameplay.confirm(id, &current).unwrap();
        e.restore_gameplay(&mut || Some(current.clone())).unwrap();
        let events = e.backend.events.len();
        e.resume_detected();
        assert!(
            e.gameplay.active(),
            "power event is not an unconditional restart revocation"
        );
        assert!(e.gameplay.offer().is_none());
        e.step_games(Some(current.clone()), 1000., &mut || Some(current.clone()));
        assert_eq!(e.activity, Activity::Coexistence);
        assert_eq!(e.backend.events.len(), events);
        e.resume_detected();
        e.step_games(None, 1100., &mut || None);
        assert!(!e.gameplay.active());
        assert!(e.resume_pending);
        assert_eq!(e.backend.events.len(), events);
    }
    #[test]
    fn resume_reconciliation_write_failure_blocks_control_and_retains_authority() {
        use crate::gameplay::fixtures::{evidence, game};
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        let before = fs::read(&e.path).unwrap();
        let events = e.backend.events.len();
        e.backend.lock_journal();
        e.resume_detected();
        let current = evidence(vec![game(42, 10)]);
        e.step_games(Some(current.clone()), 1000., &mut || Some(current.clone()));
        assert!(e.resume_pending);
        assert_eq!(e.activity, Activity::PartialFailure);
        assert_eq!(e.backend.events.len(), events);
        e.backend.held_lock = None;
        assert_eq!(fs::read(&e.path).unwrap(), before);
        e.step_games(Some(current.clone()), 1001., &mut || Some(current.clone()));
        assert!(
            e.resume_pending,
            "failed resume persistence honors retry backoff"
        );
        e.step_games(Some(current.clone()), 1030., &mut || Some(current.clone()));
        assert!(!e.resume_pending);
        assert!(e.pause_verified());
    }
    #[test]
    fn power_event_during_capture_unload_or_load_stops_before_the_next_unit() {
        for operation in ["snapshot", "unload:chat", "restore:chat"] {
            let signal = std::sync::Arc::new(crate::power::Signal::default());
            let mut e = engine(Fake::new());
            if operation.starts_with("restore") {
                e.pause().unwrap();
            }
            e.backend.power_on = Some((operation.into(), signal.clone()));
            let guard = signal.clone();
            e.observe_power(move || !guard.permits(0));
            if operation.starts_with("restore") {
                e.restore(&mut || false).unwrap();
                assert!(!e.backend.current.contains("embed"));
                assert!(e.message.contains("Power state changed"));
            } else {
                assert!(e.pause().is_err());
                assert!(e.backend.current.contains("embed"));
            }
            assert!(e.pending());
            let journal = recovery::load(&e.path, &e.config).unwrap().unwrap();
            let saved = journal.providers[0].payload.lm().unwrap();
            assert_eq!(saved.models.len(), 2);
            assert!(
                saved
                    .models
                    .iter()
                    .all(|model| model.load_config["fields"][0]["value"] == 0.7)
            );
        }
    }
    #[test]
    fn resume_after_partial_load_and_provider_restart_replays_originals() {
        use crate::gameplay::fixtures::evidence;
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        e.backend.fail_restore = Some("embed".into());
        assert!(e.restore(&mut || false).is_err());
        assert!(e.backend.current.contains("chat"));
        e.backend.current.clear();
        e.backend.running = false;
        e.backend.fail_restore = None;
        e.resume_detected();
        e.step_games(Some(evidence(vec![])), 1000., &mut || {
            Some(evidence(vec![]))
        });
        assert!(e.pending());
        e.step_games(Some(evidence(vec![])), 1030., &mut || {
            Some(evidence(vec![]))
        });
        assert!(!e.pending());
        assert_eq!(e.backend.current.len(), 2);
        assert!(e.backend.running);
    }
    #[test]
    fn resume_observation_never_rewrites_pending_recovery() {
        use crate::gameplay::fixtures::evidence;
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        let before = fs::read(&e.path).unwrap();
        let events = e.backend.events.len();
        e.config.mode = "observe".into();
        e.resume_detected();
        e.step_games(Some(evidence(vec![])), 1000., &mut || {
            Some(evidence(vec![]))
        });
        assert_eq!(e.activity, Activity::Observation);
        assert_eq!(e.backend.events.len(), events);
        assert_eq!(fs::read(&e.path).unwrap(), before);
        assert!(
            e.resume_pending,
            "switching to active must still reconcile old evidence"
        );
    }
    #[test]
    fn resume_preserves_manual_hold_until_explicit_release_and_normal_grace() {
        use crate::gameplay::fixtures::evidence;
        let mut e = engine(Fake::new());
        e.request_manual_pause();
        e.pause().unwrap();
        e.resume_detected();
        for now in [1000., 2000.] {
            e.step_games(Some(evidence(vec![])), now, &mut || Some(evidence(vec![])));
            assert!(e.manual_pause);
            assert!(e.pending());
            assert!(e.backend.current.is_empty());
            assert_eq!(e.activity, Activity::ManualHold);
        }
        e.manual_pause = false;
        e.step_games(Some(evidence(vec![])), 2001., &mut || {
            Some(evidence(vec![]))
        });
        assert!(e.pending());
        e.step_games(Some(evidence(vec![])), 2031., &mut || {
            Some(evidence(vec![]))
        });
        assert!(!e.pending());
        assert_eq!(e.backend.current.len(), 2);
    }
    #[test]
    fn compatibility_projection_cannot_replace_originals_during_a_game_save() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        let bytes = fs::read(&e.path).unwrap();
        e.state.as_mut().unwrap().models[0].load_config = serde_json::json!({"changed":true});
        assert!(
            e.remember_games(vec![crate::discovery::Game::new(
                "Custom",
                "extra",
                "Extra game",
                r"D:\Fixture Games\extra.exe"
            )])
            .is_err()
        );
        assert_eq!(fs::read(&e.path).unwrap(), bytes);
    }
    #[test]
    fn mixed_recovery_finishes_healthy_lm_and_retains_unavailable_ollama_across_restart() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        let mut journal = e.recovery.clone().unwrap();
        journal.providers.push(recovery::ollama_fixture());
        recovery::save(&e.path, Some(&journal)).unwrap();
        let original = journal.providers[1].payload.clone();
        let mut resumed = Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
        assert!(resumed.pending());
        assert!(!resumed.pause_verified());
        assert!(resumed.restore(&mut || false).is_err());
        assert_eq!(
            resumed.backend.current.len(),
            2,
            "disabled Ollama cannot block healthy LM restoration"
        );
        let saved = recovery::load(&e.path, &e.config).unwrap().unwrap();
        assert!(saved.providers.iter().any(|entry| entry.binding.kind
            == crate::provider::Kind::LMStudio
            && entry.restore_complete));
        assert_eq!(saved.providers[1].payload, original);
        assert_eq!(resumed.restore_completions, 0);
        let calls = resumed.backend.loaded_calls;
        let events = resumed.backend.events.len();
        resumed.resume_detected();
        use crate::gameplay::fixtures::evidence;
        resumed.step_games(Some(evidence(vec![])), 1000., &mut || {
            Some(evidence(vec![]))
        });
        let after_resume = recovery::load(&e.path, &e.config).unwrap().unwrap();
        assert!(after_resume.providers[0].restore_complete);
        assert_eq!(after_resume.providers[1].payload, original);
        assert_eq!(resumed.backend.events.len(), events);
        assert_eq!(resumed.backend.loaded_calls, calls);
        assert!(resumed.restore(&mut || false).is_err());
        assert_eq!(resumed.backend.loaded_calls, calls);
        resumed
            .remember_games(vec![crate::discovery::Game::new(
                "Custom",
                "extra",
                "Extra game",
                r"D:\Fixture Games\extra.exe",
            )])
            .unwrap();
        assert_eq!(
            recovery::load(&e.path, &e.config)
                .unwrap()
                .unwrap()
                .providers[1]
                .payload,
            original
        );
        let restarted =
            Engine::new(e.config.clone(), resumed.backend.clone(), e.path.clone()).unwrap();
        assert!(restarted.pending());
        assert_eq!(restarted.recovery.unwrap().providers.len(), 2);
    }
    #[test]
    fn ollama_only_obligation_is_pending_without_an_lm_projection() {
        let e = engine(Fake::new());
        let journal = Journal {
            schema: 3,
            session: recovery::Session {
                games: vec![],
                intent: Intent::Restore,
            },
            providers: vec![recovery::ollama_fixture()],
        };
        recovery::save(&e.path, Some(&journal)).unwrap();
        let mut resumed = Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
        assert!(resumed.state.is_none());
        assert!(resumed.pending());
        let posts = resumed.backend.events.clone();
        assert!(!resumed.verify_round_trip(&mut || false).ok);
        assert!(resumed.restore(&mut || false).is_err());
        assert_eq!(resumed.backend.events, posts);
        assert_eq!(
            recovery::load(&e.path, &e.config)
                .unwrap()
                .unwrap()
                .providers[0]
                .payload,
            journal.providers[0].payload
        );
    }
    #[test]
    fn completed_pause_recovers_visible_state_after_detection_returns_without_io() {
        let mut e = engine(Fake::new());
        e.step(true, 0., &mut || false);
        let calls = e.backend.loaded_calls;
        let bytes = fs::read(&e.path).unwrap();
        e.disabled = true;
        e.step(true, 1., &mut || false);
        assert_eq!(e.activity, Activity::DetectionUnavailable);
        e.disabled = false;
        e.step(true, 2., &mut || false);
        assert_eq!(e.activity, Activity::Paused);
        assert_eq!(e.backend.loaded_calls, calls);
        assert_eq!(fs::read(&e.path).unwrap(), bytes);
        assert_eq!(e.pause_completions, 1);
    }
    #[test]
    fn verified_recovery_can_clear_after_all_providers_are_disabled_without_control() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        let mut journal = e.recovery.clone().unwrap();
        e.restore(&mut || false).unwrap();
        journal.session.intent = Intent::Restore;
        journal.providers[0].restore_complete = true;
        let snapshot = journal.providers[0].payload.lm_mut().unwrap();
        snapshot.pause_complete = false;
        for model in &mut snapshot.models {
            model.stage = "restored".into();
        }
        recovery::save(&e.path, Some(&journal)).unwrap();
        for provider in &mut e.config.providers {
            if let crate::config::Provider::LMStudio { enabled, .. } = provider {
                *enabled = false;
            }
        }
        let events = e.backend.events.clone();
        let mut resumed = Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
        assert!(resumed.pending());
        resumed.restore(&mut || false).unwrap();
        assert!(!resumed.pending());
        assert!(
            recovery::load(&resumed.path, &resumed.config)
                .unwrap()
                .is_none()
        );
        assert_eq!(resumed.backend.events, events);
        recovery::save(&resumed.path, Some(&journal)).unwrap();
        for provider in &mut resumed.config.providers {
            if let crate::config::Provider::LMStudio {
                enabled,
                connection,
                ..
            } = provider
            {
                *enabled = true;
                connection.endpoint = "127.0.0.1:5432".into();
            }
        }
        resumed.backend.port = 0; // A new capture cannot validate this fixture server.
        let mut recapture = Engine::new(
            resumed.config.clone(),
            resumed.backend.clone(),
            resumed.path.clone(),
        )
        .unwrap();
        let error = recapture.pause().unwrap_err();
        assert!(
            !recapture.pending(),
            "a failed new capture cannot resurrect verified retired recovery: {error:#}"
        );
        assert!(
            recovery::load(&recapture.path, &recapture.config)
                .unwrap()
                .is_none()
        );
        let mut expected = events;
        expected.push("snapshot".into());
        assert_eq!(recapture.backend.events, expected);
    }
    #[test]
    fn ollama_ownership_refuses_capture_before_http_without_blocking_healthy_lm() {
        let mut e = engine(Fake::new());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let mut owner = crate::ownership::Claims::default();
        owner
            .claim("fixture-owner", crate::provider::Kind::Ollama, &[&endpoint])
            .unwrap();
        for provider in &mut e.config.providers {
            if let crate::config::Provider::Ollama {
                enabled,
                endpoint: route,
                ..
            } = provider
            {
                *enabled = true;
                *route = endpoint.clone();
            }
        }
        assert!(e.pause().is_err());
        assert!(e.pending());
        assert!(e.backend.current.is_empty());
        assert!(e.state.as_ref().unwrap().pause_complete);
        assert!(!e.pause_verified());
        assert_eq!(e.pause_completions, 0);
        assert_eq!(e.recovery.as_ref().unwrap().providers.len(), 1);
        assert!(
            e.provider_statuses
                .iter()
                .any(|report| report.kind == crate::provider::Kind::Ollama
                    && report.state == crate::coordinator::State::Failed)
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    #[test]
    fn routed_private_http_pause_restore_preserves_mixed_and_ollama_only_recovery() {
        use crate::{ollama_session::Stage, provider::Kind};
        use serde_json::json;
        use windows_sys::Win32::{
            Foundation::{FILETIME, SYSTEMTIME},
            System::Time::FileTimeToSystemTime,
        };
        for lm_enabled in [true, false] {
            let mut e = engine(Fake::new());
            let path = e.path.clone();
            let ticks = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 300)
                * 10_000_000
                + 116_444_736_000_000_000;
            let filetime = FILETIME {
                dwLowDateTime: ticks as u32,
                dwHighDateTime: (ticks >> 32) as u32,
            };
            let mut time: SYSTEMTIME = unsafe { std::mem::zeroed() };
            assert_ne!(unsafe { FileTimeToSystemTime(&filetime, &mut time) }, 0);
            let expiry = format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
            );
            let model = json!({"name":"fixture-http:latest", "model":"fixture-http:latest",
                "digest":"c".repeat(64), "context_length":4096, "expires_at":expiry});
            let mut present = true;
            let mut controls = 0;
            let (endpoint, thread) = crate::ollama_session::tests::http_server(
                if lm_enabled { 16 } else { 12 },
                move |route, body| {
                    let value = match route {
                        "/api/ps" => {
                            json!({"models":if present { vec![model.clone()] } else { vec![] }})
                        }
                        "/api/tags" => json!({"models":[model.clone()]}),
                        "/api/show" => {
                            json!({"details":{"format":"gguf"}, "capabilities":["completion"],
                        "model_info":{"general.architecture":"fixture", "fixture.context_length":8192}})
                        }
                        "/api/generate" => {
                            let body = body.unwrap();
                            let unloading = body["keep_alive"] == 0;
                            assert_eq!(body["prompt"], "");
                            assert_eq!(body["stream"], false);
                            assert_eq!(body["model"], "fixture-http:latest:local");
                            let journal: Journal =
                                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                            assert_eq!(
                                journal.session.intent,
                                if unloading {
                                    Intent::Pause
                                } else {
                                    Intent::Restore
                                }
                            );
                            assert_eq!(
                                journal.providers.len(),
                                if lm_enabled && controls == 0 { 2 } else { 1 }
                            );
                            let entry = journal
                                .providers
                                .iter()
                                .find(|entry| entry.binding.kind == Kind::Ollama)
                                .unwrap();
                            let recovery::Payload::Ollama(snapshot) = &entry.payload else {
                                panic!("wrong payload")
                            };
                            assert_eq!(
                                snapshot.models[0].stage,
                                if unloading {
                                    Stage::Unloading
                                } else {
                                    Stage::Loading
                                }
                            );
                            if lm_enabled && controls == 0 {
                                let lm = journal
                                    .providers
                                    .iter()
                                    .find_map(|entry| entry.payload.lm().ok())
                                    .unwrap();
                                assert_eq!(lm.models.len(), 2);
                            }
                            controls += 1;
                            assert!(controls <= if lm_enabled { 3 } else { 2 });
                            present = !unloading;
                            json!({"model":body["model"], "done":true,
                            "done_reason":if unloading {"unload"} else {"load"}, "response":""})
                        }
                        _ => panic!("unexpected fixture route"),
                    };
                    (200, serde_json::to_vec(&value).unwrap())
                },
            );
            // Explicit fixture enrollment uses the same validated settings as users.
            for provider in &mut e.config.providers {
                match provider {
                    crate::config::Provider::LMStudio { enabled, .. } => *enabled = lm_enabled,
                    crate::config::Provider::Ollama {
                        enabled,
                        endpoint: route,
                        ..
                    } => {
                        *enabled = true;
                        *route = endpoint.clone();
                    }
                }
            }
            let initial_lm_events = e.backend.events.clone();
            e.config.validate().unwrap();
            e.pause().unwrap();
            assert!(e.pending());
            assert!(e.pause_verified());
            assert_eq!(e.state.is_some(), lm_enabled);
            assert_eq!(e.pause_completions, 1);
            let mut competitor = crate::ownership::Claims::default();
            assert!(
                competitor
                    .claim("competing", Kind::Ollama, &[&endpoint])
                    .is_err()
            );
            if lm_enabled {
                // Simulate an externally disabled entry in a pending session.
                // Healthy LM restoration proceeds, but Ollama keeps its claim.
                for provider in &mut e.config.providers {
                    if let crate::config::Provider::Ollama { enabled, .. } = provider {
                        *enabled = false;
                    }
                }
                assert!(e.restore(&mut || false).is_err());
                assert!(e.pending());
                assert_eq!(e.backend.current.len(), 2);
                assert!(
                    competitor
                        .claim("competing", Kind::Ollama, &[&endpoint])
                        .is_err()
                );
                let journal = recovery::load(&e.path, &e.config).unwrap().unwrap();
                assert!(journal.providers.iter().any(|entry| entry.binding.kind == Kind::LMStudio && entry.restore_complete));
                for provider in &mut e.config.providers {
                    if let crate::config::Provider::Ollama { enabled, .. } = provider {
                        *enabled = true;
                    }
                }
                let mut updated = e.config.clone();
                for provider in &mut updated.providers {
                    if let crate::config::Provider::LMStudio { enabled, .. } = provider {
                        *enabled = false;
                    }
                }
                e.validate_recovery_edit(&updated).unwrap();
                e.config = updated;
                e.settings_changed();
                assert!(!e.provider_pending(Kind::LMStudio));
                assert!(recovery::load(&e.path, &e.config).unwrap().is_some());
                let lm_events = e.backend.events.clone();
                e.pause().unwrap();
                assert!(e.pause_verified());
                assert!(e.state.is_none());
                assert_eq!(e.recovery.as_ref().unwrap().providers.len(), 1);
                assert_eq!(
                    e.backend.events, lm_events,
                    "a restored disabled provider is not repaused"
                );
            } else {
                let (config, backend, path) = (e.config.clone(), e.backend.clone(), e.path.clone());
                drop(e);
                e = Engine::new(config, backend, path).unwrap();
                assert!(e.pending());
                assert!(e.state.is_none());
            }
            let ollama_original = e
                .recovery
                .as_ref()
                .unwrap()
                .providers
                .iter()
                .find_map(|entry| match &entry.payload {
                    recovery::Payload::Ollama(snapshot) => {
                        Some(snapshot.models[0].original.clone())
                    }
                    _ => None,
                })
                .unwrap();
            e.resume_detected();
            use crate::gameplay::fixtures::evidence;
            e.step_games(Some(evidence(vec![])), 1000., &mut || {
                Some(evidence(vec![]))
            });
            assert!(e.pending(), "resume must start a fresh grace interval");
            let recovery::Payload::Ollama(snapshot) =
                &e.recovery.as_ref().unwrap().providers[0].payload
            else {
                panic!("unfinished Ollama payload must survive resume");
            };
            assert_eq!(snapshot.models[0].original, ollama_original);
            e.restore(&mut || false).unwrap();
            assert!(!e.pending());
            assert!(recovery::load(&e.path, &e.config).unwrap().is_none());
            assert_eq!(e.restore_completions, 1);
            assert_eq!(e.backend.current.len(), 2);
            if !lm_enabled {
                assert_eq!(e.backend.events, initial_lm_events);
            }
            thread.join().unwrap();
            drop(e);
            competitor
                .claim("competing", Kind::Ollama, &[&endpoint])
                .unwrap();
        }
    }
    #[test]
    fn a_pause_with_nothing_loaded_is_reported_as_such_and_not_counted() {
        let mut backend = Fake::new();
        backend.current.clear();
        backend.running = false;
        let mut e = engine(backend);
        e.step(true, 0., &mut || false);
        assert_eq!(e.activity, Activity::Paused);
        assert_eq!(
            e.message,
            "Game running; no AI models were loaded, so nothing was paused"
        );
        assert_eq!(e.pause_completions, 0);
        e.step(false, 1., &mut || false);
        e.step(false, 40., &mut || false);
        assert!(!e.pending());
        assert_eq!(e.message, "Nothing needed restoring");
        assert_eq!(e.restore_completions, 0);
        // The ordinary case still announces both.
        let mut e = engine(Fake::new());
        e.step(true, 0., &mut || false);
        assert_eq!(
            (e.message.as_str(), e.pause_completions),
            ("AI paused for gaming", 1)
        );
    }
    #[test]
    fn routed_provider_failure_preserves_healthy_pause_and_never_claims_whole_pause() {
        let mut e = engine(Fake::new());
        // A reachable private endpoint that fails; a closed port would mean
        // Ollama is simply not running. No real provider is contacted.
        let (unavailable_endpoint, thread) =
            crate::ollama_session::tests::http_server(2, |_, _| (500, vec![]));
        for provider in &mut e.config.providers {
            if let crate::config::Provider::Ollama {
                enabled, endpoint, ..
            } = provider
            {
                *enabled = true;
                *endpoint = unavailable_endpoint.clone();
            }
        }
        let reports = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = reports.clone();
        e.observe_providers(move |reports| observed.lock().unwrap().push(reports.to_vec()));
        let mut healthy_calls = None;
        for now in [0., 31.] {
            e.step(true, now, &mut || false);
            assert_eq!(e.activity, Activity::PartialFailure);
            assert_eq!(e.pause_completions, 0);
            assert!(
                e.state.as_ref().unwrap().pause_complete,
                "healthy LM pause evidence survives"
            );
            assert!(e.backend.current.is_empty());
            if let Some(calls) = healthy_calls {
                assert_eq!(
                    e.backend.loaded_calls, calls,
                    "healthy provider is not queried on another provider's retry"
                );
            }
            healthy_calls = Some(e.backend.loaded_calls);
            assert!(
                e.provider_statuses
                    .iter()
                    .any(|report| report.kind == crate::provider::Kind::LMStudio
                        && report.state == crate::coordinator::State::Paused
                        && report.pending)
            );
            assert!(
                e.provider_statuses
                    .iter()
                    .any(|report| report.kind == crate::provider::Kind::Ollama
                        && report.state == crate::coordinator::State::Failed
                        && !report.pending)
            );
        }
        thread.join().unwrap();
        assert_eq!(
            e.backend
                .events
                .iter()
                .filter(|event| event.starts_with("unload:"))
                .count(),
            2
        );
        let observed = reports.lock().unwrap();
        assert!(observed.iter().any(|reports| {
            reports
                .iter()
                .any(|report| report.state == crate::coordinator::State::Pausing && report.pending)
        }));
        assert!(observed.iter().any(|reports| {
            reports
                .iter()
                .any(|report| report.state == crate::coordinator::State::Paused && report.pending)
        }));
        let journal: Journal = serde_json::from_slice(&fs::read(&e.path).unwrap()).unwrap();
        assert_eq!(journal.providers.len(), 1);
        assert_eq!(
            journal.providers[0].binding.kind,
            crate::provider::Kind::LMStudio
        );
    }
    #[test]
    fn confirmed_gameplay_restore_is_immediate_and_stays_coexisting_without_io() {
        use crate::gameplay::fixtures::{evidence, game};
        let current = evidence(vec![game(42, 10), game(7, 2)]);
        let mut e = engine(Fake::new());
        e.config.restore_delay_seconds = 3600.;
        let id = prepare_gameplay(&mut e, &current);
        let feedback = crate::app::confirmed_restore(
            &mut e,
            id,
            &[],
            0.,
            true,
            &mut |_| Some(current.clone()),
            &mut |_| panic!("no selections must not save settings"),
        );
        assert!(feedback.restoration.contains("completed"));
        assert!(!feedback.tracking_recovery);
        assert_eq!(e.activity, Activity::Coexistence);
        assert!(e.gameplay.active());
        assert!(e.state.is_none());
        assert_eq!(e.backend.current.len(), 2);
        assert!(e.config.automation_enabled);
        assert!(!e.manual_pause);
        let events = e.backend.events.clone();
        e.step_games(Some(current.clone()), 1., &mut || Some(current.clone()));
        assert_eq!(
            e.backend.events, events,
            "approved coexistence does not repause or probe models"
        );
        assert!(controls(&e, true).pause);
        assert!(!controls(&e, true).restore);
        let empty = evidence(vec![]);
        e.step_games(Some(empty.clone()), 2., &mut || Some(empty.clone()));
        assert!(!e.gameplay.active());
        assert_eq!(e.activity, Activity::Watching);
    }
    #[test]
    fn confirmation_rechecks_games_before_preferences_or_model_changes() {
        use crate::gameplay::fixtures::{evidence, game};
        let current = evidence(vec![game(42, 10)]);
        for changed in [
            Some(evidence(vec![game(42, 11)])),
            Some(evidence(vec![game(42, 10), game(7, 2)])),
            None,
        ] {
            let mut e = engine(Fake::new());
            let id = prepare_gameplay(&mut e, &current);
            let events = e.backend.events.clone();
            let journal = fs::read(&e.path).unwrap();
            let feedback = crate::app::confirmed_restore(
                &mut e,
                id,
                &[current.all[0].executable.clone()],
                0.,
                true,
                &mut |_| changed.clone(),
                &mut |_| panic!("stale confirmation must not save preferences"),
            );
            assert!(feedback.restoration.contains("refused"));
            assert_eq!(e.backend.events, events);
            assert_eq!(fs::read(&e.path).unwrap(), journal);
            assert!(!e.gameplay.active());
            assert!(e.config.excluded_paths.is_empty());
        }
    }
    #[test]
    fn ignore_save_failure_is_separate_from_successful_gameplay_restore() {
        use crate::gameplay::fixtures::{evidence, game};
        let current = evidence(vec![game(42, 10), game(7, 2)]);
        for fail in [false, true] {
            let mut e = engine(Fake::new());
            let id = prepare_gameplay(&mut e, &current);
            let config_path = e.path.with_file_name("fixture-config.json");
            let mut saved = None;
            let feedback = crate::app::confirmed_restore(
                &mut e,
                id,
                &[current.all[0].executable.clone()],
                0.,
                true,
                &mut |_| Some(current.clone()),
                &mut |config| {
                    if fail {
                        bail!("injected preference save failure");
                    }
                    write_json(&config_path, config)?;
                    saved = Some(config.clone());
                    Ok(())
                },
            );
            assert!(feedback.restoration.contains("completed"));
            assert!(e.gameplay.active());
            if fail {
                assert!(feedback.exclusions.contains("could not be saved"));
                assert!(e.config.excluded_paths.is_empty());
            } else {
                assert!(feedback.exclusions.contains("preferences saved"));
                assert_eq!(
                    saved.unwrap().excluded_paths,
                    vec![current.all[0].executable.clone()]
                );
                let loaded = Config::load(&config_path).unwrap();
                assert_eq!(loaded.excluded_paths, e.config.excluded_paths);
                assert!(!loaded.excluded_paths.contains(&current.all[1].executable));
            }
        }
    }
    #[test]
    fn new_game_between_loads_revokes_scope_and_preserves_original_partial_journal() {
        use crate::gameplay::fixtures::{evidence, game};
        let current = evidence(vec![game(42, 10)]);
        let changed = evidence(vec![game(42, 10), game(7, 2)]);
        let mut e = engine(Fake::new());
        let id = prepare_gameplay(&mut e, &current);
        let original = e.state.as_ref().unwrap().models[0].load_config.clone();
        let path = e.path.clone();
        let feedback = crate::app::confirmed_restore(
            &mut e,
            id,
            &[],
            0.,
            true,
            &mut |_| {
                let restored = has_restored_model(&path);
                Some(if restored {
                    changed.clone()
                } else {
                    current.clone()
                })
            },
            &mut |_| Ok(()),
        );
        assert!(!e.gameplay.active());
        assert!(feedback.restoration.contains("interrupted"));
        assert!(e.state.is_some());
        assert_eq!(e.restore_completions, 0);
        assert_eq!(
            e.backend.current.len(),
            1,
            "only the first model loaded before revocation"
        );
        e.step_games(Some(changed.clone()), 1., &mut || Some(changed.clone()));
        assert_eq!(e.activity, Activity::Paused);
        assert!(e.backend.current.is_empty());
        assert_eq!(e.state.as_ref().unwrap().models[0].load_config, original);
        assert!(
            crate::app::confirmed_restore(
                &mut e,
                id,
                &[],
                2.,
                true,
                &mut |_| Some(current.clone()),
                &mut |_| Ok(())
            )
            .restoration
            .contains("refused")
        );
        let mut resumed = Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
        assert!(!resumed.gameplay.active());
        resumed.restore(&mut || true).unwrap();
        assert!(
            resumed.state.is_some(),
            "ordinary restoration has no gameplay exemption"
        );
    }
    #[test]
    fn partial_gameplay_restore_retries_under_same_scope_and_restart_revokes_it() {
        use crate::gameplay::fixtures::{evidence, game};
        let current = evidence(vec![game(42, 10)]);
        let mut e = engine(Fake::new());
        let id = prepare_gameplay(&mut e, &current);
        e.backend.fail_restore = Some("chat".into());
        let feedback = crate::app::confirmed_restore(
            &mut e,
            id,
            &[],
            0.,
            true,
            &mut |_| Some(current.clone()),
            &mut |_| Ok(()),
        );
        assert!(feedback.tracking_recovery);
        assert!(feedback.restoration.contains("failed"));
        assert!(e.gameplay.active());
        assert!(e.backend.current.contains("embed"));
        let mut resumed = Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
        assert!(!resumed.gameplay.active());
        resumed.step_games(Some(current.clone()), 1., &mut || Some(current.clone()));
        assert!(
            resumed.backend.current.is_empty(),
            "restart repauses partial restoration while gaming"
        );
        e.backend.fail_restore = None;
        let retry = e
            .coordinator_memory
            .as_ref()
            .unwrap()
            .next_retry_at()
            .unwrap()
            .as_secs_f64();
        e.step_games(Some(current.clone()), retry + 0.001, &mut || {
            Some(current.clone())
        });
        assert_eq!(e.activity, Activity::Coexistence);
        assert!(e.state.is_none());
        assert_eq!(e.backend.current.len(), 2);
    }
    #[test]
    fn unknown_detection_revokes_approval_then_verified_detection_repauses() {
        use crate::gameplay::fixtures::{evidence, game};
        let current = evidence(vec![game(42, 10)]);
        let mut e = engine(Fake::new());
        let id = prepare_gameplay(&mut e, &current);
        crate::app::confirmed_restore(
            &mut e,
            id,
            &[],
            0.,
            true,
            &mut |_| Some(current.clone()),
            &mut |_| Ok(()),
        );
        let events = e.backend.events.clone();
        e.step_games(None, 1., &mut || None);
        assert!(!e.gameplay.active());
        assert_eq!(e.activity, Activity::DetectionUnavailable);
        assert_eq!(
            e.backend.events, events,
            "unknown detection does not invent an empty game set"
        );
        e.step_games(Some(current.clone()), 2., &mut || Some(current.clone()));
        assert_eq!(e.activity, Activity::Paused);
        assert!(e.backend.current.is_empty());
    }
    #[test]
    fn observation_and_unknown_readiness_refuse_confirmation_without_saving() {
        use crate::gameplay::fixtures::{evidence, game};
        let current = evidence(vec![game(42, 10)]);
        for ready in [true, false] {
            let mut e = engine(Fake::new());
            let id = prepare_gameplay(&mut e, &current);
            if ready {
                e.config.mode = "observe".into();
            }
            let events = e.backend.events.clone();
            let journal = fs::read(&e.path).unwrap();
            let feedback = crate::app::confirmed_restore(
                &mut e,
                id,
                &[current.all[0].executable.clone()],
                0.,
                ready,
                &mut |_| Some(current.clone()),
                &mut |_| panic!("refused confirmation cannot save selections"),
            );
            assert!(feedback.restoration.contains("refused"));
            assert_eq!(e.backend.events, events);
            assert_eq!(fs::read(&e.path).unwrap(), journal);
            assert!(!e.gameplay.active());
        }
    }
    #[test]
    fn ignore_without_confirmation_does_not_restore_or_drop_remembered_games() {
        use crate::gameplay::fixtures::{evidence, game};
        let mut current = evidence(vec![game(42, 10)]);
        let mut e = engine(Fake::new());
        prepare_gameplay(&mut e, &current);
        e.gameplay.invalidate_offer();
        e.config = crate::gameplay::selected_exclusions(
            &e.config,
            &current.all,
            &[current.all[0].executable.clone()],
        )
        .unwrap();
        current.triggers.clear();
        let remembered = e.state.as_ref().unwrap().games[0].path.clone();
        e.step_games(Some(current.clone()), 1., &mut || Some(current.clone()));
        assert!(e.backend.current.is_empty());
        assert!(!e.gameplay.active());
        e.restore(&mut || current.gaming()).unwrap();
        assert!(e.state.is_some());
        assert_eq!(e.state.as_ref().unwrap().games[0].path, remembered);
    }
    fn controls(e: &Engine<Fake>, gaming: bool) -> crate::control::Availability {
        crate::control::ControlState {
            activity: e.activity,
            active_mode: e.config.mode == "active",
            provider_enabled: e.config.lm_enabled(),
            detection_ready: !e.disabled,
            gaming,
            pending: e.state.is_some(),
            manual_hold: e.manual_pause,
            gameplay_restore: e.gameplay.active(),
            coexistence: e.gameplay.active(),
        }
        .availability()
    }
    #[test]
    fn actual_transitions_drive_core_availability_and_publish_progress() {
        use std::sync::{Arc, Mutex};
        let progress = Arc::new(Mutex::new(Vec::new()));
        let sink = progress.clone();
        let mut e = engine(Fake::new());
        e.observe_progress(move |activity| sink.lock().unwrap().push(activity));
        e.step(false, 0., &mut || false);
        assert!(controls(&e, false).pause);
        e.step(true, 1., &mut || false);
        assert_eq!(e.activity, Activity::Paused);
        assert!(!controls(&e, true).pause);
        assert!(!controls(&e, true).restore);
        let work = e.backend.events.len();
        e.step(true, 2., &mut || false);
        assert_eq!(e.backend.events.len(), work, "completed pause stays quiet");
        assert!(!e.manual_pause, "automatic pause never becomes a hold");
        e.step(false, 3., &mut || false);
        assert_eq!(e.activity, Activity::Countdown);
        assert!(controls(&e, false).restore);
        e.step(false, 33., &mut || false);
        assert_eq!(e.activity, Activity::Watching);
        assert!(!controls(&e, false).restore);
        let observed = progress.lock().unwrap();
        for activity in [
            Activity::Capturing,
            Activity::Unloading,
            Activity::Restoring,
        ] {
            assert!(observed.contains(&activity));
            let availability = crate::control::ControlState {
                activity,
                active_mode: true,
                provider_enabled: true,
                detection_ready: true,
                gaming: false,
                pending: true,
                manual_hold: true,
                gameplay_restore: false,
                coexistence: false,
            }
            .availability();
            assert!(!availability.pause && !availability.restore && !availability.resume);
        }
    }
    #[test]
    fn partial_pause_and_restore_are_never_completed_pause_evidence() {
        let mut backend = Fake::new();
        backend.fail_unload = Some("embed".into());
        let mut e = engine(backend);
        e.step(true, 0., &mut || false);
        assert_eq!(e.activity, Activity::PartialFailure);
        assert!(!e.state.as_ref().unwrap().pause_complete);
        assert_eq!(e.pause_completions, 0);
        assert!(!controls(&e, true).pause);
        e.step(false, 1., &mut || false);
        assert_eq!(e.activity, Activity::Recovery);
        assert!(e.message.contains("pause was incomplete"));
        e.backend.fail_unload = None;
        e.backend.fail_restore = Some("chat".into());
        let result = e.restore(&mut || false);
        e.restore_failed = true;
        e.attempt(result, 1.);
        assert_eq!(e.activity, Activity::PartialFailure);
        assert!(e.state.is_some());
        assert_eq!(e.restore_completions, 0);
        assert!(controls(&e, false).restore);
    }
    #[test]
    fn explicit_manual_pause_retries_before_old_backoff_and_clears_old_failure() {
        let mut e = engine(Fake::new());
        e.attempt(Err(anyhow::anyhow!("Fixture previous capture failed")), 0.);
        assert!(e.retry_at > 1.);
        e.request_manual_pause();
        e.step(false, 1., &mut || false);
        assert_eq!(e.activity, Activity::ManualHold);
        assert!(e.last_error.is_empty());
        assert!(e.state.as_ref().unwrap().pause_complete);
    }
    #[test]
    fn delayed_unload_retains_intent_and_retries_without_false_completion() {
        let mut backend = Fake::new();
        backend.delayed_unload = true;
        let mut e = engine(backend);
        e.step(true, 0., &mut || false);
        assert_eq!(e.activity, Activity::PartialFailure);
        assert_eq!(e.pause_completions, 0);
        assert!(!e.state.as_ref().unwrap().pause_complete);
        let original = e.state.as_ref().unwrap().models[0].load_config.clone();
        e.backend.delayed_unload = false;
        let retry = e
            .coordinator_memory
            .as_ref()
            .unwrap()
            .next_retry_at()
            .unwrap()
            .as_secs_f64();
        let calls = e.backend.loaded_calls;
        let journal = fs::read(&e.path).unwrap();
        e.step(true, retry - 0.001, &mut || false);
        assert_eq!(e.backend.loaded_calls, calls, "backoff must not probe");
        assert_eq!(
            fs::read(&e.path).unwrap(),
            journal,
            "backoff must not persist"
        );
        e.step(true, retry + 0.001, &mut || false);
        assert_eq!(e.activity, Activity::Paused);
        assert_eq!(e.pause_completions, 1);
        assert_eq!(e.state.as_ref().unwrap().models[0].load_config, original);
    }
    #[test]
    fn busy_inference_and_failed_detection_remain_explicit() {
        let mut e = engine(Fake::new());
        e.attempt(Err(crate::lmstudio::InferenceBusy.into()), 0.);
        assert_eq!(e.activity, Activity::WaitingForInference);
        assert_eq!(e.pause_completions, 0);
        assert!(e.state.is_none());
        e.disabled = true;
        e.step(false, 1., &mut || false);
        assert_eq!(e.activity, Activity::DetectionUnavailable);
        assert!(!controls(&e, false).pause);
        assert!(!controls(&e, false).restore);
    }
    #[test]
    fn failed_final_pause_write_cannot_claim_completion_on_retry() {
        let mut backend = Fake::new();
        backend.lock_loaded_on = Some(3);
        let mut e = engine(backend);
        e.step(true, 0., &mut || false);
        assert_eq!(e.activity, Activity::PartialFailure);
        assert_eq!(e.pause_completions, 0);
        assert!(!e.state.as_ref().unwrap().pause_complete);
        e.backend.held_lock = None;
        let disk: Journal = serde_json::from_slice(&fs::read(&e.path).unwrap()).unwrap();
        assert!(!disk.into_lm().unwrap().1.pause_complete);
        e.step(true, e.config.retry_seconds, &mut || false);
        assert_eq!(e.activity, Activity::Paused);
        assert_eq!(e.pause_completions, 1);
    }
    #[test]
    fn both_models_and_grace() {
        let mut e = engine(Fake::new());
        e.step(true, 0., &mut || false);
        assert!(e.backend.current.is_empty());
        assert!(!e.backend.running);
        e.step(false, 2., &mut || false);
        e.step(false, 31., &mut || false);
        assert!(e.backend.current.is_empty());
        e.step(false, 32., &mut || false);
        assert_eq!(e.backend.current.len(), 2);
        assert!(e.backend.running);
        assert!(e.state.is_none());
    }
    #[test]
    fn completed_pause_does_no_io() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        let events = e.backend.events.clone();
        let bytes = fs::read(&e.path).unwrap();
        e.path = PathBuf::from(r"Z:\unwritable-test-target\state.json");
        e.pause().unwrap();
        assert_eq!(e.backend.events, events);
        assert!(!bytes.is_empty());
    }
    #[test]
    fn automation_off_does_not_start_pause_but_recovers_existing_session() {
        let mut e = engine(Fake::new());
        e.config.automation_enabled = false;
        e.step(true, 0., &mut || false);
        assert!(e.backend.events.is_empty());
        e.config.automation_enabled = true;
        e.step(true, 1., &mut || false);
        let saved = fs::read(&e.path).unwrap();
        e.config.automation_enabled = false;
        e.step(true, 200., &mut || false);
        assert_eq!(saved, fs::read(&e.path).unwrap());
        assert!(e.backend.current.is_empty());
        e.step(false, 201., &mut || false);
        e.step(false, 231., &mut || false);
        assert!(e.state.is_none());
        assert_eq!(e.backend.current.len(), 2);
    }
    #[test]
    fn abandoned_pause_error_clears_when_no_game_or_recovery_remains() {
        let mut e = engine(Fake::new());
        e.last_error = "AI is busy".into();
        e.step(false, 0., &mut || false);
        assert!(e.last_error.is_empty());
        assert_eq!(
            e.message,
            "Watching games; LM Studio state has not been probed"
        );
    }
    #[test]
    fn recovery_remembers_game_paths_across_registration_changes_and_restart() {
        let mut e = engine(Fake::new());
        let game = crate::discovery::Game::new("Custom", "Game", "Game", r"D:\Game\play.exe");
        e.remember_games(vec![game.clone()]).unwrap();
        e.pause().unwrap();
        e.remember_games(vec![]).unwrap();
        let resumed = Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
        assert_eq!(resumed.remembered_games.len(), 1);
        assert_eq!(resumed.remembered_games[0].path, game.path);
    }
    #[test]
    fn partial_unload_preserves_snapshot() {
        let mut b = Fake::new();
        b.fail_unload = Some("embed".into());
        let mut e = engine(b);
        e.step(true, 0., &mut || false);
        assert_eq!(e.backend.current.len(), 1);
        assert_eq!(e.state.as_ref().unwrap().models.len(), 2);
        e.backend.fail_unload = None;
        e.step(true, 31., &mut || false);
        assert!(e.backend.current.is_empty());
        e.restore(&mut || false).unwrap();
        assert_eq!(e.backend.current.len(), 2);
    }
    #[test]
    fn partial_restore_recovers_on_restart() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        e.backend.fail_restore = Some("embed".into());
        assert!(e.restore(&mut || false).is_err());
        assert_eq!(e.backend.current.len(), 1);
        let mut backend = e.backend.clone();
        backend.fail_restore = None;
        let mut resumed = Engine::new(e.config.clone(), backend, e.path.clone()).unwrap();
        resumed.restore(&mut || false).unwrap();
        assert_eq!(resumed.backend.current.len(), 2);
        assert!(resumed.state.is_none());
    }
    #[test]
    fn observation_never_controls_ai() {
        let mut e = engine(Fake::new());
        e.config.mode = "observe".into();
        e.step(true, 0., &mut || false);
        e.step(false, 100., &mut || false);
        assert!(e.backend.events.is_empty());
        assert!(!e.path.exists());
    }
    #[test]
    fn disabled_provider_does_no_io_and_retains_an_existing_obligation() {
        let mut e = engine(Fake::new());
        let original = e.config.providers.clone();
        e.config.providers.clear();
        e.step(true, 0., &mut || false);
        assert!(e.pause().is_err());
        assert!(!e.verify_round_trip(&mut || false).ok);
        assert!(e.backend.events.is_empty());
        assert_eq!(e.backend.loaded_calls, 0);
        assert!(!e.path.exists());
        e.config.providers = original;
        e.pause().unwrap();
        let journal = fs::read(&e.path).unwrap();
        let events = e.backend.events.clone();
        e.config.providers.clear();
        assert!(e.restore(&mut || false).is_err());
        assert_eq!(e.backend.events, events);
        assert_eq!(fs::read(&e.path).unwrap(), journal);
        assert!(e.state.is_some());
    }
    #[test]
    fn empty_snapshot_never_loads_default() {
        let mut b = Fake::new();
        b.current.clear();
        b.running = false;
        let mut e = engine(b);
        e.pause().unwrap();
        e.restore(&mut || false).unwrap();
        assert!(e.backend.current.is_empty());
        assert!(!e.backend.running);
        assert!(!e.backend.events.iter().any(|v| v.starts_with("restore:")));
    }
    #[test]
    fn switching_games_resets_timer() {
        let mut e = engine(Fake::new());
        e.step(true, 0., &mut || false);
        e.step(false, 1., &mut || false);
        e.step(true, 29., &mut || false);
        e.step(false, 30., &mut || false);
        e.step(false, 59., &mut || false);
        assert!(e.backend.current.is_empty());
        e.step(false, 60., &mut || false);
        assert_eq!(e.backend.current.len(), 2);
    }
    #[test]
    fn disabled_keeps_journal() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        let bytes = fs::read(&e.path).unwrap();
        e.disabled = true;
        e.step(false, 100., &mut || false);
        assert_eq!(bytes, fs::read(&e.path).unwrap());
    }
    #[test]
    fn manual_pause_holds_session() {
        let mut e = engine(Fake::new());
        e.manual_pause = true;
        e.step(false, 0., &mut || false);
        e.step(false, 100., &mut || false);
        assert!(e.backend.current.is_empty());
        e.manual_pause = false;
        e.step(false, 101., &mut || false);
        e.step(false, 131., &mut || false);
        assert_eq!(e.backend.current.len(), 2);
    }
    #[test]
    fn stop_failure_retries_safely() {
        let mut b = Fake::new();
        b.fail_stop = true;
        let mut e = engine(b);
        e.step(true, 0., &mut || false);
        assert_eq!(e.backend.current.len(), 2);
        assert!(e.state.as_ref().unwrap().server_stopped);
        e.backend.fail_stop = false;
        e.step(true, 31., &mut || false);
        assert!(e.backend.current.is_empty());
        assert_eq!(
            e.backend.events.iter().filter(|s| *s == "snapshot").count(),
            1
        );
    }
    #[test]
    fn restart_interrupts_between_models() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        let path = e.path.clone();
        e.restore(&mut || has_restored_model(&path)).unwrap();
        assert_eq!(e.backend.current.len(), 1);
        assert!(e.state.is_some());
        e.step(true, 10., &mut || false);
        assert!(e.backend.current.is_empty());
    }
    #[test]
    fn restart_before_restore_keeps_server_off() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        e.restore(&mut || true).unwrap();
        assert!(!e.backend.running);
        assert!(e.backend.current.is_empty());
    }
    #[test]
    fn unknown_journal_format_refused() {
        let e = engine(Fake::new());
        fs::write(
            &e.path,
            r#"{"schema":9,"server":{},"server_stopped":false,"models":[]}"#,
        )
        .unwrap();
        assert!(Engine::new(e.config, e.backend, e.path).is_err());
    }
    #[test]
    fn failed_restore_keeps_journal_and_surfaces_explicit_status() {
        // P0-4: a failed Engine::restore must (a) leave the pending journal
        // intact and resumable, and (b) surface the explicit
        // "Restore failed — AI not restored" status rather than a generic
        // error line. A *pause* failure must NOT be mislabelled as a restore
        // failure.
        let mut b = Fake::new();
        b.fail_restore = Some("embed".into());
        let mut e = engine(b);
        e.pause().unwrap();
        // First restore attempt fails on "embed"; the journal must survive.
        let result = e.restore(&mut || false);
        assert!(result.is_err());
        e.restore_failed = true;
        e.attempt(result, 100.);
        // (b) explicit, non-surprising status.
        assert!(
            e.message.starts_with("Restore failed — AI not restored"),
            "expected the explicit restore-failure status, got {:?}",
            e.message
        );
        // (a) journal retained on disk and still a valid, resumable recovery.
        let bytes = fs::read(&e.path).unwrap();
        assert!(!bytes.is_empty());
        assert!(
            e.state.is_some(),
            "state must be retained after a failed restore"
        );
        let mut resumed = Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
        assert_eq!(resumed.state.as_ref().unwrap().models.len(), 2);
        // Recovery resumes where it stopped and completes cleanly.
        resumed.backend.fail_restore = None;
        resumed.restore(&mut || false).unwrap();
        assert_eq!(resumed.backend.current.len(), 2);
        assert!(resumed.state.is_none());
    }
    #[test]
    fn pause_failure_is_not_labeled_a_restore_failure() {
        let mut b = Fake::new();
        b.fail_unload = Some("embed".into());
        let mut e = engine(b);
        // step() internally drives pause(); the failure must be reported as
        // "Needs attention", never "Restore failed".
        e.step(true, 0., &mut || false);
        assert!(
            !e.message.starts_with("Restore failed"),
            "a pause failure must not be mislabelled as a restore failure, got {:?}",
            e.message
        );
        assert!(
            e.message.starts_with("Needs attention"),
            "got {:?}",
            e.message
        );
        // A later successful operation must not leave the flag set either.
        assert!(
            !e.restore_failed,
            "restore_failed must not linger after a pause failure"
        );
    }
    #[test]
    fn invalid_stage_retains_journal_instead_of_clearing_recovery() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        e.state.as_mut().unwrap().models[0].stage = "unknown".into();
        assert!(e.save().is_err());
        let mut corrupt: serde_json::Value =
            serde_json::from_slice(&fs::read(&e.path).unwrap()).unwrap();
        corrupt["providers"][0]["payload"]["snapshot"]["models"][0]["stage"] = "unknown".into();
        write_json(&e.path, &corrupt).unwrap();
        let original = fs::read(&e.path).unwrap();
        assert!(Engine::new(e.config, e.backend, e.path.clone()).is_err());
        assert_eq!(fs::read(e.path).unwrap(), original);
    }
    // P2-1 acceptance: with the mock round-tripping cleanly, verify reports
    // success and exercises every logical step.
    #[test]
    fn verify_reports_success_when_mock_round_trips() {
        let mut e = engine(Fake::new());
        let report = e.verify_round_trip(&mut || false);
        assert!(
            report.ok,
            "expected a successful round-trip, got: {}",
            report.summary
        );
        let names: Vec<&str> = report.steps.iter().map(|s| s.name.as_str()).collect();
        // Capture, unload and verify-fields are always present; the server was
        // running in the Fake, so verify-unloaded is exercised too.
        for expected in [
            "capture",
            "unload",
            "verify-unloaded",
            "restore",
            "verify-fields",
        ] {
            assert!(
                names.contains(&expected),
                "missing step {expected}; got {names:?}"
            );
        }
        // The engine also records the report for the dashboard/CLI to render.
        assert_eq!(e.verify_report.as_ref().map(|r| r.ok), Some(true));
    }
    // P2-1 acceptance: when the mock returns a mutated config on restore,
    // verify reports the EXACT failing field, not an opaque error.
    #[test]
    fn verify_reports_exact_failing_field_when_config_mutated() {
        let mut e = engine(Fake::new());
        e.backend.mutate_read = true;
        let report = e.verify_round_trip(&mut || false);
        assert!(
            !report.ok,
            "expected a failing round-trip, got: {}",
            report.summary
        );
        // The field-compare step is the one that fails…
        let failing = report.steps.iter().find(|s| !s.ok).expect("a failing step");
        assert!(
            report
                .steps
                .iter()
                .any(|s| s.name == "verify-fields" && !s.ok)
        );
        // …and it names the exact field that drifted (temperature).
        assert!(
            failing.detail.contains("temperature"),
            "detail must name the failing field, got: {:?}",
            failing.detail
        );
        assert!(
            failing.detail.contains("differs"),
            "detail should carry compare_fields' message, got: {:?}",
            failing.detail
        );
    }
    #[test]
    fn round_trip_tests_lm_studio_then_ollama_and_reports_each() {
        use serde_json::json;
        let model = json!({"name":"fixture-http:latest", "model":"fixture-http:latest",
            "digest":"c".repeat(64), "context_length":4096, "expires_at":"2262-01-01T00:00:00Z"});
        let mut present = true;
        let (endpoint, thread) = crate::ollama_session::tests::http_server(
            12,
            move |route, body| {
                let value = match route {
                    "/api/ps" => {
                        json!({"models":if present { vec![model.clone()] } else { vec![] }})
                    }
                    "/api/tags" => json!({"models":[model.clone()]}),
                    "/api/show" => {
                        json!({"details":{"format":"gguf"}, "capabilities":["completion"],
                        "model_info":{"general.architecture":"fixture", "fixture.context_length":8192}})
                    }
                    "/api/generate" => {
                        let body = body.unwrap();
                        let unloading = body["keep_alive"] == 0;
                        present = !unloading;
                        json!({"model":body["model"], "done":true,
                            "done_reason":if unloading {"unload"} else {"load"}, "response":""})
                    }
                    _ => panic!("unexpected fixture route"),
                };
                (200, serde_json::to_vec(&value).unwrap())
            },
        );
        let mut e = engine(Fake::new());
        for provider in &mut e.config.providers {
            if let crate::config::Provider::Ollama {
                enabled,
                endpoint: route,
                ..
            } = provider
            {
                *enabled = true;
                *route = endpoint.clone();
            }
        }
        e.config.validate().unwrap();
        let report = e.verify_round_trip(&mut || false);
        assert!(report.ok, "{report:?}");
        assert!(!e.pending());
        assert_eq!(e.backend.current.len(), 2);
        thread.join().unwrap();
        let names = report
            .steps
            .iter()
            .map(|step| step.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "capture",
                "unload",
                "verify-unloaded",
                "restore",
                "verify-fields",
                "ollama-unload",
                "ollama-restore"
            ]
        );
        assert!(report.steps[5].detail.starts_with("1 model(s) unloaded"));
        // With nothing listening, the Ollama half reports that and still passes.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let unused = closed.local_addr().unwrap().to_string();
        drop(closed);
        for provider in &mut e.config.providers {
            if let crate::config::Provider::Ollama { endpoint, .. } = provider {
                *endpoint = unused.clone();
            }
        }
        e.settings_changed();
        let report = e.verify_round_trip(&mut || false);
        assert!(report.ok, "{report:?}");
        assert_eq!(
            report.steps[5].detail,
            "Ollama is not running; nothing to test"
        );
    }
    #[test]
    fn verify_guards_do_not_mutate_or_replace_recovery() {
        for mode in 0..5 {
            let mut e = engine(Fake::new());
            match mode {
                0 => e.config.mode = "observe".into(),
                1 => e.manual_pause = true,
                2 => e.disabled = true,
                3 => {
                    e.pause().unwrap();
                }
                _ => (),
            }
            let before = e.backend.events.clone();
            let disk = fs::read(&e.path).ok();
            assert!(!e.verify_round_trip(&mut || mode == 4).ok);
            assert_eq!(before, e.backend.events);
            assert_eq!(disk, fs::read(&e.path).ok());
        }
    }
    #[test]
    fn verify_restores_original_server_state_and_avoids_redundant_start() {
        for running in [false, true] {
            let mut b = Fake::new();
            b.running = running;
            let mut e = engine(b);
            e.config.lm_mut().unwrap().stop_server_during_gaming = false;
            assert!(e.verify_round_trip(&mut || false).ok);
            assert_eq!(e.backend.running, running);
            assert_eq!(e.backend.current.len(), 2);
            assert_eq!(
                e.backend.events.iter().filter(|s| *s == "start").count(),
                usize::from(!running)
            );
        }
    }
    #[test]
    fn verify_partial_unload_and_inventory_failure_attempt_cleanup() {
        for failure in 0..3 {
            let mut b = Fake::new();
            match failure {
                0 => b.fail_unload = Some("chat".into()),
                1 => b.fail_unload = Some("embed".into()),
                _ => b.fail_loaded_on = Some(3),
            }
            let mut e = engine(b);
            assert!(!e.verify_round_trip(&mut || false).ok);
            assert_eq!(e.backend.current.len(), 2);
            assert!(
                e.state.is_none(),
                "cleanup succeeded despite original failure"
            );
        }
    }
    #[test]
    fn verify_restore_or_read_failure_recovers_other_models_and_survives_restart() {
        for read in [false, true] {
            let mut b = Fake::new();
            if read {
                b.fail_read = Some("chat".into());
            } else {
                b.fail_restore = Some("chat".into());
            }
            let mut e = engine(b);
            let report = e.verify_round_trip(&mut || false);
            assert!(!report.ok);
            assert!(report.summary.contains("recovery pending"));
            assert!(
                e.backend.current.contains("embed"),
                "later model must still recover"
            );
            let mut resumed =
                Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
            resumed.backend.fail_read = None;
            resumed.backend.fail_restore = None;
            resumed.restore(&mut || false).unwrap();
            assert!(resumed.state.is_none());
            assert_eq!(resumed.backend.current.len(), 2);
        }
    }
    #[test]
    fn verify_game_start_after_unload_retains_durable_recovery() {
        let mut e = engine(Fake::new());
        let journal = e.path.clone();
        assert!(
            !e.verify_round_trip(&mut || {
                fs::read(&journal)
                    .ok()
                    .and_then(|bytes| {
                        serde_json::from_slice::<Option<Journal>>(&bytes)
                            .ok()
                            .flatten()
                    })
                    .is_some_and(|journal| {
                        let (_, snapshot, _) = journal.into_lm().unwrap();
                        !snapshot.models.is_empty()
                            && snapshot
                                .models
                                .iter()
                                .all(|model| model.stage == "unloaded")
                    })
            })
            .ok
        );
        assert!(e.backend.current.is_empty());
        assert!(e.state.is_some());
        let mut resumed = Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
        resumed.restore(&mut || false).unwrap();
        assert_eq!(resumed.backend.current.len(), 2);
    }
    #[test]
    fn verify_cannot_unload_when_initial_journal_write_fails() {
        let mut e = engine(Fake::new());
        fs::create_dir(&e.path).unwrap();
        assert!(!e.verify_round_trip(&mut || false).ok);
        assert_eq!(e.backend.events, vec!["snapshot"]);
        assert_eq!(e.backend.current.len(), 2);
    }
    #[test]
    fn restart_recovers_each_destructive_stage() {
        for running in [false, true] {
            for stage in ["planned", "unloading", "unloaded", "restoring", "restored"] {
                let mut backend = Fake::new();
                backend.running = running;
                let mut e = engine(backend);
                if stage == "planned" {
                    e.state = Some(e.backend.snapshot().unwrap());
                } else {
                    e.pause().unwrap();
                }
                for model in &mut e.state.as_mut().unwrap().models {
                    model.stage = stage.into();
                }
                e.save().unwrap();
                let mut resumed =
                    Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
                resumed.restore(&mut || false).unwrap();
                assert!(resumed.state.is_none());
                assert_eq!(resumed.backend.current.len(), 2);
                assert_eq!(resumed.backend.running, running);
            }
        }
    }
    #[test]
    fn failed_stage_and_final_writes_keep_recovery_in_memory_and_on_disk() {
        for boundary in 0..3 {
            let mut b = Fake::new();
            b.running = false;
            match boundary {
                0 => b.lock_after_unload = true,
                1 => b.lock_after_restore = true,
                _ => b.lock_after_stop = true,
            }
            let mut e = engine(b);
            assert!(!e.verify_round_trip(&mut || false).ok);
            assert!(e.state.is_some());
            e.backend.held_lock = None;
            e.backend.lock_after_unload = false;
            e.backend.lock_after_restore = false;
            e.backend.lock_after_stop = false;
            let mut resumed =
                Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
            assert!(resumed.state.is_some());
            resumed.restore(&mut || false).unwrap();
            assert!(resumed.state.is_none());
            assert_eq!(resumed.backend.current.len(), 2);
        }
    }
    #[test]
    fn interrupted_restore_of_initially_stopped_server_is_closed_on_repause() {
        let mut b = Fake::new();
        b.running = false;
        let mut e = engine(b);
        e.pause().unwrap();
        let path = e.path.clone();
        e.restore(&mut || has_restored_model(&path)).unwrap();
        assert!(e.backend.running);
        e.pause().unwrap();
        assert!(!e.backend.running);
        assert!(e.backend.current.is_empty());
    }
    #[test]
    fn verify_nondefault_port_and_server_failures_remain_recoverable() {
        for failure in 0..3 {
            let mut b = Fake::new();
            b.running = false;
            b.port = 4321;
            b.fail_start = failure == 1;
            b.fail_stop = failure == 2;
            let mut e = engine(b);
            let report = e.verify_round_trip(&mut || false);
            assert_eq!(report.ok, failure == 0);
            assert_eq!(e.backend.port, 4321);
            if failure != 0 {
                assert!(e.state.is_some());
                e.backend.fail_start = false;
                e.backend.fail_stop = false;
                let mut resumed =
                    Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
                resumed.restore(&mut || false).unwrap();
                assert!(resumed.state.is_none());
            }
            assert_eq!(e.backend.current.len(), if failure == 1 { 0 } else { 2 });
        }
    }
    #[test]
    fn externally_changed_port_does_not_load_into_another_server() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        e.backend.running = true;
        e.backend.port = 4322;
        assert!(e.restore(&mut || false).is_err());
        assert!(e.backend.current.is_empty());
        assert!(e.state.is_some());
    }
    #[test]
    fn legacy_empty_stopped_server_journal_remains_schema_two_compatible() {
        let mut backend = Fake::new();
        backend.running = false;
        let mut e = engine(backend);
        write_json(&e.path, &json!({"schema":2,"server":{"running":false},"server_stopped":false,"models":[],"pause_complete":true})).unwrap();
        e = Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
        assert_eq!(e.state.as_ref().unwrap().schema, 2);
        assert_eq!(e.state.as_ref().unwrap().server["port"], 1234);
        e.restore(&mut || false).unwrap();
        assert!(e.state.is_none());
    }
    #[test]
    fn empty_recovery_cannot_clear_an_unverified_original_server_state() {
        let mut e = engine(Fake::new());
        write_json(&e.path, &json!({"schema":2,"server":{"running":false},"server_stopped":false,"models":[],"pause_complete":true})).unwrap();
        e = Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
        assert!(
            e.restore(&mut || false)
                .unwrap_err()
                .to_string()
                .contains("server state is unverified")
        );
        assert!(e.state.is_some());
        assert!(e.backend.running);
        assert!(
            e.backend.events.is_empty(),
            "do not stop an externally started service without saved control intent"
        );
        assert_eq!(e.restore_completions, 0);
        let journal: Journal = serde_json::from_slice(&fs::read(&e.path).unwrap()).unwrap();
        assert!(!journal.providers[0].restore_complete);
    }
}
