//! Serial, checkpointed provider scheduling. Game/grace/override policy stays
//! with the engine; adapters own payloads, replay safety and verification.
use crate::{
    config::normalized_endpoint,
    discovery::Game,
    provider::{Guarantee, InferenceBusy, Kind},
    recovery::{Binding, Entry, Intent, Journal, Session},
};
use anyhow::{Context, Result, bail};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

pub struct Planned<P, W> {
    /// Durable intent/stages, without replacing original captured settings.
    pub checkpoint: P,
    pub work: W,
}
pub struct Outcome<P> {
    pub payload: P,
    /// True only after final promised-field and original lifecycle verification.
    pub restore_complete: bool,
}
pub trait Runtime {
    type Payload: Clone;
    type Work;
    fn capture(&mut self, binding: &Binding, games: &[Game]) -> Result<Entry<Self::Payload>>;
    fn validate(&self, entry: &Entry<Self::Payload>, games: &[Game]) -> Result<()>;
    fn validate_transition(&self, original: &Self::Payload, updated: &Self::Payload) -> Result<()>;
    /// Only legacy payloads duplicate the shared remembered-game list.
    fn remember_games(&self, _payload: &mut Self::Payload, _games: &[Game]) {}
    /// Reset completion evidence when direction changes; preserve originals.
    fn begin(&self, payload: &mut Self::Payload, intent: Intent) -> Result<()>;
    fn complete(&self, payload: &Self::Payload, intent: Intent) -> bool;
    /// After a unit failure, adapters may finish independent healthy units.
    /// This never authorizes completion of the failed obligation.
    fn can_continue(&self, _entry: &Entry<Self::Payload>, _intent: Intent) -> bool {
        false
    }
    /// A provider retry starts a fresh attempt with new verification evidence.
    fn retry(&self, _binding: &Binding) {}
    /// Factual limits of a successful operation, such as models that were
    /// unloaded but are outside the restore guarantee. Never an error.
    fn note(&self, _payload: &Self::Payload) -> String {
        String::new()
    }
    /// Plans must be replayable from their checkpoint after a crash/write error.
    fn plan(
        &mut self,
        entry: &Entry<Self::Payload>,
        intent: Intent,
    ) -> Result<Planned<Self::Payload, Self::Work>>;
    /// A completed payload requires promised-field/lifecycle verification.
    fn execute(&mut self, binding: &Binding, work: Self::Work) -> Result<Outcome<Self::Payload>>;
}
pub trait Store<P> {
    fn save(&mut self, journal: Option<&Journal<P>>) -> Result<()>;
}
pub struct JournalFile(pub std::path::PathBuf);
impl Store<crate::recovery::Payload> for JournalFile {
    fn save(&mut self, journal: Option<&Journal>) -> Result<()> {
        crate::recovery::save(&self.0, journal)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Uncaptured,
    Pausing,
    Paused,
    Restoring,
    Restored,
    Deferred,
    Failed,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct Report {
    pub id: String,
    pub kind: Kind,
    pub guarantee: Guarantee,
    pub state: State,
    pub pending: bool,
    pub error: String,
    pub retry_seconds: Option<u64>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}
#[derive(Clone, Debug)]
pub struct Status {
    pub state: State,
    pub error: String,
    pub retry_at: Duration,
    /// Kept after the journal clears so a finished restore still reports it.
    pub note: String,
}
impl Default for Status {
    fn default() -> Self {
        Self {
            state: State::Uncaptured,
            error: String::new(),
            retry_at: Duration::ZERO,
            note: String::new(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Idle,
    Captured,
    Advanced,
    Interrupted,
    Completed,
}

pub struct Coordinator<R: Runtime, S: Store<R::Payload>> {
    pub runtime: R,
    pub store: S,
    journal: Option<Journal<R::Payload>>,
    configured: Vec<Binding>,
    status: BTreeMap<String, Status>,
    retry: Duration,
    cursor: usize,
    dirty: bool,
    reconciled: bool,
    continuing: BTreeSet<String>,
}
/// Process-local scheduling continuity. Durable authority remains the journal.
/// This is never serialized and cannot authorize gameplay after restart.
pub struct Continuation<P> {
    journal: Option<Journal<P>>,
    configured: Vec<Binding>,
    status: BTreeMap<String, Status>,
    cursor: usize,
    dirty: bool,
    reconciled: bool,
    continuing: BTreeSet<String>,
}
impl<P> Continuation<P> {
    pub fn journal(&self) -> Option<&Journal<P>> {
        self.journal.as_ref()
    }
    pub fn persistence_pending(&self) -> bool {
        self.dirty
    }
    pub fn next_retry_at(&self) -> Option<Duration> {
        self.status
            .values()
            .filter(|status| matches!(status.state, State::Failed | State::Deferred))
            .map(|status| status.retry_at)
            .min()
    }
    pub fn ready(&self, intent: Intent, now: Duration) -> bool {
        self.dirty
            || self
                .journal
                .as_ref()
                .is_some_and(|journal| journal.session.intent != intent)
            || !self.continuing.is_empty()
            || self.status.values().any(|status| match status.state {
                State::Failed | State::Deferred => now >= status.retry_at,
                State::Paused | State::Restored => false,
                _ => true,
            })
    }
    pub fn request_retry(&mut self) {
        for status in self.status.values_mut() {
            if matches!(status.state, State::Failed | State::Deferred) {
                status.retry_at = Duration::ZERO;
            }
        }
    }
}
impl<R: Runtime, S: Store<R::Payload>> Coordinator<R, S> {
    pub fn resume(
        runtime: R,
        store: S,
        configured: Vec<Binding>,
        memory: Continuation<R::Payload>,
        retry: Duration,
    ) -> Result<Self> {
        let same_configuration = configured == memory.configured;
        let mut coordinator = Self::new(runtime, store, configured, memory.journal, retry)?;
        if same_configuration {
            coordinator.status = memory.status;
            coordinator.cursor = memory.cursor;
            coordinator.reconciled = memory.reconciled;
            coordinator.continuing = memory.continuing;
        }
        // Failed persistence always remains a global gate, even after settings change.
        coordinator.dirty = memory.dirty;
        Ok(coordinator)
    }
    pub fn into_parts(self) -> (R, S, Continuation<R::Payload>) {
        let memory = Continuation {
            journal: self.journal,
            configured: self.configured,
            status: self.status,
            cursor: self.cursor,
            dirty: self.dirty,
            reconciled: self.reconciled,
            continuing: self.continuing,
        };
        (self.runtime, self.store, memory)
    }
    pub fn new(
        runtime: R,
        store: S,
        configured: Vec<Binding>,
        journal: Option<Journal<R::Payload>>,
        retry: Duration,
    ) -> Result<Self> {
        if retry.is_zero() {
            bail!("Provider retry interval must be positive");
        }
        validate_bindings(configured.iter())?;
        if let Some(journal) = &journal {
            if journal.schema != 3 || journal.providers.is_empty() {
                bail!("Invalid recovery envelope; recovery retained");
            }
            validate_bindings(journal.providers.iter().map(|entry| &entry.binding))?;
            validate_bindings(
                journal
                    .providers
                    .iter()
                    .filter(|entry| !entry.restore_complete)
                    .map(|entry| &entry.binding)
                    .chain(configured.iter().filter(|current| {
                        !journal
                            .providers
                            .iter()
                            .any(|entry| !entry.restore_complete && entry.binding.id == current.id)
                    })),
            )?;
            for entry in &journal.providers {
                if entry.restore_complete && journal.session.intent != Intent::Restore {
                    bail!("Provider completion conflicts with recovery intent; source retained");
                }
                runtime.validate(entry, &journal.session.games)?;
                if !entry.restore_complete
                    && let Some(current) = configured
                        .iter()
                        .find(|binding| binding.id == entry.binding.id)
                {
                    same_provider(current, &entry.binding)?;
                }
            }
        }
        let mut status = BTreeMap::new();
        for binding in &configured {
            status.insert(binding.id.clone(), Status::default());
        }
        if let Some(journal) = &journal {
            for entry in &journal.providers {
                status.entry(entry.binding.id.clone()).or_default();
            }
        }
        let reconciled = journal.is_none();
        Ok(Self {
            runtime,
            store,
            journal,
            configured,
            status,
            retry,
            cursor: 0,
            dirty: false,
            reconciled,
            continuing: BTreeSet::new(),
        })
    }
    pub fn journal(&self) -> Option<&Journal<R::Payload>> {
        self.journal.as_ref()
    }
    pub fn statuses(&self) -> &BTreeMap<String, Status> {
        &self.status
    }
    /// Factual operation evidence for native/CLI presentation, without probing.
    pub fn reports(&self, now: Duration) -> Vec<Report> {
        self.status
            .iter()
            .filter_map(|(id, status)| {
                let entry = self.journal.as_ref().and_then(|journal| {
                    journal
                        .providers
                        .iter()
                        .find(|entry| entry.binding.id == *id)
                });
                let binding = entry
                    .map(|entry| &entry.binding)
                    .or_else(|| self.configured.iter().find(|binding| binding.id == *id))?;
                let remaining = status.retry_at.saturating_sub(now);
                Some(Report {
                    id: id.clone(),
                    kind: binding.kind,
                    guarantee: binding.guarantee,
                    state: status.state,
                    pending: entry.is_some_and(|entry| !entry.restore_complete),
                    error: status.error.clone(),
                    retry_seconds: (!remaining.is_zero()).then(|| {
                        remaining
                            .as_secs()
                            .saturating_add(u64::from(remaining.subsec_nanos() > 0))
                    }),
                    note: entry.map_or_else(
                        || status.note.clone(),
                        |entry| self.runtime.note(&entry.payload),
                    ),
                })
            })
            .collect()
    }
    pub fn persistence_pending(&self) -> bool {
        self.dirty
    }
    pub fn pause_complete(&self) -> bool {
        self.reconciled
            && !self.configured.is_empty()
            && self.journal.as_ref().is_some_and(|journal| {
                journal.session.intent == Intent::Pause
                    && journal
                        .providers
                        .iter()
                        .all(|entry| self.runtime.complete(&entry.payload, Intent::Pause))
                    && self.configured.iter().all(|binding| {
                        journal
                            .providers
                            .iter()
                            .find(|entry| entry.binding.id == binding.id)
                            .is_some_and(|entry| {
                                self.runtime.complete(&entry.payload, Intent::Pause)
                            })
                    })
            })
            && !self.dirty
    }
    fn complete(&self, entry: &Entry<R::Payload>, intent: Intent) -> bool {
        if intent == Intent::Restore {
            entry.restore_complete
        } else {
            self.runtime.complete(&entry.payload, intent)
        }
    }
    fn persist(&mut self, journal: Journal<R::Payload>) -> Result<()> {
        self.journal = Some(journal);
        self.dirty = true;
        self.flush()
    }
    fn flush(&mut self) -> Result<()> {
        self.store
            .save(self.journal.as_ref())
            .context("Recovery persistence failed; provider control held")?;
        self.dirty = false;
        Ok(())
    }
    fn failed(&mut self, id: &str, error: anyhow::Error, now: Duration) {
        if self.continuing.contains(id)
            && let Some(status) = self.status.get_mut(id)
            && now < status.retry_at
        {
            status.error.push_str(&format!("; {error:#}"));
            self.continuing.remove(id);
            return;
        }
        self.continuing.remove(id);
        self.status.insert(
            id.into(),
            Status {
                state: if error.downcast_ref::<InferenceBusy>().is_some() {
                    State::Deferred
                } else {
                    State::Failed
                },
                error: format!("{error:#}"),
                retry_at: now.saturating_add(self.retry),
                note: String::new(),
            },
        );
    }
    /// One capture or one checkpointed provider unit per call. It never executes
    /// concurrent loads. A slow unit still needs worker isolation in the caller.
    pub fn advance(
        &mut self,
        intent: Intent,
        games: &[Game],
        now: Duration,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<Step> {
        if intent == Intent::Reconcile {
            bail!("Choose pause or restore after fresh detection");
        }
        // A failed write is a global gate, not a provider-local retry.
        if self.dirty {
            self.flush()?;
        }
        if cancelled() {
            return Ok(Step::Interrupted);
        }
        if let Some(original) = &self.journal {
            let mut changed = original.clone();
            let direction_changed = changed.session.intent != intent || !self.reconciled;
            let mut games_changed = false;
            for game in games {
                if !changed.session.games.iter().any(|old| {
                    crate::discovery::canonical(&old.path)
                        == crate::discovery::canonical(&game.path)
                }) {
                    changed.session.games.push(game.clone());
                    games_changed = true;
                }
            }
            if direction_changed {
                if intent == Intent::Pause {
                    // Verified restoration ends this provider's obligation. A
                    // disabled/removed/reassigned entry must not be repaused.
                    changed.providers.retain(|entry| {
                        !entry.restore_complete
                            || self
                                .configured
                                .iter()
                                .any(|binding| same_provider(binding, &entry.binding).is_ok())
                    });
                    if changed.providers.is_empty() {
                        if let Err(error) = self.store.save(None) {
                            self.dirty = true;
                            return Err(error)
                                .context("Recovery clear failed; verified obligations retained");
                        }
                        self.journal = None;
                        self.status.retain(|id, _| {
                            self.configured.iter().any(|binding| &binding.id == id)
                        });
                        self.reconciled = true;
                        return Ok(Step::Advanced);
                    }
                }
                changed.session.intent = intent;
                for entry in &mut changed.providers {
                    if intent == Intent::Restore && entry.restore_complete {
                        continue;
                    }
                    let original = entry.payload.clone();
                    self.runtime.begin(&mut entry.payload, intent)?;
                    entry.restore_complete = false;
                    self.runtime
                        .validate_transition(&original, &entry.payload)?;
                }
            }
            if direction_changed || games_changed {
                if games_changed {
                    for entry in &mut changed.providers {
                        let original = entry.payload.clone();
                        self.runtime
                            .remember_games(&mut entry.payload, &changed.session.games);
                        self.runtime
                            .validate_transition(&original, &entry.payload)?;
                    }
                }
                for entry in &changed.providers {
                    self.runtime.validate(entry, &changed.session.games)?;
                }
                if direction_changed {
                    self.continuing.clear();
                    self.status.retain(|id, _| {
                        self.configured.iter().any(|binding| &binding.id == id)
                            || changed
                                .providers
                                .iter()
                                .any(|entry| &entry.binding.id == id)
                    });
                    for status in self.status.values_mut() {
                        *status = Status::default();
                    }
                }
                self.reconciled = true;
                self.persist(changed)?;
            }
        }
        if intent == Intent::Restore {
            let Some(journal) = &self.journal else {
                return Ok(Step::Idle);
            };
            if journal
                .providers
                .iter()
                .all(|entry| self.complete(entry, intent))
            {
                if cancelled() {
                    return Ok(Step::Interrupted);
                }
                if let Err(error) = self.store.save(None) {
                    self.dirty = true;
                    return Err(error)
                        .context("Recovery clear failed; verified obligations retained");
                }
                for entry in &journal.providers {
                    self.status.insert(
                        entry.binding.id.clone(),
                        Status {
                            state: State::Restored,
                            note: self.runtime.note(&entry.payload),
                            ..Default::default()
                        },
                    );
                }
                self.journal = None;
                return Ok(Step::Completed);
            }
        }
        let mut candidates = self.configured.clone();
        if let Some(journal) = &self.journal {
            for entry in &journal.providers {
                if !candidates
                    .iter()
                    .any(|binding| binding.id == entry.binding.id)
                {
                    candidates.push(entry.binding.clone());
                }
            }
        }
        for offset in 0..candidates.len() {
            let index = (self.cursor + offset) % candidates.len();
            let binding = &candidates[index];
            let entry = self
                .journal
                .as_ref()
                .and_then(|journal| {
                    journal
                        .providers
                        .iter()
                        .find(|entry| entry.binding.id == binding.id)
                })
                .cloned();
            if entry
                .as_ref()
                .is_some_and(|entry| self.complete(entry, intent))
            {
                self.status.insert(
                    binding.id.clone(),
                    Status {
                        state: if intent == Intent::Pause {
                            State::Paused
                        } else {
                            State::Restored
                        },
                        ..Default::default()
                    },
                );
                continue;
            }
            if let Some(status) = self.status.get(&binding.id)
                && matches!(status.state, State::Failed | State::Deferred)
            {
                if now < status.retry_at {
                    if !self.continuing.contains(&binding.id)
                        || !entry
                            .as_ref()
                            .is_some_and(|entry| self.runtime.can_continue(entry, intent))
                    {
                        continue;
                    }
                } else {
                    self.runtime.retry(binding);
                    self.continuing.remove(&binding.id);
                    self.status.insert(binding.id.clone(), Status::default());
                }
            }
            if intent == Intent::Restore && entry.is_none() {
                continue;
            }
            self.cursor = (index + 1) % candidates.len();
            if !self
                .configured
                .iter()
                .any(|current| current.id == binding.id)
            {
                self.failed(&binding.id, anyhow::anyhow!("Provider is missing or disabled; restore its configuration. Recovery retained"), now);
                return Ok(Step::Advanced);
            }
            if cancelled() {
                return Ok(Step::Interrupted);
            }
            if let Some(entry) = entry {
                let planned = match self.runtime.plan(&entry, intent) {
                    Ok(planned) => planned,
                    Err(error) => {
                        self.failed(&binding.id, error, now);
                        return Ok(Step::Advanced);
                    }
                };
                let mut checkpoint = self.journal.clone().unwrap();
                let slot = checkpoint
                    .providers
                    .iter_mut()
                    .find(|entry| entry.binding.id == binding.id)
                    .unwrap();
                slot.payload = planned.checkpoint;
                let valid = self
                    .runtime
                    .validate(slot, &checkpoint.session.games)
                    .and_then(|_| {
                        self.runtime
                            .validate_transition(&entry.payload, &slot.payload)
                    })
                    .and_then(|_| {
                        if self.runtime.complete(&slot.payload, intent) {
                            bail!("Plan claims completion before verification");
                        }
                        Ok(())
                    });
                if let Err(error) = valid {
                    self.failed(&binding.id, error, now);
                    return Ok(Step::Advanced);
                }
                self.persist(checkpoint)?;
                if cancelled() {
                    return Ok(Step::Interrupted);
                }
                let actual_binding = self
                    .journal
                    .as_ref()
                    .unwrap()
                    .providers
                    .iter()
                    .find(|entry| entry.binding.id == binding.id)
                    .unwrap()
                    .binding
                    .clone();
                let outcome = match self.runtime.execute(&actual_binding, planned.work) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        self.failed(&binding.id, error, now);
                        if self.runtime.can_continue(&entry, intent) {
                            self.continuing.insert(binding.id.clone());
                        }
                        return Ok(Step::Advanced);
                    }
                };
                let mut finished = self.journal.clone().unwrap();
                let slot = finished
                    .providers
                    .iter_mut()
                    .find(|entry| entry.binding.id == binding.id)
                    .unwrap();
                slot.payload = outcome.payload;
                slot.restore_complete = intent == Intent::Restore && outcome.restore_complete;
                if let Err(error) = self
                    .runtime
                    .validate(slot, &finished.session.games)
                    .and_then(|_| {
                        self.runtime
                            .validate_transition(&entry.payload, &slot.payload)
                    })
                {
                    self.failed(&binding.id, error, now);
                    return Ok(Step::Advanced);
                }
                let complete = if intent == Intent::Restore {
                    slot.restore_complete
                } else {
                    self.runtime.complete(&slot.payload, intent)
                };
                self.persist(finished)?;
                if !self
                    .journal
                    .as_ref()
                    .unwrap()
                    .providers
                    .iter()
                    .find(|entry| entry.binding.id == binding.id)
                    .is_some_and(|entry| self.runtime.can_continue(entry, intent))
                {
                    self.continuing.remove(&binding.id);
                }
                if complete
                    || !self.status.get(&binding.id).is_some_and(|status| {
                        matches!(status.state, State::Failed | State::Deferred)
                    })
                {
                    self.status.insert(
                        binding.id.clone(),
                        Status {
                            state: match (intent, complete) {
                                (Intent::Pause, true) => State::Paused,
                                (Intent::Restore, true) => State::Restored,
                                (Intent::Pause, false) => State::Pausing,
                                _ => State::Restoring,
                            },
                            ..Default::default()
                        },
                    );
                }
                return Ok(if cancelled() {
                    Step::Interrupted
                } else {
                    Step::Advanced
                });
            }
            let remembered = self
                .journal
                .as_ref()
                .map(|journal| journal.session.games.clone())
                .unwrap_or_else(|| games.to_vec());
            match self.runtime.capture(binding, &remembered) {
                Ok(mut captured) => {
                    captured.restore_complete = false;
                    let original = captured.payload.clone();
                    let valid = same_provider(binding, &captured.binding)
                        .and_then(|_| self.runtime.begin(&mut captured.payload, intent))
                        .and_then(|_| {
                            self.runtime
                                .validate_transition(&original, &captured.payload)
                        })
                        .and_then(|_| self.runtime.validate(&captured, &remembered));
                    if let Err(error) = valid {
                        self.failed(&binding.id, error, now);
                        return Ok(Step::Advanced);
                    }
                    let mut journal = self.journal.clone().unwrap_or_else(|| Journal {
                        schema: 3,
                        session: Session {
                            games: games.to_vec(),
                            intent,
                        },
                        providers: Vec::new(),
                    });
                    self.runtime.validate(&captured, &journal.session.games)?;
                    journal.providers.push(captured);
                    let bindings = journal.providers.iter().map(|entry| &entry.binding).chain(
                        self.configured.iter().filter(|configured| {
                            !journal
                                .providers
                                .iter()
                                .any(|entry| entry.binding.id == configured.id)
                        }),
                    );
                    if let Err(error) = validate_bindings(bindings) {
                        self.failed(&binding.id, error, now);
                        return Ok(Step::Advanced);
                    }
                    self.persist(journal)?;
                    self.status.insert(
                        binding.id.clone(),
                        Status {
                            state: State::Pausing,
                            ..Default::default()
                        },
                    );
                    return Ok(if cancelled() {
                        Step::Interrupted
                    } else {
                        Step::Captured
                    });
                }
                Err(error) => {
                    self.failed(&binding.id, error, now);
                    return Ok(Step::Advanced);
                }
            }
        }
        Ok(Step::Idle)
    }
}
fn same_provider(configured: &Binding, captured: &Binding) -> Result<()> {
    if configured.id != captured.id
        || configured.kind != captured.kind
        || configured.payload_version != captured.payload_version
        || configured.guarantee != captured.guarantee
        || normalized_endpoint(&configured.configured_endpoint)?
            != normalized_endpoint(&captured.configured_endpoint)?
    {
        bail!("Provider identity/route/policy changed; recovery retained");
    }
    Ok(())
}
pub(crate) fn validate_bindings<'a>(bindings: impl Iterator<Item = &'a Binding>) -> Result<()> {
    let mut ids = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    let mut routes = BTreeMap::new();
    for binding in bindings {
        if binding.id.is_empty()
            || binding.id.len() > 64
            || !binding
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || binding.payload_version == 0
            || binding.guarantee == Guarantee::MonitorOnly
            || !ids.insert(&binding.id)
            || !kinds.insert(binding.kind as u8)
        {
            bail!("Unsupported or duplicate provider binding; recovery retained");
        }
        for endpoint in [&binding.configured_endpoint, &binding.endpoint] {
            let route = normalized_endpoint(endpoint)?;
            if routes
                .insert(route, &binding.id)
                .is_some_and(|owner| owner != &binding.id)
            {
                bail!("Duplicate provider endpoint ownership; recovery retained");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Kind;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
    enum Phase {
        Capture,
        Unload,
        VerifyPause,
        Restore,
        VerifyRestore,
    }
    #[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
    enum Stage {
        Planned,
        Unloading,
        Unloaded,
        Restoring,
        Restored,
    }
    #[derive(Clone, Debug, serde::Serialize)]
    struct Progress {
        stages: [Stage; 2],
        paused: bool,
        restored: bool,
    }
    #[derive(Clone, Debug, serde::Serialize)]
    enum Payload {
        LM {
            original_raw: String,
            progress: Progress,
        },
        Other {
            original_digest: String,
            progress: Progress,
        },
    }
    impl Payload {
        fn progress(&self) -> &Progress {
            match self {
                Self::LM { progress, .. } | Self::Other { progress, .. } => progress,
            }
        }
        fn progress_mut(&mut self) -> &mut Progress {
            match self {
                Self::LM { progress, .. } | Self::Other { progress, .. } => progress,
            }
        }
        fn original(&self) -> &str {
            match self {
                Self::LM { original_raw, .. } => original_raw,
                Self::Other {
                    original_digest, ..
                } => original_digest,
            }
        }
    }
    #[derive(Clone)]
    struct Work {
        payload: Payload,
        phase: Phase,
        index: Option<usize>,
    }
    #[derive(Default)]
    struct Disk {
        saved: Option<Journal<Payload>>,
        writes: usize,
        fail_at: Option<usize>,
        fail_clear: bool,
    }
    #[derive(Clone)]
    struct Storage(Arc<Mutex<Disk>>);
    impl Store<Payload> for Storage {
        fn save(&mut self, journal: Option<&Journal<Payload>>) -> Result<()> {
            let mut disk = self.0.lock().unwrap();
            disk.writes += 1;
            if disk.fail_at == Some(disk.writes) || (journal.is_none() && disk.fail_clear) {
                bail!("injected journal failure");
            }
            disk.saved = journal.cloned();
            Ok(())
        }
    }
    #[derive(Clone)]
    struct Fake {
        disk: Arc<Mutex<Disk>>,
        current: BTreeMap<String, [bool; 2]>,
        failures: BTreeSet<(String, Phase)>,
        after_change: bool,
        busy: bool,
        offline: BTreeSet<String>,
        events: Vec<(String, Phase)>,
        game: Arc<AtomicBool>,
        game_after_load: bool,
        corrupt: Option<&'static str>,
    }
    impl Runtime for Fake {
        type Payload = Payload;
        type Work = Work;
        fn capture(&mut self, binding: &Binding, _: &[Game]) -> Result<Entry<Payload>> {
            self.events.push((binding.id.clone(), Phase::Capture));
            if self.offline.contains(&binding.id)
                || self
                    .failures
                    .contains(&(binding.id.clone(), Phase::Capture))
            {
                if self.busy {
                    return Err(InferenceBusy.into());
                }
                bail!("capture unavailable");
            }
            let progress = Progress {
                stages: [Stage::Planned; 2],
                paused: false,
                restored: false,
            };
            let payload = match binding.kind {
                Kind::LMStudio => Payload::LM {
                    original_raw: "fixture original raw config".into(),
                    progress,
                },
                Kind::Ollama => Payload::Other {
                    original_digest: "fixture original content digest".into(),
                    progress,
                },
            };
            Ok(Entry {
                binding: binding.clone(),
                payload,
                restore_complete: false,
            })
        }
        fn validate(&self, entry: &Entry<Payload>, _: &[Game]) -> Result<()> {
            if !matches!(
                (entry.binding.kind, &entry.payload),
                (Kind::LMStudio, Payload::LM { .. }) | (Kind::Ollama, Payload::Other { .. })
            ) {
                bail!("payload kind mismatch");
            }
            Ok(())
        }
        fn validate_transition(&self, original: &Payload, updated: &Payload) -> Result<()> {
            if original.original() != updated.original()
                || std::mem::discriminant(original) != std::mem::discriminant(updated)
            {
                bail!("original capture changed");
            }
            Ok(())
        }
        fn begin(&self, payload: &mut Payload, _: Intent) -> Result<()> {
            payload.progress_mut().paused = false;
            payload.progress_mut().restored = false;
            Ok(())
        }
        fn complete(&self, payload: &Payload, intent: Intent) -> bool {
            if intent == Intent::Pause {
                payload.progress().paused
            } else {
                payload.progress().restored
            }
        }
        fn plan(
            &mut self,
            entry: &Entry<Payload>,
            intent: Intent,
        ) -> Result<Planned<Payload, Work>> {
            if self.offline.contains(&entry.binding.id) {
                bail!("provider disappeared");
            }
            let mut payload = entry.payload.clone();
            let done = if intent == Intent::Pause {
                Stage::Unloaded
            } else {
                Stage::Restored
            };
            let index = payload
                .progress()
                .stages
                .iter()
                .position(|stage| *stage != done);
            let phase = match (intent, index) {
                (Intent::Pause, Some(_)) => Phase::Unload,
                (Intent::Pause, None) => Phase::VerifyPause,
                (_, Some(_)) => Phase::Restore,
                (_, None) => Phase::VerifyRestore,
            };
            if let Some(index) = index {
                payload.progress_mut().stages[index] = if intent == Intent::Pause {
                    Stage::Unloading
                } else {
                    Stage::Restoring
                };
            }
            if entry.binding.id == "lm" {
                if self.corrupt == Some("plan_original") {
                    if let Payload::LM { original_raw, .. } = &mut payload {
                        *original_raw = "changed original".into();
                    }
                } else if self.corrupt == Some("plan_completion") {
                    payload.progress_mut().paused = true;
                }
            }
            Ok(Planned {
                checkpoint: payload.clone(),
                work: Work {
                    payload,
                    phase,
                    index,
                },
            })
        }
        fn execute(&mut self, binding: &Binding, mut work: Work) -> Result<Outcome<Payload>> {
            let disk = self.disk.lock().unwrap();
            let saved = disk.saved.as_ref().unwrap();
            assert_eq!(
                saved.session.intent,
                if matches!(work.phase, Phase::Unload | Phase::VerifyPause) {
                    Intent::Pause
                } else {
                    Intent::Restore
                }
            );
            let durable = &saved
                .providers
                .iter()
                .find(|entry| entry.binding.id == binding.id)
                .unwrap()
                .payload;
            assert_eq!(
                serde_json::to_value(durable).unwrap(),
                serde_json::to_value(&work.payload).unwrap()
            );
            drop(disk);
            self.events.push((binding.id.clone(), work.phase));
            let failing = self.failures.contains(&(binding.id.clone(), work.phase));
            if failing && !self.after_change {
                bail!("injected provider failure");
            }
            if let Some(index) = work.index {
                let restored = work.phase == Phase::Restore;
                self.current.get_mut(&binding.id).unwrap()[index] = restored;
                work.payload.progress_mut().stages[index] = if restored {
                    Stage::Restored
                } else {
                    Stage::Unloaded
                };
                if restored && self.game_after_load {
                    self.game.store(true, Ordering::SeqCst);
                }
            } else {
                let current = self.current.get(&binding.id).unwrap();
                let restored = work.phase == Phase::VerifyRestore;
                if !current.iter().all(|resident| *resident == restored) {
                    bail!("final residency unverified");
                }
                if restored {
                    work.payload.progress_mut().restored = true;
                } else {
                    work.payload.progress_mut().paused = true;
                }
            }
            if failing {
                bail!("partial operation failure");
            }
            if binding.id == "lm"
                && self.corrupt == Some("execute_original")
                && let Payload::LM { original_raw, .. } = &mut work.payload
            {
                *original_raw = "changed original".into();
            }
            Ok(Outcome {
                restore_complete: work.phase == Phase::VerifyRestore,
                payload: work.payload,
            })
        }
    }
    type Harness = Coordinator<Fake, Storage>;
    fn bindings() -> Vec<Binding> {
        [
            (
                "lm",
                Kind::LMStudio,
                "127.0.0.1:1234",
                Guarantee::CapturedConfiguration,
            ),
            (
                "other",
                Kind::Ollama,
                "127.0.0.1:11434",
                Guarantee::SupportedFields,
            ),
        ]
        .into_iter()
        .map(|(id, kind, endpoint, guarantee)| Binding {
            id: id.into(),
            kind,
            endpoint: endpoint.into(),
            configured_endpoint: endpoint.into(),
            payload_version: 1,
            guarantee,
        })
        .collect()
    }
    fn harness() -> Harness {
        let disk = Arc::new(Mutex::new(Disk::default()));
        let fake = Fake {
            disk: disk.clone(),
            current: [("lm".into(), [true; 2]), ("other".into(), [true; 2])].into(),
            failures: BTreeSet::new(),
            after_change: false,
            busy: false,
            offline: BTreeSet::new(),
            events: Vec::new(),
            game: Arc::new(AtomicBool::new(false)),
            game_after_load: false,
            corrupt: None,
        };
        Coordinator::new(
            fake,
            Storage(disk),
            bindings(),
            None,
            Duration::from_secs(10),
        )
        .unwrap()
    }
    fn pump(c: &mut Harness, intent: Intent, now: u64) {
        for _ in 0..20 {
            c.advance(intent, &[], Duration::from_secs(now), &mut || false)
                .unwrap();
        }
    }
    #[test]
    fn completed_disabled_provider_survives_restart_and_retires_before_repause_control() {
        for (retired, pending) in [("lm", "other"), ("other", "lm")] {
            let mut c = harness();
            pump(&mut c, Intent::Pause, 0);
            c.runtime.offline.insert(pending.into());
            pump(&mut c, Intent::Restore, 1);
            assert!(
                c.journal()
                    .unwrap()
                    .providers
                    .iter()
                    .any(|entry| entry.binding.id == retired && entry.restore_complete)
            );
            let original_pending = c
                .journal()
                .unwrap()
                .providers
                .iter()
                .find(|entry| entry.binding.id == pending)
                .unwrap()
                .payload
                .original()
                .to_owned();
            let retired_calls = c
                .runtime
                .events
                .iter()
                .filter(|(id, _)| id == retired)
                .count();
            let configured = bindings()
                .into_iter()
                .filter(|binding| binding.id == pending)
                .collect();
            let saved = c.store.0.lock().unwrap().saved.clone();
            c.runtime.offline.insert(retired.into());
            let mut c = Coordinator::new(
                c.runtime,
                c.store,
                configured,
                saved,
                Duration::from_secs(10),
            )
            .unwrap();
            pump(&mut c, Intent::Restore, 2);
            assert!(
                c.journal()
                    .unwrap()
                    .providers
                    .iter()
                    .any(|entry| entry.binding.id == retired && entry.restore_complete)
            );
            assert_eq!(
                c.runtime
                    .events
                    .iter()
                    .filter(|(id, _)| id == retired)
                    .count(),
                retired_calls
            );
            let events = c.runtime.events.clone();
            {
                let mut disk = c.store.0.lock().unwrap();
                disk.fail_at = Some(disk.writes + 1);
            }
            assert!(
                c.advance(Intent::Pause, &[], Duration::from_secs(3), &mut || false)
                    .is_err()
            );
            assert_eq!(
                c.runtime.events, events,
                "retirement must persist before more control"
            );
            assert_eq!(
                c.store
                    .0
                    .lock()
                    .unwrap()
                    .saved
                    .as_ref()
                    .unwrap()
                    .providers
                    .len(),
                2
            );
            c.store.0.lock().unwrap().fail_at = None;
            c.runtime.offline.remove(pending);
            pump(&mut c, Intent::Pause, 3);
            assert!(c.pause_complete());
            let journal = c.store.0.lock().unwrap().saved.clone().unwrap();
            assert_eq!(journal.providers.len(), 1);
            assert_eq!(journal.providers[0].payload.original(), original_pending);
            assert_eq!(
                c.runtime
                    .events
                    .iter()
                    .filter(|(id, _)| id == retired)
                    .count(),
                retired_calls
            );
            assert_eq!(c.runtime.current[retired], [true; 2]);
            pump(&mut c, Intent::Restore, 4);
            assert!(c.journal().is_none());
        }
    }
    #[test]
    fn two_distinct_payloads_restore_serially_and_completed_pause_is_quiet() {
        let mut c = harness();
        pump(&mut c, Intent::Pause, 0);
        assert!(c.pause_complete());
        let writes = c.store.0.lock().unwrap().writes;
        let events = c.runtime.events.clone();
        pump(&mut c, Intent::Pause, 1);
        assert_eq!(c.runtime.events, events);
        assert_eq!(c.store.0.lock().unwrap().writes, writes);
        let original: Vec<_> = c
            .journal()
            .unwrap()
            .providers
            .iter()
            .map(|entry| entry.payload.original().to_owned())
            .collect();
        pump(&mut c, Intent::Restore, 1);
        assert!(c.journal().is_none());
        assert!(
            c.runtime
                .current
                .values()
                .all(|models| models == &[true; 2])
        );
        assert_eq!(
            original,
            vec![
                "fixture original raw config",
                "fixture original content digest"
            ]
        );
        assert!(
            c.statuses()
                .values()
                .all(|status| status.state == State::Restored)
        );
    }
    #[test]
    fn provider_failure_matrix_preserves_healthy_work_and_retries_independently() {
        for target in ["lm", "other"] {
            for phase in [
                Phase::Capture,
                Phase::Unload,
                Phase::VerifyPause,
                Phase::Restore,
                Phase::VerifyRestore,
            ] {
                for after_change in [false, true] {
                    let mut c = harness();
                    c.runtime.after_change = after_change;
                    let restoring = matches!(phase, Phase::Restore | Phase::VerifyRestore);
                    if restoring {
                        pump(&mut c, Intent::Pause, 0);
                    }
                    c.runtime.failures.insert((target.into(), phase));
                    let intent = if restoring {
                        Intent::Restore
                    } else {
                        Intent::Pause
                    };
                    pump(&mut c, intent, 0);
                    assert_eq!(c.statuses()[target].state, State::Failed);
                    let healthy = if target == "lm" { "other" } else { "lm" };
                    assert_eq!(
                        c.statuses()[healthy].state,
                        if restoring {
                            State::Restored
                        } else {
                            State::Paused
                        }
                    );
                    assert!(c.journal().is_some());
                    assert!(!c.pause_complete());
                    let events = c.runtime.events.len();
                    pump(&mut c, intent, 9);
                    assert_eq!(c.runtime.events.len(), events);
                    c.runtime.failures.clear();
                    pump(&mut c, intent, 10);
                    if restoring {
                        assert!(c.journal().is_none());
                    } else {
                        assert!(c.pause_complete());
                        pump(&mut c, Intent::Restore, 10);
                    }
                    assert!(
                        c.runtime
                            .current
                            .values()
                            .all(|models| models == &[true; 2])
                    );
                }
            }
        }
    }
    #[test]
    fn each_checkpoint_failure_globally_holds_mutation_and_restart_recovers() {
        for fail_at in 1..=28 {
            let mut c = harness();
            c.store.0.lock().unwrap().fail_at = Some(fail_at);
            let mut failed = false;
            for intent in [Intent::Pause, Intent::Restore] {
                for _ in 0..20 {
                    if c.advance(intent, &[], Duration::ZERO, &mut || false)
                        .is_err()
                    {
                        failed = true;
                        break;
                    }
                }
                if failed {
                    break;
                }
            }
            assert!(failed, "write {fail_at}");
            let events = c.runtime.events.len();
            // Keep the same persistence failure active; no provider may proceed.
            {
                let mut disk = c.store.0.lock().unwrap();
                disk.fail_at = Some(disk.writes + 1);
                disk.fail_clear = true;
            }
            assert!(
                c.advance(Intent::Restore, &[], Duration::ZERO, &mut || false)
                    .is_err()
            );
            assert_eq!(c.runtime.events.len(), events);
            let disk = c.store.0.clone();
            let saved = disk.lock().unwrap().saved.clone();
            {
                let mut disk = disk.lock().unwrap();
                disk.fail_at = None;
                disk.fail_clear = false;
            }
            let mut resumed = Coordinator::new(
                c.runtime.clone(),
                Storage(disk),
                bindings(),
                saved,
                Duration::from_secs(10),
            )
            .unwrap();
            pump(&mut resumed, Intent::Restore, 0);
            assert!(resumed.journal().is_none());
            assert!(
                resumed
                    .runtime
                    .current
                    .values()
                    .all(|models| models == &[true; 2]),
                "write {fail_at}"
            );
        }
    }
    #[test]
    fn new_game_between_loads_retains_originals_and_repause_recovers_partial_restore() {
        let mut c = harness();
        pump(&mut c, Intent::Pause, 0);
        let originals: Vec<_> = c
            .journal()
            .unwrap()
            .providers
            .iter()
            .map(|entry| entry.payload.original().to_owned())
            .collect();
        c.runtime.game_after_load = true;
        let game = c.runtime.game.clone();
        assert_eq!(
            c.advance(Intent::Restore, &[], Duration::ZERO, &mut || game
                .load(Ordering::SeqCst))
                .unwrap(),
            Step::Interrupted
        );
        assert_eq!(
            c.runtime
                .events
                .iter()
                .filter(|(_, phase)| *phase == Phase::Restore)
                .count(),
            1
        );
        assert!(c.journal().is_some());
        c.runtime.game_after_load = false;
        pump(&mut c, Intent::Pause, 0);
        assert!(c.pause_complete());
        assert!(
            c.runtime
                .current
                .values()
                .all(|models| models == &[false; 2])
        );
        assert_eq!(
            c.journal()
                .unwrap()
                .providers
                .iter()
                .map(|entry| entry.payload.original().to_owned())
                .collect::<Vec<_>>(),
            originals
        );
        game.store(false, Ordering::SeqCst);
        pump(&mut c, Intent::Restore, 0);
        assert!(c.journal().is_none());
    }
    #[test]
    fn missing_provider_and_busy_capture_do_not_block_healthy_recovery() {
        let mut c = harness();
        c.runtime.failures.insert(("other".into(), Phase::Capture));
        c.runtime.busy = true;
        pump(&mut c, Intent::Pause, 0);
        assert_eq!(c.statuses()["other"].state, State::Deferred);
        assert_eq!(c.statuses()["lm"].state, State::Paused);
        c.runtime.failures.clear();
        pump(&mut c, Intent::Pause, 10);
        let saved = c.journal().cloned();
        let mut resumed = Coordinator::new(
            c.runtime.clone(),
            c.store.clone(),
            vec![bindings()[0].clone()],
            saved,
            Duration::from_secs(10),
        )
        .unwrap();
        pump(&mut resumed, Intent::Restore, 0);
        assert_eq!(resumed.statuses()["lm"].state, State::Restored);
        assert_eq!(resumed.statuses()["other"].state, State::Failed);
        assert!(resumed.journal().is_some());
        let saved = resumed.journal().cloned();
        let mut restored = Coordinator::new(
            resumed.runtime,
            resumed.store,
            bindings(),
            saved,
            Duration::from_secs(10),
        )
        .unwrap();
        pump(&mut restored, Intent::Restore, 0);
        assert!(restored.journal().is_none());
    }
    #[test]
    fn restart_reconciles_completed_pause_and_retains_all_remembered_games() {
        let mut c = harness();
        let first = Game::new("Custom", "fixture", "First", r"D:\Fixture Games\first.exe");
        let second = Game::new(
            "Custom",
            "fixture",
            "Second",
            r"D:\Fixture Games\second.exe",
        );
        c.advance(
            Intent::Pause,
            std::slice::from_ref(&first),
            Duration::ZERO,
            &mut || false,
        )
        .unwrap();
        c.advance(
            Intent::Pause,
            std::slice::from_ref(&second),
            Duration::ZERO,
            &mut || false,
        )
        .unwrap();
        pump(&mut c, Intent::Pause, 0);
        assert_eq!(c.journal().unwrap().session.games.len(), 2);
        let saved = c.journal().cloned();
        c.runtime.offline.insert("other".into());
        let mut resumed = Coordinator::new(
            c.runtime,
            c.store,
            bindings(),
            saved,
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(!resumed.pause_complete());
        pump(&mut resumed, Intent::Pause, 0);
        assert_eq!(resumed.statuses()["lm"].state, State::Paused);
        assert_eq!(resumed.statuses()["other"].state, State::Failed);
        assert!(!resumed.pause_complete());
        let games = &resumed.journal().unwrap().session.games;
        assert_eq!(
            games.iter().map(|game| &game.path).collect::<Vec<_>>(),
            vec![&first.path, &second.path]
        );
        resumed.runtime.offline.clear();
        pump(&mut resumed, Intent::Restore, 0);
        assert!(resumed.journal().is_none());
    }
    #[test]
    fn adapter_cannot_replace_originals_or_claim_completion_before_control() {
        for corruption in ["plan_original", "plan_completion", "execute_original"] {
            let mut c = harness();
            c.runtime.corrupt = Some(corruption);
            pump(&mut c, Intent::Pause, 0);
            assert_eq!(c.statuses()["lm"].state, State::Failed);
            assert_eq!(c.statuses()["other"].state, State::Paused);
            assert!(!c.pause_complete());
            let entry = c
                .journal()
                .unwrap()
                .providers
                .iter()
                .find(|entry| entry.binding.id == "lm")
                .unwrap();
            assert_eq!(entry.payload.original(), "fixture original raw config");
            assert_eq!(
                c.runtime
                    .events
                    .iter()
                    .filter(|(id, phase)| id == "lm" && *phase == Phase::Unload)
                    .count(),
                usize::from(corruption == "execute_original")
            );
            c.runtime.corrupt = None;
            pump(&mut c, Intent::Pause, 10);
            assert!(c.pause_complete());
            pump(&mut c, Intent::Restore, 10);
            assert!(c.journal().is_none());
        }
    }
    #[test]
    fn duplicate_routes_and_reassigned_pending_binding_are_refused_without_writes() {
        let mut c = harness();
        let mut duplicate = bindings();
        duplicate[1].configured_endpoint = "localhost:01234".into();
        assert!(
            Coordinator::new(
                c.runtime.clone(),
                c.store.clone(),
                duplicate,
                None,
                Duration::from_secs(10)
            )
            .is_err()
        );
        assert_eq!(c.store.0.lock().unwrap().writes, 0);
        pump(&mut c, Intent::Pause, 0);
        let writes = c.store.0.lock().unwrap().writes;
        let mut changed = bindings();
        changed[0].configured_endpoint = "localhost:4321".into();
        assert!(
            Coordinator::new(
                c.runtime,
                c.store.clone(),
                changed,
                c.journal.clone(),
                Duration::from_secs(10)
            )
            .is_err()
        );
        assert_eq!(c.store.0.lock().unwrap().writes, writes);
    }
}
