//! Serial, checkpointed provider scheduling. Game/grace/override policy stays
//! with the engine; adapters own payloads, replay safety and verification.
use crate::{
    config::normalized_endpoint,
    discovery::Game,
    provider::{Guarantee, InferenceBusy, Kind, ManualRetryRequired},
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
    pub manual_retry: bool,
    /// Kept after the journal clears so a finished restore still reports it.
    pub note: String,
}
impl Default for Status {
    fn default() -> Self {
        Self {
            state: State::Uncaptured,
            error: String::new(),
            retry_at: Duration::ZERO,
            manual_retry: false,
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
            .filter(|status| {
                matches!(status.state, State::Failed | State::Deferred) && !status.manual_retry
            })
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
                State::Failed | State::Deferred => !status.manual_retry && now >= status.retry_at,
                State::Paused | State::Restored => false,
                _ => true,
            })
    }
    pub fn request_retry(&mut self) {
        for status in self.status.values_mut() {
            if matches!(status.state, State::Failed | State::Deferred) {
                status.retry_at = Duration::ZERO;
                status.manual_retry = false;
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
                    retry_seconds: (!status.manual_retry && !remaining.is_zero()).then(|| {
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
            && (status.manual_retry || now < status.retry_at)
        {
            status.manual_retry |= error.downcast_ref::<ManualRetryRequired>().is_some();
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
                manual_retry: error.downcast_ref::<ManualRetryRequired>().is_some(),
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
                if status.manual_retry || now < status.retry_at {
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
        || if configured.kind == Kind::Process {
            configured.configured_endpoint != captured.configured_endpoint
        } else {
            normalized_endpoint(&configured.configured_endpoint)?
                != normalized_endpoint(&captured.configured_endpoint)?
        }
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
        if binding.kind == Kind::Process {
            // Local processes have one fixed route identity instead of a port.
            if binding.endpoint != crate::process_session::ROUTE
                || binding.configured_endpoint != crate::process_session::ROUTE
            {
                bail!("Unsupported process provider route; recovery retained");
            }
            continue;
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
mod tests;
