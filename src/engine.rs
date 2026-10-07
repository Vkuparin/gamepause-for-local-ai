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
    /// The watcher saw no LM Studio process in its last scan. Its CLI starts
    /// LM Studio when asked anything, so a closed LM Studio that owes no
    /// recovery is left out of control instead of being woken by a game.
    pub lm_closed: bool,
    /// `lm_closed` applies: nothing is owed to LM Studio.
    lm_idle: bool,
    /// Approximate model memory the current pause released; 0 when unknown,
    /// including after a restart mid-pause.
    pub freed_bytes: u64,
    /// Round-trip verification drives one provider at a time.
    verify_scope: Option<crate::provider::Kind>,
    provider_progress: Option<ProviderObserver>,
    coordinator_memory: Option<crate::coordinator::Continuation<crate::recovery::Payload>>,
    adapter_progress: Option<crate::lm_session::Progress>,
    ollama_runtime: crate::provider_runtime::OllamaRuntime,
    process_runtime: crate::provider_runtime::ProcessRuntime,
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
            lm_closed: false,
            lm_idle: false,
            freed_bytes: 0,
            verify_scope: None,
            provider_progress: None,
            coordinator_memory: None,
            adapter_progress: None,
            ollama_runtime: Default::default(),
            process_runtime: Default::default(),
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
                        crate::recovery::Payload::Process(snapshot) => snapshot.pause_complete,
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
        if self.lm_missing || self.lm_idle {
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
        self.lm_idle = self.config.lm_enabled() && !owed && self.lm_closed;
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
                crate::recovery::Payload::Process(snapshot) => snapshot.units() == 0,
            })
        })
    }
    fn paused_message(&self) -> String {
        match (self.manual_pause, self.nothing_held()) {
            (true, true) => "No AI models were loaded; Resume AI releases the hold".into(),
            (true, false) => "AI paused by you; choose Resume AI to release the hold".into(),
            (false, true) => "Game running; no AI models were loaded, so nothing was paused".into(),
            (false, false) if self.freed_bytes > 0 => format!(
                "AI paused for gaming; about {} freed",
                crate::presentation::size(self.freed_bytes)
            ),
            (false, false) => "AI paused for gaming".into(),
        }
    }
    /// Sizes the adapters saw at this session's capture, counted only for
    /// providers whose journal entry actually holds models.
    fn captured_bytes(&mut self) -> u64 {
        let Some(journal) = &self.recovery else {
            return 0;
        };
        let (mut lm, mut ollama) = (false, false);
        for entry in &journal.providers {
            match &entry.payload {
                crate::recovery::Payload::LMStudio(snapshot) => lm |= !snapshot.models.is_empty(),
                crate::recovery::Payload::Ollama(snapshot) => ollama |= snapshot.units() > 0,
                crate::recovery::Payload::Process(_) => (),
            }
        }
        let lm = if lm { self.backend.captured_bytes() } else { 0 };
        let ollama = if ollama {
            self.ollama_runtime.captured_bytes()
        } else {
            0
        };
        lm.saturating_add(ollama)
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
        if !self.provider_pending(crate::provider::Kind::Process) {
            self.process_runtime = Default::default();
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
                    crate::recovery::Payload::Process(snapshot) => {
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
            if !self
                .remembered_games
                .iter()
                .any(|g| crate::discovery::same_path(&g.path, &game.path))
            {
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
            } else if self.lm_idle {
                "Watching games; LM Studio is not running, so there is nothing to pause".into()
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
                } else if self.last_error.is_empty() {
                    // Unverified, not failed: a restart or a fresh journal
                    // carries no evidence either way.
                    self.set_activity(Activity::Recovery);
                    self.message = format!(
                        "Saved AI is waiting to be restored; restoring in {}s",
                        remaining.ceil() as u64
                    );
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
            self.message = self.paused_message();
            return Ok(());
        }
        let fresh = !self.pending();
        self.pause_units(cancelled, only_lm)?;
        if fresh {
            self.freed_bytes = self.captured_bytes();
        }
        // A pause that found nothing loaded is not a success to announce.
        if !self.nothing_held() {
            self.pause_completions += 1;
        }
        self.set_activity(if self.manual_pause {
            Activity::ManualHold
        } else {
            Activity::Paused
        });
        self.message = self.paused_message();
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
            &mut self.process_runtime,
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
                                    crate::recovery::Payload::Process(snapshot) => snapshot.units(),
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
            bail!("AI recovery is pending; re-enable its provider in Advanced. Recovery retained");
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
        self.freed_bytes = 0;
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
                        crate::recovery::Payload::Process(snapshot) => snapshot.units(),
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
            &mut self.process_runtime,
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
mod tests;
