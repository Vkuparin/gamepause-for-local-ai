//! Experimental coordinator adapter with typed recovery and bounded local HTTP.
use crate::{
    config::normalized_endpoint,
    coordinator::{Outcome, Planned, Runtime},
    discovery::Game,
    ollama_contract::{self as contract, Identity, ReplayCandidate, ResidentInventory},
    ollama_expiry::Policy,
    provider::{Guarantee, InferenceBusy, Kind},
    recovery::{Binding, Entry, Intent},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    collections::BTreeMap,
    io::Read,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub trait Transport {
    fn request(&mut self, path: &str, body: Option<serde_json::Value>) -> Result<Vec<u8>>;
}
/// Nothing accepted a connection on the endpoint: Ollama is not installed or
/// not running. Timeouts and HTTP errors from a live service are not this.
#[derive(Debug)]
pub struct Unreachable;
impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ollama is not running at its configured address")
    }
}
impl std::error::Error for Unreachable {}
pub const NOT_RUNNING: &str = "Not running; nothing to pause.";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// A preload answers only once the model is resident, and Ollama abandons a
/// load whose client disconnects, so it gets the same allowance as an LM
/// Studio load.
const LOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Only a generate request that keeps the model resident is a load; reads and
/// unloads (`keep_alive: 0`) are acknowledged at once.
fn is_load(path: &str, body: Option<&serde_json::Value>) -> bool {
    body.is_some_and(|body| path == "/api/generate" && body["keep_alive"] != 0)
}
pub struct Http {
    endpoint: String,
    agent: ureq::Agent,
    /// Limits for an ordinary request and for a model load.
    timeouts: (Duration, Duration),
}
impl Http {
    pub fn new(endpoint: &str) -> Result<Self> {
        Ok(Self {
            endpoint: normalized_endpoint(endpoint)?,
            agent: ureq::AgentBuilder::new()
                .try_proxy_from_env(false)
                .redirects(0)
                .timeout(REQUEST_TIMEOUT)
                .build(),
            timeouts: (REQUEST_TIMEOUT, LOAD_TIMEOUT),
        })
    }
}
impl Transport for Http {
    fn request(&mut self, path: &str, body: Option<serde_json::Value>) -> Result<Vec<u8>> {
        if !matches!(
            (path, body.is_some()),
            ("/api/version" | "/api/ps" | "/api/tags", false)
                | ("/api/show" | "/api/generate", true)
        ) {
            bail!("Unsupported Ollama request route");
        }
        let url = format!("http://{}{path}", self.endpoint);
        let timeout = if is_load(path, body.as_ref()) {
            self.timeouts.1
        } else {
            self.timeouts.0
        };
        let response = match body {
            Some(body) => self.agent.post(&url).timeout(timeout).send_json(body),
            None => self.agent.get(&url).timeout(timeout).call(),
        }
        .map_err(|error| match error {
            ureq::Error::Transport(transport)
                if transport.kind() == ureq::ErrorKind::ConnectionFailed =>
            {
                anyhow::Error::new(Unreachable)
            }
            _ => anyhow::anyhow!("Ollama local request failed"),
        })?;
        if response.status() != 200 {
            bail!("Ollama local request returned an unexpected status");
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(contract::MAX_RESPONSE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .context("Could not read Ollama local response")?;
        if bytes.len() > contract::MAX_RESPONSE_BYTES {
            bail!("Ollama local response exceeds limit");
        }
        Ok(bytes)
    }
}
pub trait Clock {
    fn wall(&self) -> SystemTime;
    fn monotonic(&self) -> Duration;
}
pub struct SystemClock(Instant);
impl Default for SystemClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}
impl Clock for SystemClock {
    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }
    fn monotonic(&self) -> Duration {
        self.0.elapsed()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Captured,
    Unloading,
    UnloadAcknowledged,
    Loading,
    LoadAcknowledged,
    ExpiryPending,
    Expired,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub original: ReplayCandidate,
    pub stage: Stage,
}
/// A resident local model outside the replay subset: unloaded with the rest,
/// never reloaded. Its stage only ever reaches `UnloadAcknowledged`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnloadOnly {
    pub identity: Identity,
    pub stage: Stage,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    pub source_revision: String,
    pub expiry_policy: String,
    pub models: Vec<Model>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unload_only: Vec<UnloadOnly>,
    /// The service did not answer at capture, so this session holds no
    /// Ollama obligation and sends it no request.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub absent: bool,
    pub pause_complete: bool,
}
impl Snapshot {
    pub fn begin(&mut self) {
        self.pause_complete = false;
        for model in &mut self.models {
            if !matches!(model.stage, Stage::Expired | Stage::ExpiryPending) {
                model.stage = Stage::Captured;
            }
        }
        for model in &mut self.unload_only {
            model.stage = Stage::Captured;
        }
    }
    /// Serial control units one pause can need; bounds the engine's drivers.
    pub fn units(&self) -> usize {
        self.models.len() + self.unload_only.len()
    }
    pub fn policy(&self) -> Result<Policy> {
        Policy::parse(&self.expiry_policy)
    }
    /// Names the models this session unloads without a restore obligation.
    pub fn note(&self) -> String {
        if self.absent {
            return NOT_RUNNING.into();
        }
        if self.unload_only.is_empty() {
            return String::new();
        }
        let mut names = self
            .unload_only
            .iter()
            .take(3)
            .map(|model| model.identity.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if self.unload_only.len() > 3 {
            names.push_str(&format!(" and {} more", self.unload_only.len() - 3));
        }
        format!("Unloaded without reload (outside the restore subset): {names}.")
    }
    pub fn validate_transition(&self, new: &Self) -> Result<()> {
        if self.version != new.version
            || self.source_revision != new.source_revision
            || self.expiry_policy != new.expiry_policy
            || self.absent != new.absent
            || self.models.len() != new.models.len()
            || self.unload_only.len() != new.unload_only.len()
            || self
                .unload_only
                .iter()
                .zip(&new.unload_only)
                .any(|(old, new)| old.identity != new.identity)
            || self.models.iter().zip(&new.models).any(|(old, new)| {
                old.original != new.original
                    || (old.stage == Stage::Expired && new.stage != Stage::Expired)
                    || (old.stage == Stage::ExpiryPending
                        && !matches!(new.stage, Stage::ExpiryPending | Stage::Expired))
            })
        {
            bail!("Ollama original recovery evidence changed");
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        let Ok(policy) = self.policy() else {
            bail!("Unsupported Ollama recovery contract");
        };
        if self.version != 1
            || self.source_revision != contract::SOURCE_REVISION
            || self.units() > 256
            || (self.absent && self.units() != 0)
        {
            bail!("Unsupported Ollama recovery contract");
        }
        let mut names = std::collections::BTreeSet::new();
        for model in &self.unload_only {
            contract::unload_request(&model.identity)?;
            if !names.insert(&model.identity.name)
                || !matches!(
                    model.stage,
                    Stage::Captured | Stage::Unloading | Stage::UnloadAcknowledged
                )
                || (self.pause_complete && model.stage != Stage::UnloadAcknowledged)
            {
                bail!("Inconsistent Ollama recovery progress");
            }
        }
        let mut captured_at = None;
        for model in &self.models {
            model.original.validate(policy)?;
            if !names.insert(&model.original.resident().identity.name)
                || captured_at.is_some_and(|time| time != model.original.captured_at())
                || (self.pause_complete
                    && !matches!(model.stage, Stage::UnloadAcknowledged | Stage::Expired))
            {
                bail!("Inconsistent Ollama recovery progress");
            }
            captured_at = Some(model.original.captured_at());
        }
        Ok(())
    }
    fn inventory(&self, active_only: bool) -> ResidentInventory {
        ResidentInventory(
            self.models
                .iter()
                .filter(|model| !active_only || model.stage != Stage::Expired)
                .map(|model| {
                    let resident = model.original.resident().clone();
                    (resident.identity.name.clone(), resident)
                })
                .collect(),
        )
    }
    /// Every digest a pause promised to free, replayable or not.
    fn unloaded(&self) -> ResidentInventory {
        let mut inventory = self.inventory(true);
        for model in &self.unload_only {
            inventory.0.insert(
                model.identity.name.clone(),
                contract::Resident {
                    identity: model.identity.clone(),
                    context_length: None,
                    expires_at: None,
                },
            );
        }
        inventory
    }
}
#[derive(Clone, Copy)]
enum Operation {
    Unload(usize),
    Release(usize),
    Load(usize),
    ResolveExpiry(usize),
    VerifyPause,
    VerifyRestore,
}
pub struct Work {
    snapshot: Snapshot,
    operation: Operation,
}
pub struct Adapter<T, C = SystemClock> {
    pub transport: T,
    pub clock: C,
    binding: Binding,
    started_wall: Duration,
    started_monotonic: Duration,
    retry_restore: Cell<bool>,
    /// New captures freeze the remaining keep-alive; saved journals keep theirs.
    capture_policy: Policy,
    /// Process-local acknowledgement times of frozen loads, by model name.
    loaded_at: BTreeMap<String, Duration>,
    /// Memory the latest capture found resident; presentation only.
    pub captured_bytes: u64,
}
impl<T: Transport, C: Clock> Adapter<T, C> {
    pub fn new(transport: T, clock: C, binding: Binding) -> Result<Self> {
        if binding.kind != Kind::Ollama
            || binding.guarantee != Guarantee::SupportedFields
            || binding.payload_version != 1
            || normalized_endpoint(&binding.endpoint)?
                != normalized_endpoint(&binding.configured_endpoint)?
        {
            bail!("Invalid Ollama adapter binding");
        }
        let started_wall = clock
            .wall()
            .duration_since(UNIX_EPOCH)
            .context("Invalid Ollama clock")?;
        let started_monotonic = clock.monotonic();
        Ok(Self {
            transport,
            clock,
            binding,
            started_wall,
            started_monotonic,
            retry_restore: Cell::new(false),
            capture_policy: Policy::FrozenRemaining,
            loaded_at: BTreeMap::new(),
            captured_bytes: 0,
        })
    }
    fn guard(&self, binding: &Binding) -> Result<()> {
        if *binding != self.binding {
            bail!("Ollama provider binding changed; recovery retained");
        }
        Ok(())
    }
    fn inventory(&mut self) -> Result<ResidentInventory> {
        contract::parse_resident_inventory(self.transport.request("/api/ps", None)?.as_slice())
    }
    fn elapsed(&self, original: &ReplayCandidate) -> Duration {
        self.started_wall
            .saturating_sub(original.captured_at())
            .saturating_add(
                self.clock
                    .monotonic()
                    .saturating_sub(self.started_monotonic),
            )
    }
    fn preload(
        &self,
        original: &ReplayCandidate,
        policy: Policy,
    ) -> Result<Option<serde_json::Value>> {
        original.preload_request(self.clock.wall(), Some(self.elapsed(original)), policy)
    }
    fn metadata(&mut self, name: &str) -> Result<(contract::Catalog, Vec<u8>)> {
        let catalog =
            contract::parse_catalog(self.transport.request("/api/tags", None)?.as_slice())?;
        let show = self.transport.request(
            "/api/show",
            Some(serde_json::json!({ "model": contract::local_reference(name)? })),
        )?;
        Ok((catalog, show))
    }
    fn expired_stage(&mut self, original: &ReplayCandidate) -> Result<Stage> {
        let observed = self.inventory()?;
        Ok(
            if observed
                .0
                .values()
                .any(|resident| resident.identity.digest == original.resident().identity.digest)
            {
                Stage::ExpiryPending
            } else {
                Stage::Expired
            },
        )
    }
    /// Recheck the pinned limited contract before each mutation. No tag substitution.
    fn revalidate(&mut self, original: &ReplayCandidate, policy: Policy) -> Result<()> {
        let (catalog, show) = self.metadata(&original.resident().identity.name)?;
        ReplayCandidate::capture(
            original.resident(),
            &catalog,
            show.as_slice(),
            UNIX_EPOCH
                .checked_add(original.captured_at())
                .context("Invalid persisted Ollama capture clock")?,
            policy,
        )?;
        Ok(())
    }
}
impl<T: Transport, C: Clock> Runtime for Adapter<T, C> {
    type Payload = Snapshot;
    type Work = Work;
    fn capture(&mut self, binding: &Binding, _: &[Game]) -> Result<Entry<Snapshot>> {
        self.guard(binding)?;
        let now = self.clock.wall();
        // Each new capture starts its own monotonic budget. Keeping the adapter
        // alive across ticks must not charge previous idle/session time to it.
        self.started_wall = now
            .duration_since(UNIX_EPOCH)
            .context("Invalid Ollama capture clock")?;
        self.started_monotonic = self.clock.monotonic();
        let policy = self.capture_policy;
        self.loaded_at.clear();
        self.captured_bytes = 0;
        let inventory = match self.transport.request("/api/ps", None).and_then(|body| {
            let inventory = contract::parse_resident_inventory(body.as_slice())?;
            self.captured_bytes = contract::resident_bytes(&body);
            Ok(inventory)
        }) {
            Ok(inventory) => inventory,
            // No service means nothing to pause, not a failed pause.
            Err(error) if error.downcast_ref::<Unreachable>().is_some() => {
                return Ok(Entry {
                    binding: binding.clone(),
                    payload: Snapshot {
                        version: 1,
                        source_revision: contract::SOURCE_REVISION.into(),
                        expiry_policy: policy.name().into(),
                        models: vec![],
                        unload_only: vec![],
                        absent: true,
                        pause_complete: false,
                    },
                    restore_complete: false,
                });
            }
            Err(error) => return Err(error),
        };
        let catalog =
            contract::parse_catalog(self.transport.request("/api/tags", None)?.as_slice())?;
        let mut models = Vec::new();
        let mut unload_only = Vec::new();
        for resident in inventory.0.values() {
            let show = self.transport.request(
                "/api/show",
                Some(serde_json::json!({
                    "model": contract::local_reference(&resident.identity.name)?,
                })),
            )?;
            match ReplayCandidate::capture(resident, &catalog, show.as_slice(), now, policy) {
                Ok(original) => models.push(Model {
                    original,
                    stage: Stage::Captured,
                }),
                // Local content outside the replay subset still frees memory.
                Err(_) => unload_only.push(UnloadOnly {
                    identity: contract::unload_only(resident, &catalog, show.as_slice())?,
                    stage: Stage::Captured,
                }),
            }
        }
        if self.inventory()? != inventory {
            bail!("Ollama residency changed during capture; no control started");
        }
        let snapshot = Snapshot {
            version: 1,
            source_revision: contract::SOURCE_REVISION.into(),
            expiry_policy: policy.name().into(),
            models,
            unload_only,
            absent: false,
            pause_complete: false,
        };
        snapshot.validate()?;
        Ok(Entry {
            binding: binding.clone(),
            payload: snapshot,
            restore_complete: false,
        })
    }
    fn validate(&self, entry: &Entry<Snapshot>, _: &[Game]) -> Result<()> {
        self.guard(&entry.binding)?;
        entry.payload.validate()?;
        if entry.restore_complete
            && (entry.payload.pause_complete
                || entry
                    .payload
                    .models
                    .iter()
                    .any(|model| !matches!(model.stage, Stage::LoadAcknowledged | Stage::Expired)))
        {
            bail!("Ollama completion conflicts with recovery progress");
        }
        Ok(())
    }
    fn validate_transition(&self, old: &Snapshot, new: &Snapshot) -> Result<()> {
        old.validate_transition(new)
    }
    fn begin(&self, payload: &mut Snapshot, _: Intent) -> Result<()> {
        payload.begin();
        Ok(())
    }
    fn complete(&self, payload: &Snapshot, intent: Intent) -> bool {
        intent == Intent::Pause && payload.pause_complete
    }
    fn retry(&self, _: &Binding) {
        self.retry_restore.set(true);
    }
    fn note(&self, payload: &Snapshot) -> String {
        payload.note()
    }
    fn plan(&mut self, entry: &Entry<Snapshot>, intent: Intent) -> Result<Planned<Snapshot, Work>> {
        self.validate(entry, &[])?;
        let mut snapshot = entry.payload.clone();
        if intent == Intent::Restore && self.retry_restore.replace(false) {
            for model in &mut snapshot.models {
                if !matches!(model.stage, Stage::Expired | Stage::ExpiryPending) {
                    model.stage = Stage::Captured;
                }
            }
        }
        let operation = match intent {
            _ if snapshot
                .models
                .iter()
                .any(|model| model.stage == Stage::ExpiryPending) =>
            {
                Operation::ResolveExpiry(
                    snapshot
                        .models
                        .iter()
                        .position(|model| model.stage == Stage::ExpiryPending)
                        .unwrap(),
                )
            }
            Intent::Pause => {
                if let Some(index) = snapshot.models.iter().position(|model| {
                    !matches!(model.stage, Stage::UnloadAcknowledged | Stage::Expired)
                }) {
                    snapshot.models[index].stage = Stage::Unloading;
                    Operation::Unload(index)
                } else if let Some(index) = snapshot
                    .unload_only
                    .iter()
                    .position(|model| model.stage != Stage::UnloadAcknowledged)
                {
                    snapshot.unload_only[index].stage = Stage::Unloading;
                    Operation::Release(index)
                } else {
                    Operation::VerifyPause
                }
            }
            Intent::Restore => {
                if let Some(index) = snapshot.models.iter().position(|model| {
                    !matches!(model.stage, Stage::LoadAcknowledged | Stage::Expired)
                }) {
                    snapshot.models[index].stage = Stage::Loading;
                    Operation::Load(index)
                } else {
                    Operation::VerifyRestore
                }
            }
            Intent::Reconcile => bail!("Ollama requires a reconciled direction"),
        };
        Ok(Planned {
            checkpoint: snapshot.clone(),
            work: Work {
                snapshot,
                operation,
            },
        })
    }
    fn execute(&mut self, binding: &Binding, work: Work) -> Result<Outcome<Snapshot>> {
        self.guard(binding)?;
        let mut snapshot = work.snapshot;
        snapshot.validate()?;
        let policy = snapshot.policy()?;
        let mut complete = false;
        match work.operation {
            Operation::ResolveExpiry(index) => {
                let observed = self.inventory()?;
                if observed.0.values().any(|resident| {
                    resident.identity.digest
                        == snapshot.models[index].original.resident().identity.digest
                }) {
                    bail!(
                        "Ollama load exceeded its expiry budget and remains resident; recovery retained"
                    );
                }
                snapshot.models[index].stage = Stage::Expired;
            }
            Operation::Unload(index) => {
                let model = &mut snapshot.models[index];
                self.revalidate(&model.original, policy)?;
                let response = self
                    .transport
                    .request("/api/generate", Some(model.original.unload_request()?))?;
                contract::verify_acknowledgement(
                    response.as_slice(),
                    &contract::local_reference(&model.original.resident().identity.name)?,
                    true,
                )?;
                // Only final inventory establishes unload completion.
                model.stage = Stage::UnloadAcknowledged;
            }
            Operation::Release(index) => {
                let model = &mut snapshot.unload_only[index];
                // The catalog entry must still be this local content.
                let (catalog, show) = self.metadata(&model.identity.name)?;
                let resident = contract::Resident {
                    identity: model.identity.clone(),
                    context_length: None,
                    expires_at: None,
                };
                contract::unload_only(&resident, &catalog, show.as_slice())?;
                let response = self.transport.request(
                    "/api/generate",
                    Some(contract::unload_request(&model.identity)?),
                )?;
                contract::verify_acknowledgement(
                    response.as_slice(),
                    &contract::local_reference(&model.identity.name)?,
                    true,
                )?;
                model.stage = Stage::UnloadAcknowledged;
            }
            Operation::Load(index) => {
                let model = &mut snapshot.models[index];
                let name = model.original.resident().identity.name.clone();
                self.loaded_at.remove(&name);
                if self.preload(&model.original, policy)?.is_none() {
                    model.stage = self.expired_stage(&model.original)?;
                } else {
                    self.revalidate(&model.original, policy)?;
                    // Read time again after bounded catalog/show work.
                    if let Some(body) = self.preload(&model.original, policy)? {
                        let response = self.transport.request("/api/generate", Some(body))?;
                        contract::verify_acknowledgement(
                            response.as_slice(),
                            &contract::local_reference(&model.original.resident().identity.name)?,
                            false,
                        )?;
                        model.stage = if self.preload(&model.original, policy)?.is_none() {
                            Stage::ExpiryPending
                        } else {
                            self.loaded_at.insert(name, self.clock.monotonic());
                            Stage::LoadAcknowledged
                        };
                    } else {
                        model.stage = self.expired_stage(&model.original)?;
                    }
                }
            }
            Operation::VerifyPause if snapshot.absent => snapshot.pause_complete = true,
            Operation::VerifyRestore if snapshot.absent => complete = true,
            Operation::VerifyPause => {
                let observed = self.inventory()?;
                if !contract::captured_models_absent(&snapshot.unloaded(), &observed) {
                    return Err(InferenceBusy.into());
                }
                if !observed.0.is_empty() {
                    bail!("Other Ollama content is resident; pause incomplete");
                }
                snapshot.pause_complete = true;
            }
            Operation::VerifyRestore => {
                for model in &mut snapshot.models {
                    if model.stage != Stage::Expired
                        && self.preload(&model.original, policy)?.is_none()
                    {
                        model.stage = Stage::ExpiryPending;
                    }
                }
                let observed = self.inventory()?;
                // A frozen residency shorter than the remaining restore work can
                // run out before this check. That is the replayed timer ending,
                // not an eviction to retry.
                let now = self.clock.monotonic();
                for model in &mut snapshot.models {
                    let resident = model.original.resident();
                    if model.stage == Stage::LoadAcknowledged
                        && !observed.0.contains_key(&resident.identity.name)
                        && model
                            .original
                            .frozen_residency(policy)
                            .zip(self.loaded_at.get(&resident.identity.name))
                            .is_some_and(|(kept, loaded)| now.saturating_sub(*loaded) >= kept)
                    {
                        model.stage = Stage::Expired;
                    }
                }
                for model in &mut snapshot.models {
                    if model.stage == Stage::ExpiryPending
                        && !observed.0.values().any(|resident| {
                            resident.identity.digest == model.original.resident().identity.digest
                        })
                    {
                        model.stage = Stage::Expired;
                    }
                }
                if snapshot
                    .models
                    .iter()
                    .any(|model| model.stage == Stage::ExpiryPending)
                {
                    return Ok(Outcome {
                        payload: snapshot,
                        restore_complete: false,
                    });
                }
                if !contract::compare_residency(&snapshot.inventory(true), &observed).is_empty() {
                    bail!(
                        "Ollama final identity/context/residency is unverified; recovery retained"
                    );
                }
                complete = true;
            }
        }
        Ok(Outcome {
            payload: snapshot,
            restore_complete: complete,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests;
