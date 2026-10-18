//! One native runtime hold and serial model replay, through durable units.
use crate::{
    coordinator::{Outcome, Planned, Runtime},
    discovery::Game,
    hermes::{Backend, Original},
    provider::{Guarantee, Kind},
    recovery::{Binding, Entry, Intent},
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Captured,
    GatewayStopping,
    GatewayStopped,
    GatewayStarting,
    GatewayStarted,
    Stopping,
    Stopped,
    Starting,
    Started,
    Restored,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    pub original: Original,
    pub stage: Stage,
    pub loaded: Vec<String>,
    pub pause_complete: bool,
}
impl Snapshot {
    pub fn units(&self) -> usize {
        self.original
            .models
            .len()
            .saturating_add(if self.original.gateway.is_some() {
                4
            } else {
                2
            })
    }
    pub fn held(&self) -> bool {
        self.original.running
    }
    pub fn begin(&mut self) {
        self.pause_complete = false;
        self.stage = Stage::Captured;
        self.loaded.clear();
    }
    pub fn validate(&self) -> Result<()> {
        self.original.validate()?;
        let mut ids = std::collections::BTreeSet::new();
        if !matches!(self.version, 1 | 2)
            || (self.version == 1 && self.original.gateway.is_some())
            || (self.original.gateway.is_none()
                && matches!(
                    self.stage,
                    Stage::GatewayStopping
                        | Stage::GatewayStopped
                        | Stage::GatewayStarting
                        | Stage::GatewayStarted
                ))
            || (self.pause_complete && self.stage != Stage::Stopped)
            || self
                .loaded
                .iter()
                .any(|id| !ids.insert(id) || !self.original.models.iter().any(|m| m.id == *id))
            || (self.stage == Stage::Restored
                && self.original.running
                && self.loaded.len() != self.original.models.len())
        {
            bail!("Invalid Hermes recovery progress; source retained");
        }
        Ok(())
    }
    pub fn validate_transition(&self, updated: &Self) -> Result<()> {
        updated.validate()?;
        if self.version != updated.version || self.original != updated.original {
            bail!("Original Hermes recovery evidence changed");
        }
        Ok(())
    }
    pub fn note(&self) -> String {
        if !self.original.running {
            "Not running; nothing to pause.".into()
        } else if self.original.gateway.is_some() {
            "Closes the runtime-owning Hermes gateway while paused, then reopens it and restores the captured models.".into()
        } else {
            "Restores the managed runtime, captured model content and supported live fields; conversations and idle timing are not preserved.".into()
        }
    }
}
enum Operation {
    StopGateway,
    StartGateway,
    Stop,
    VerifyPause,
    Start,
    Load(usize),
    VerifyRestore,
}
pub struct Work {
    binding: Binding,
    snapshot: Snapshot,
    operation: Operation,
}
pub struct Adapter<B> {
    pub backend: B,
    binding: Binding,
    retry: std::cell::Cell<bool>,
}
pub fn binding(id: &str) -> Binding {
    Binding {
        id: id.into(),
        kind: Kind::Hermes,
        endpoint: crate::hermes::ROUTE.into(),
        configured_endpoint: crate::hermes::ROUTE.into(),
        payload_version: 1,
        guarantee: Guarantee::SupportedFields,
    }
}
impl<B: Backend> Adapter<B> {
    pub fn new(backend: B, binding: Binding) -> Result<Self> {
        if binding != self::binding(&binding.id) {
            bail!("Invalid Hermes provider binding");
        }
        Ok(Self {
            backend,
            binding,
            retry: std::cell::Cell::new(false),
        })
    }
    fn guard(&self, binding: &Binding) -> Result<()> {
        if binding != &self.binding {
            bail!("Hermes provider binding changed; recovery retained");
        }
        Ok(())
    }
}
impl<B: Backend> Runtime for Adapter<B> {
    type Payload = Snapshot;
    type Work = Work;
    fn capture(&mut self, binding: &Binding, _: &[Game]) -> Result<Entry<Snapshot>> {
        self.guard(binding)?;
        let original = self.backend.capture()?;
        let snapshot = Snapshot {
            version: if original.gateway.is_some() { 2 } else { 1 },
            original,
            stage: Stage::Captured,
            loaded: vec![],
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
        entry.payload.validate()
    }
    fn validate_transition(&self, original: &Snapshot, updated: &Snapshot) -> Result<()> {
        original.validate_transition(updated)
    }
    fn begin(&self, snapshot: &mut Snapshot, _: Intent) -> Result<()> {
        snapshot.begin();
        Ok(())
    }
    fn complete(&self, snapshot: &Snapshot, intent: Intent) -> bool {
        intent == Intent::Pause && snapshot.pause_complete
    }
    fn retry(&self, _: &Binding) {
        self.retry.set(true);
    }
    fn note(&self, snapshot: &Snapshot) -> String {
        snapshot.note()
    }
    fn plan(&mut self, entry: &Entry<Snapshot>, intent: Intent) -> Result<Planned<Snapshot, Work>> {
        self.guard(&entry.binding)?;
        let mut snapshot = entry.payload.clone();
        if self.retry.replace(false) {
            snapshot.begin();
        }
        let operation = match intent {
            Intent::Pause if matches!(snapshot.stage, Stage::Stopped) => Operation::VerifyPause,
            Intent::Pause
                if snapshot.original.gateway.is_some()
                    && matches!(snapshot.stage, Stage::Captured | Stage::GatewayStopping) =>
            {
                snapshot.stage = Stage::GatewayStopping;
                Operation::StopGateway
            }
            Intent::Pause => {
                snapshot.stage = Stage::Stopping;
                Operation::Stop
            }
            Intent::Restore
                if matches!(
                    snapshot.stage,
                    Stage::Started
                        | Stage::Restored
                        | Stage::GatewayStarting
                        | Stage::GatewayStarted
                ) =>
            {
                if let Some(index) = snapshot
                    .original
                    .models
                    .iter()
                    .position(|m| !snapshot.loaded.contains(&m.id))
                {
                    Operation::Load(index)
                } else if snapshot.original.gateway.is_some()
                    && !matches!(snapshot.stage, Stage::GatewayStarted | Stage::Restored)
                {
                    snapshot.stage = Stage::GatewayStarting;
                    Operation::StartGateway
                } else {
                    Operation::VerifyRestore
                }
            }
            Intent::Restore => {
                snapshot.stage = Stage::Starting;
                Operation::Start
            }
            Intent::Reconcile => bail!("Hermes requires a reconciled direction"),
        };
        Ok(Planned {
            checkpoint: snapshot.clone(),
            work: Work {
                binding: entry.binding.clone(),
                snapshot,
                operation,
            },
        })
    }
    fn execute(&mut self, binding: &Binding, work: Work) -> Result<Outcome<Snapshot>> {
        self.guard(binding)?;
        if binding != &work.binding {
            bail!("Queued Hermes binding changed; recovery retained");
        }
        let mut snapshot = work.snapshot;
        snapshot.validate()?;
        let mut restore_complete = false;
        match work.operation {
            Operation::StopGateway => {
                self.backend.stop_gateway(&snapshot.original)?;
                snapshot.stage = Stage::GatewayStopped;
            }
            Operation::StartGateway => {
                self.backend.start_gateway(&snapshot.original)?;
                snapshot.stage = Stage::GatewayStarted;
            }
            Operation::Stop => {
                if snapshot.original.running {
                    self.backend.stop(&snapshot.original)?;
                }
                snapshot.stage = Stage::Stopped;
            }
            Operation::VerifyPause => {
                if snapshot.original.running {
                    self.backend.verify_paused(&snapshot.original)?;
                }
                snapshot.pause_complete = true;
            }
            Operation::Start => {
                if snapshot.original.running {
                    self.backend.start(&snapshot.original)?;
                }
                snapshot.stage = Stage::Started;
            }
            Operation::Load(index) => {
                let model = &snapshot.original.models[index];
                self.backend.load(&snapshot.original, model)?;
                snapshot.loaded.push(model.id.clone());
            }
            Operation::VerifyRestore => {
                if snapshot.original.running {
                    self.backend.verify_restored(&snapshot.original)?;
                }
                snapshot.stage = Stage::Restored;
                restore_complete = true;
            }
        }
        Ok(Outcome {
            payload: snapshot,
            restore_complete,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests;
