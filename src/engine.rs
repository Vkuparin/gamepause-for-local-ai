//! The session state machine: when to pause and restore, given game evidence,
//! grace, manual holds, gameplay approval and power changes. `Engine` owns that
//! state and the recovery journal view; `provider_work` drives the coordinator
//! units and `verification` runs the round-trip test.

mod provider_work;
mod verification;

pub use verification::{VerifyReport, VerifyStep};

#[cfg(test)]
use crate::config::write_json;
use crate::{
    config::Config,
    control::Activity,
    gameplay::{GameEvidence, GameplayControl},
    lmstudio::{Backend, Snapshot},
    recovery::{self, Binding, Intent, Journal},
};
use anyhow::{Result, bail};
#[cfg(test)]
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
type ProviderObserver = Box<dyn FnMut(&[crate::coordinator::Report]) + Send>;

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
    pub fn lm_loadout_offer(&self) -> Option<crate::lmstudio::LoadoutOffer> {
        self.adapter_progress
            .as_ref()
            .and_then(|p| p.loadout_offer())
    }
    pub fn approve_lm_replacement(&mut self, offer: &crate::lmstudio::LoadoutOffer) -> Result<()> {
        if !self.pending() || self.config.mode != "active" {
            bail!("LM Studio recovery is no longer pending");
        }
        self.adapter_progress
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("LM Studio recovery changed"))?
            .approve_replacement(offer)
    }
    /// Power events carry no approval for replacing a user loadout.
    pub fn resume_detected(&mut self) {
        if let Some(progress) = &mut self.adapter_progress {
            progress.revoke_replacement();
        }
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
            if let Some(progress) = &mut self.adapter_progress {
                progress.revoke_replacement();
            }
            self.set_activity(Activity::Recovery);
            self.message = "Game restarted; restoration deferred".into();
            return Ok(());
        }
        self.set_activity(Activity::Restoring);
        if !self.restore_units(cancelled, compare)? {
            if let Some(progress) = &mut self.adapter_progress {
                progress.revoke_replacement();
            }
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
}

fn bail_gameplay_refused() -> Result<()> {
    bail!("Gameplay Restore approval is no longer valid; recovery retained")
}

#[cfg(test)]
mod tests;
