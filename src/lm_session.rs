//! LM-owned checkpoint and lifecycle units for the common coordinator.
use crate::{
    config::{Config, normalized_endpoint},
    coordinator::{Outcome, Planned, Runtime},
    discovery::Game,
    lmstudio::{self, Backend, Snapshot},
    provider::{Backend as ProviderBackend, Guarantee, InferenceBusy, Kind},
    recovery::{Binding, Entry, Intent, Payload},
};
use anyhow::{Context, Result, bail};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
};

pub(crate) fn validate_original(original: &Snapshot, updated: &Snapshot) -> Result<()> {
    fn immutable(snapshot: &Snapshot) -> Result<serde_json::Value> {
        let mut original = snapshot.clone();
        original.server_stopped = false;
        original.pause_complete = false;
        original.games.clear();
        for model in &mut original.models {
            model.stage.clear();
        }
        Ok(serde_json::to_value(original)?)
    }
    if immutable(original)? != immutable(updated)? {
        bail!("Original LM Studio capture changed; recovery retained");
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    StopForPause,
    Unload(usize),
    VerifyPause,
    EnsureRestore,
    Restore(usize),
    VerifyModel(usize),
    FinishRestore,
}
pub struct Work {
    operation: Operation,
    snapshot: Snapshot,
}
impl Work {
    pub fn operation(&self) -> Operation {
        self.operation
    }
}
pub struct LMRuntime<B: Backend> {
    pub backend: B,
    pub config: Config,
    verify_raw: bool,
    ready: Cell<bool>,
    verified: Cell<usize>,
    failed_models: RefCell<BTreeSet<usize>>,
}
#[derive(Clone, Default)]
pub struct Progress {
    ready: bool,
    verified: usize,
    failed_models: BTreeSet<usize>,
}
impl<B: Backend> LMRuntime<B> {
    pub fn progress(&self) -> Progress {
        Progress {
            ready: self.ready.get(),
            verified: self.verified.get(),
            failed_models: self.failed_models.borrow().clone(),
        }
    }
    pub fn with_progress(self, progress: Progress) -> Self {
        self.ready.set(progress.ready);
        self.verified.set(progress.verified);
        *self.failed_models.borrow_mut() = progress.failed_models;
        self
    }
    pub fn new(backend: B, config: Config, verify_raw: bool) -> Self {
        Self {
            backend,
            config,
            verify_raw,
            ready: Cell::new(false),
            verified: Cell::new(0),
            failed_models: RefCell::new(BTreeSet::new()),
        }
    }
    pub fn binding(&self) -> Result<Binding> {
        let provider = self
            .config
            .providers
            .iter()
            .find(|provider| provider.kind() == Kind::LMStudio)
            .context("LM Studio provider is not configured")?;
        let endpoint = normalized_endpoint(provider.endpoint())?;
        Ok(Binding {
            id: provider.id().into(),
            kind: Kind::LMStudio,
            endpoint: endpoint.clone(),
            configured_endpoint: endpoint,
            payload_version: 1,
            guarantee: Guarantee::CapturedConfiguration,
        })
    }
    fn guard(&self, binding: &Binding) -> Result<()> {
        if self.config.mode != "active" {
            bail!("Observation cannot control LM Studio");
        }
        binding.validate_route(&self.config)
    }
    fn state(&mut self, port: u16) -> Result<bool> {
        let server = self.backend.server_state()?;
        let running = server["running"]
            .as_bool()
            .context("Unsupported LM Studio server status; recovery retained")?;
        if running && server["port"].as_u64() != Some(u64::from(port)) {
            bail!("Server port changed; recovery retained");
        }
        Ok(running)
    }
    fn keys(&mut self, pause: bool) -> Result<Vec<String>> {
        let loaded = self.backend.loaded()?;
        if pause
            && loaded.iter().any(|model| {
                model["status"]
                    .as_str()
                    .is_some_and(|status| status != "idle")
                    || model["queued"].as_u64().unwrap_or(0) > 0
            })
        {
            return Err(InferenceBusy.into());
        }
        lmstudio::resident_keys(&loaded)
    }
}
impl<B: Backend> Runtime for LMRuntime<B> {
    type Payload = Payload;
    type Work = Work;
    fn capture(&mut self, binding: &Binding, games: &[Game]) -> Result<Entry<Payload>> {
        self.guard(binding)?;
        let mut snapshot = self.backend.capture()?;
        snapshot.games = games.to_vec();
        let binding = Binding::capture(&self.config, &snapshot)?;
        Ok(Entry {
            binding,
            payload: Payload::LMStudio(snapshot),
            restore_complete: false,
        })
    }
    fn validate(&self, entry: &Entry<Payload>, games: &[Game]) -> Result<()> {
        let snapshot = entry.payload.lm()?;
        snapshot.validate_recovery()?;
        if entry.binding.kind != Kind::LMStudio
            || entry.binding.payload_version != 1
            || entry.binding.guarantee != Guarantee::CapturedConfiguration
            || normalized_endpoint(&entry.binding.endpoint)?
                != format!("127.0.0.1:{}", snapshot.server["port"].as_u64().unwrap())
            || serde_json::to_value(&snapshot.games)? != serde_json::to_value(games)?
            || (entry.restore_complete
                && (snapshot.pause_complete
                    || snapshot
                        .models
                        .iter()
                        .any(|model| model.stage != "restored")))
        {
            bail!("LM Studio recovery binding/progress mismatch; recovery retained");
        }
        Ok(())
    }
    fn validate_transition(&self, original: &Payload, updated: &Payload) -> Result<()> {
        validate_original(original.lm()?, updated.lm()?)
    }
    fn remember_games(&self, payload: &mut Payload, games: &[Game]) {
        if let Ok(snapshot) = payload.lm_mut() {
            snapshot.games = games.to_vec();
        }
    }
    fn begin(&self, payload: &mut Payload, _: Intent) -> Result<()> {
        let snapshot = payload.lm_mut()?;
        snapshot.pause_complete = false;
        self.ready.set(false);
        self.verified.set(0);
        self.failed_models.borrow_mut().clear();
        Ok(())
    }
    fn complete(&self, payload: &Payload, intent: Intent) -> bool {
        payload
            .lm()
            .is_ok_and(|snapshot| intent == Intent::Pause && snapshot.pause_complete)
    }
    fn can_continue(&self, entry: &Entry<Payload>, intent: Intent) -> bool {
        let Ok(snapshot) = entry.payload.lm() else {
            return false;
        };
        let failed = self.failed_models.borrow();
        intent == Intent::Restore
            && !failed.is_empty()
            && snapshot.models.iter().enumerate().any(|(index, model)| {
                !failed.contains(&index)
                    && (model.stage != "restored" || index >= self.verified.get())
            })
    }
    fn retry(&self, _: &Binding) {
        self.failed_models.borrow_mut().clear();
        self.ready.set(false);
        self.verified.set(0);
    }
    fn plan(&mut self, entry: &Entry<Payload>, intent: Intent) -> Result<Planned<Payload, Work>> {
        self.guard(&entry.binding)?;
        let original = entry.payload.lm()?;
        original.validate_recovery()?;
        if intent == Intent::Reconcile {
            bail!("LM Studio control requires a reconciled direction");
        }
        let mut snapshot = original.clone();
        let port = snapshot.server["port"].as_u64().unwrap() as u16;
        let running = self.state(port)?;
        let operation = if intent == Intent::Pause {
            let loaded = self.keys(true)?;
            if snapshot.pause_service_required(self.config.stop_server_during_gaming()) && running {
                snapshot.server_stopped = true;
                Operation::StopForPause
            } else if let Some(index) = snapshot
                .models
                .iter()
                .position(|model| model.stage != "unloaded" || loaded.contains(&model.identifier))
            {
                snapshot.models[index].stage = "unloading".into();
                Operation::Unload(index)
            } else {
                Operation::VerifyPause
            }
        } else {
            let need_server = snapshot.server_stopped
                || !snapshot.models.is_empty()
                || snapshot.server["running"] == true;
            if need_server && (!self.ready.get() || !running) {
                snapshot.server_stopped = true;
                Operation::EnsureRestore
            } else {
                let loaded = self.keys(false)?;
                let failed = self.failed_models.borrow();
                if let Some(index) =
                    snapshot
                        .models
                        .iter()
                        .enumerate()
                        .position(|(index, model)| {
                            !failed.contains(&index)
                                && (model.stage != "restored"
                                    || !loaded.contains(&model.identifier))
                        })
                {
                    snapshot.models[index].stage = "restoring".into();
                    Operation::Restore(index)
                } else if let Some(index) = (self.verified.get()..snapshot.models.len())
                    .find(|index| !failed.contains(index))
                {
                    Operation::VerifyModel(index)
                } else if !failed.is_empty() {
                    bail!("LM Studio model recovery failed; recovery retained");
                } else {
                    Operation::FinishRestore
                }
            }
        };
        Ok(Planned {
            checkpoint: Payload::LMStudio(snapshot.clone()),
            work: Work {
                operation,
                snapshot,
            },
        })
    }
    fn execute(&mut self, binding: &Binding, work: Work) -> Result<Outcome<Payload>> {
        let operation = work.operation;
        let result = self.execute_unit(binding, work);
        if result.is_err()
            && let Operation::Restore(index) | Operation::VerifyModel(index) = operation
        {
            self.failed_models.borrow_mut().insert(index);
        }
        result
    }
}
impl<B: Backend> LMRuntime<B> {
    fn execute_unit(&mut self, binding: &Binding, work: Work) -> Result<Outcome<Payload>> {
        self.guard(binding)?;
        let mut snapshot = work.snapshot;
        let port = snapshot.server["port"].as_u64().unwrap() as u16;
        self.backend.select_control_port(port)?;
        let mut restore_complete = false;
        match work.operation {
            Operation::StopForPause => {
                self.keys(true)?;
                self.state(port)?;
                self.backend.stop_server()?;
                if self.state(port)? {
                    bail!("LM Studio server is still running; pause incomplete");
                }
            }
            Operation::Unload(index) => {
                self.state(port)?;
                let loaded = self.keys(true)?;
                let model = &snapshot.models[index];
                if loaded.contains(&model.identifier) {
                    self.backend.unload_captured(model)?;
                }
                if self.keys(false)?.contains(&model.identifier) {
                    bail!("Captured model is still loaded; pause incomplete");
                }
                snapshot.models[index].stage = "unloaded".into();
            }
            Operation::VerifyPause => {
                let loaded = self.keys(true)?;
                if snapshot
                    .models
                    .iter()
                    .any(|model| loaded.contains(&model.identifier))
                {
                    bail!("Captured models remain loaded; pause incomplete");
                }
                if !loaded.is_empty() {
                    bail!(
                        "Other LM Studio models loaded after capture; protection is incomplete and recovery retained"
                    );
                }
                if snapshot.pause_service_required(self.config.stop_server_during_gaming())
                    && self.state(port)?
                {
                    bail!("LM Studio server resumed; pause incomplete");
                }
                snapshot.pause_complete = true;
            }
            Operation::EnsureRestore => {
                self.backend.ensure_server(port)?;
                if !self.state(port)? {
                    bail!("LM Studio server start was not verified; recovery retained");
                }
                self.ready.set(true);
                self.verified.set(0);
            }
            Operation::Restore(index) => {
                if !self.state(port)? {
                    bail!("LM Studio server stopped; recovery retained");
                }
                self.backend
                    .restore_captured(&snapshot.models[index], self.verify_raw)?;
                snapshot.models[index].stage = "restored".into();
                self.verified.set(0);
            }
            Operation::VerifyModel(index) => {
                self.backend.verify_restored(&snapshot.models[index])?;
                if self.verify_raw {
                    lmstudio::compare_fields(
                        &snapshot.models[index].load_config,
                        &self.backend.read_config(&snapshot.models[index])?,
                    )?;
                }
                self.verified.set(index + 1);
            }
            Operation::FinishRestore => {
                let loaded = self.keys(false)?;
                if snapshot
                    .models
                    .iter()
                    .any(|model| !loaded.contains(&model.identifier))
                {
                    bail!("Captured model disappeared; recovery retained");
                }
                if snapshot.close_restored_service() {
                    self.state(port)?;
                    self.backend.stop_server()?;
                }
                if self.state(port)? != (snapshot.server["running"] == true) {
                    bail!("Original LM Studio server state is unverified; recovery retained");
                }
                restore_complete = true;
            }
        }
        Ok(Outcome {
            payload: Payload::LMStudio(snapshot),
            restore_complete,
        })
    }
}

#[cfg(test)]
mod tests;
