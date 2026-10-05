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
mod tests {
    use super::*;
    use crate::{
        config::write_json,
        coordinator::{Coordinator, JournalFile, State, Step},
        lmstudio::Model,
        recovery::{self, Journal},
    };
    use serde_json::{Value, json};
    use std::{
        collections::BTreeMap,
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let folder = std::env::temp_dir().join(format!(
                "gamepause-lm-units-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&folder).unwrap();
            Self(folder)
        }
        fn path(&self) -> PathBuf {
            self.0.join("state.json")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[derive(Clone)]
    struct Mock {
        original: Snapshot,
        current: BTreeMap<String, Model>,
        running: bool,
        port: u16,
        start_ack_only: bool,
        stop_ack_only: bool,
        fail_load: Option<(String, bool)>,
        fail_verify: Option<String>,
        events: Vec<String>,
        path: PathBuf,
    }
    impl Mock {
        fn new(running: bool, path: PathBuf) -> Self {
            let models = ["llm","embedding"].into_iter().map(|namespace| Model {
                identifier:namespace.into(), model_key:format!("fixture/{namespace}@q4"), base_key:format!("fixture/{namespace}"), namespace:namespace.into(), ttl_ms:Some(60000),
                load_config:json!({"fields":[{"key":"opaque","value":{"kept":[1,true]}},{"key":"context","value":4096}]}), native_config:json!({"context_length":4096,"opaque_native":"kept"}), stage:"planned".into()
            }).collect::<Vec<_>>();
            let original = Snapshot {
                schema: 2,
                server: json!({"running":running,"port":4321,"opaque_original":"kept"}),
                server_stopped: false,
                pause_complete: false,
                games: vec![],
                models: models.clone(),
            };
            Self {
                original,
                current: models
                    .into_iter()
                    .map(|model| (model.identifier.clone(), model))
                    .collect(),
                running,
                port: 4321,
                start_ack_only: false,
                stop_ack_only: false,
                fail_load: None,
                fail_verify: None,
                events: Vec::new(),
                path,
            }
        }
        fn durable(&self, id: &str, intent: Intent, stage: &str) {
            let journal: Journal = serde_json::from_slice(&fs::read(&self.path).unwrap()).unwrap();
            assert_eq!(journal.session.intent, intent);
            let snapshot = journal.providers[0].payload.lm().unwrap();
            assert!(
                snapshot
                    .models
                    .iter()
                    .any(|model| model.identifier == id && model.stage == stage)
            );
        }
    }
    impl Backend for Mock {
        fn snapshot(&mut self) -> Result<Snapshot> {
            self.events.push("capture".into());
            Ok(self.original.clone())
        }
        fn loaded(&mut self) -> Result<Vec<Value>> {
            Ok(self
                .current
                .values()
                .map(|model| json!({"identifier":model.identifier,"status":"idle"}))
                .collect())
        }
        fn server_state(&mut self) -> Result<Value> {
            Ok(json!({"running":self.running,"port":self.port}))
        }
        fn stop_server(&mut self) -> Result<()> {
            self.events.push("stop".into());
            if !self.stop_ack_only {
                self.running = false;
            }
            Ok(())
        }
        fn start_server(&mut self, port: u16) -> Result<()> {
            self.events.push("start".into());
            self.port = port;
            if !self.start_ack_only {
                self.running = true;
            }
            Ok(())
        }
        fn ensure_server(&mut self, port: u16) -> Result<()> {
            if self.running && self.port != port {
                bail!("port changed");
            }
            if !self.running {
                self.start_server(port)?;
            }
            Ok(())
        }
        fn unload(&mut self, id: &str) -> Result<()> {
            self.durable(id, Intent::Pause, "unloading");
            self.events.push(format!("unload:{id}"));
            self.current.remove(id);
            Ok(())
        }
        fn restore(&mut self, model: &Model) -> Result<()> {
            self.durable(&model.identifier, Intent::Restore, "restoring");
            let failure = self
                .fail_load
                .as_ref()
                .filter(|(id, _)| id == &model.identifier)
                .map(|(_, after)| *after);
            if failure == Some(false) {
                bail!("fixture load failure: {}", model.identifier);
            }
            if !self.running {
                bail!("server stopped");
            }
            if !self.current.contains_key(&model.identifier) {
                self.events.push(format!("load:{}", model.identifier));
                self.current.insert(model.identifier.clone(), model.clone());
            }
            if failure == Some(true) {
                bail!("fixture lost load response: {}", model.identifier);
            }
            self.verify_restored(model)
        }
        fn read_config(&mut self, model: &Model) -> Result<Value> {
            Ok(self
                .current
                .get(&model.identifier)
                .context("missing model")?
                .load_config
                .clone())
        }
        fn verify_restored(&mut self, model: &Model) -> Result<()> {
            self.events.push(format!("verify:{}", model.identifier));
            if self.fail_verify.as_ref() == Some(&model.identifier) {
                bail!("fixture verification failure: {}", model.identifier);
            }
            if !self.running {
                bail!("verification server stopped");
            }
            let actual = self
                .current
                .get(&model.identifier)
                .context("missing model")?;
            if actual.model_key != model.model_key
                || actual.namespace != model.namespace
                || actual.ttl_ms != model.ttl_ms
                || actual.native_config != model.native_config
            {
                bail!("identity/native/TTL changed");
            }
            lmstudio::compare_fields(&model.load_config, &actual.load_config)
        }
    }
    type Harness = Coordinator<LMRuntime<Mock>, JournalFile>;
    #[test]
    fn failed_model_retains_recovery_while_healthy_models_finish_and_retry_is_bounded() {
        for failed_id in ["llm", "embedding"] {
            for after_effect in [false, true] {
                let fixture = Fixture::new();
                let mut c = harness(Mock::new(false, fixture.path()), None);
                pump(&mut c, Intent::Pause);
                let original = c.journal().unwrap().providers[0].payload.clone();
                c.runtime.backend.fail_load = Some((failed_id.into(), after_effect));
                for _ in 0..12 {
                    if c.advance(Intent::Restore, &[], Duration::ZERO, &mut || false)
                        .unwrap()
                        == Step::Idle
                    {
                        break;
                    }
                }
                let entry = &c.journal().unwrap().providers[0];
                assert!(!entry.restore_complete);
                c.runtime
                    .validate_transition(&original, &entry.payload)
                    .unwrap();
                let snapshot = entry.payload.lm().unwrap();
                let healthy = snapshot
                    .models
                    .iter()
                    .find(|model| model.identifier != failed_id)
                    .unwrap();
                assert_eq!(healthy.stage, "restored");
                assert!(
                    c.runtime
                        .backend
                        .events
                        .contains(&format!("verify:{}", healthy.identifier))
                );
                assert!(
                    c.runtime.backend.running,
                    "partial recovery must not close the service"
                );
                let status = c.statuses().values().next().unwrap();
                assert_eq!(status.state, State::Failed);
                assert!(status.error.contains(failed_id));
                assert_eq!(status.retry_at, Duration::from_secs(10));
                let events = c.runtime.backend.events.clone();
                c.runtime.backend.fail_load = None;
                assert_eq!(
                    c.advance(Intent::Restore, &[], Duration::from_secs(9), &mut || false)
                        .unwrap(),
                    Step::Idle
                );
                assert_eq!(c.runtime.backend.events, events);
                for _ in 0..12 {
                    c.advance(Intent::Restore, &[], Duration::from_secs(10), &mut || false)
                        .unwrap();
                    if c.journal().is_none() {
                        break;
                    }
                }
                assert!(c.journal().is_none());
                assert!(!c.runtime.backend.running);
            }
        }
    }
    #[test]
    fn read_only_model_failure_allows_other_verification_but_never_clears_recovery() {
        let fixture = Fixture::new();
        let mut c = harness(Mock::new(true, fixture.path()), None);
        pump(&mut c, Intent::Pause);
        for _ in 0..3 {
            c.advance(Intent::Restore, &[], Duration::ZERO, &mut || false)
                .unwrap();
        }
        c.runtime.backend.fail_verify = Some("llm".into());
        c.runtime.backend.events.clear();
        for _ in 0..6 {
            if c.advance(Intent::Restore, &[], Duration::ZERO, &mut || false)
                .unwrap()
                == Step::Idle
            {
                break;
            }
        }
        assert!(
            c.runtime
                .backend
                .events
                .contains(&"verify:embedding".into())
        );
        assert!(
            !c.runtime
                .backend
                .events
                .iter()
                .any(|event| event.starts_with("load:"))
        );
        assert!(c.journal().is_some());
        assert_eq!(c.statuses().values().next().unwrap().state, State::Failed);
        let mut backend = c.runtime.backend.clone();
        backend.fail_verify = None;
        let mut restarted = harness(backend, c.journal().cloned());
        pump(&mut restarted, Intent::Restore);
        assert!(restarted.journal().is_none());
    }
    fn harness(backend: Mock, journal: Option<Journal>) -> Harness {
        let path = backend.path.clone();
        let runtime = LMRuntime::new(backend, Config::default(), true);
        let binding = runtime.binding().unwrap();
        Coordinator::new(
            runtime,
            JournalFile(path),
            vec![binding],
            journal,
            Duration::from_secs(10),
        )
        .unwrap()
    }
    fn pump(c: &mut Harness, intent: Intent) {
        for _ in 0..24 {
            c.advance(intent, &[], Duration::ZERO, &mut || false)
                .unwrap();
            if c.statuses()
                .values()
                .any(|status| matches!(status.state, State::Failed | State::Deferred))
            {
                break;
            }
        }
    }
    #[test]
    fn real_payload_units_preserve_raw_fields_ports_and_original_service_state() {
        for running in [false, true] {
            let fixture = Fixture::new();
            let mut c = harness(Mock::new(running, fixture.path()), None);
            pump(&mut c, Intent::Pause);
            assert!(c.pause_complete());
            assert!(c.runtime.backend.current.is_empty());
            assert!(!c.runtime.backend.running);
            let binding = &c.journal().unwrap().providers[0].binding;
            assert_eq!(binding.endpoint, "127.0.0.1:4321");
            assert_eq!(binding.configured_endpoint, "127.0.0.1:1234");
            pump(&mut c, Intent::Restore);
            assert!(c.journal().is_none());
            assert_eq!(c.runtime.backend.running, running);
            assert_eq!(c.runtime.backend.port, 4321);
            for expected in &c.runtime.backend.original.models {
                let actual = &c.runtime.backend.current[&expected.identifier];
                assert_eq!(actual.load_config, expected.load_config);
                assert_eq!(actual.native_config, expected.native_config);
                assert_eq!(actual.ttl_ms, expected.ttl_ms);
                assert_eq!(actual.model_key, expected.model_key);
            }
            assert_eq!(
                c.runtime
                    .backend
                    .events
                    .iter()
                    .filter(|event| event.starts_with("load:"))
                    .count(),
                2
            );
            assert!(fs::read_to_string(fixture.path()).unwrap().trim() == "null");
        }
    }
    #[test]
    fn legacy_restart_matrix_recovers_all_model_stages_with_verified_lifecycle() {
        for running in [false, true] {
            for stage in ["planned", "unloading", "unloaded", "restoring", "restored"] {
                let fixture = Fixture::new();
                let mut backend = Mock::new(running, fixture.path());
                let mut source = backend.original.clone();
                source.server_stopped = true;
                for model in &mut source.models {
                    model.stage = stage.into();
                }
                if stage == "unloaded" {
                    backend.current.clear();
                }
                if matches!(stage, "unloading" | "restoring") {
                    backend.current.remove("embedding");
                }
                backend.running = matches!(stage, "restoring" | "restored");
                write_json(&fixture.path(), &source).unwrap();
                let original = fs::read(fixture.path()).unwrap();
                let journal = recovery::load(&fixture.path(), &Config::default()).unwrap();
                let mut c = harness(backend, journal);
                pump(&mut c, Intent::Restore);
                assert!(c.journal().is_none(), "{stage}");
                assert_eq!(c.runtime.backend.running, running);
                assert_eq!(c.runtime.backend.current.len(), 2);
                assert_eq!(
                    fs::read(fixture.path().with_extension("v2.backup.json")).unwrap(),
                    original
                );
            }
        }
    }
    #[test]
    fn service_acknowledgement_does_not_prove_pause_or_restore_completion() {
        let fixture = Fixture::new();
        let mut backend = Mock::new(true, fixture.path());
        backend.stop_ack_only = true;
        let mut c = harness(backend, None);
        pump(&mut c, Intent::Pause);
        assert!(!c.pause_complete());
        assert_eq!(c.runtime.backend.current.len(), 2);
        assert!(c.journal().is_some());
        let fixture = Fixture::new();
        let mut c = harness(Mock::new(false, fixture.path()), None);
        pump(&mut c, Intent::Pause);
        c.runtime.backend.start_ack_only = true;
        pump(&mut c, Intent::Restore);
        assert!(c.journal().is_some());
        assert!(c.runtime.backend.current.is_empty());
        assert!(!c.journal().unwrap().providers[0].restore_complete);
        let fixture = Fixture::new();
        let mut c = harness(Mock::new(false, fixture.path()), None);
        pump(&mut c, Intent::Pause);
        c.runtime.backend.stop_ack_only = true;
        pump(&mut c, Intent::Restore);
        assert!(c.journal().is_some());
        assert_eq!(c.runtime.backend.current.len(), 2);
        assert!(!c.journal().unwrap().providers[0].restore_complete);
    }
    #[test]
    fn changed_port_and_final_config_drift_retain_recovery_without_extra_loads() {
        let fixture = Fixture::new();
        let mut c = harness(Mock::new(true, fixture.path()), None);
        pump(&mut c, Intent::Pause);
        c.runtime.backend.running = true;
        c.runtime.backend.port = 5555;
        pump(&mut c, Intent::Restore);
        assert!(c.journal().is_some());
        assert!(c.runtime.backend.current.is_empty());
        assert_eq!(c.runtime.backend.port, 5555);
        let fixture = Fixture::new();
        let mut c = harness(Mock::new(true, fixture.path()), None);
        pump(&mut c, Intent::Pause);
        for _ in 0..3 {
            c.advance(Intent::Restore, &[], Duration::ZERO, &mut || false)
                .unwrap();
        }
        c.runtime
            .backend
            .current
            .get_mut("llm")
            .unwrap()
            .load_config["fields"][0]["value"] = json!("changed");
        let loads = c
            .runtime
            .backend
            .events
            .iter()
            .filter(|event| event.starts_with("load:"))
            .count();
        pump(&mut c, Intent::Restore);
        assert!(c.journal().is_some());
        assert!(!c.journal().unwrap().providers[0].restore_complete);
        assert_eq!(
            c.runtime
                .backend
                .events
                .iter()
                .filter(|event| event.starts_with("load:"))
                .count(),
            loads
        );
    }
    #[test]
    fn missing_model_between_verification_units_is_replayed_and_game_guard_repauses() {
        let fixture = Fixture::new();
        let mut c = harness(Mock::new(false, fixture.path()), None);
        pump(&mut c, Intent::Pause);
        for _ in 0..4 {
            c.advance(Intent::Restore, &[], Duration::ZERO, &mut || false)
                .unwrap();
        }
        c.runtime.backend.current.remove("llm");
        pump(&mut c, Intent::Restore);
        assert!(c.journal().is_none());
        assert_eq!(c.runtime.backend.current.len(), 2);
        let fixture = Fixture::new();
        let mut c = harness(Mock::new(false, fixture.path()), None);
        pump(&mut c, Intent::Pause);
        c.advance(Intent::Restore, &[], Duration::ZERO, &mut || false)
            .unwrap();
        let mut guards = 0;
        assert_eq!(
            c.advance(Intent::Restore, &[], Duration::ZERO, &mut || {
                guards += 1;
                guards >= 4
            })
            .unwrap(),
            Step::Interrupted
        );
        assert!(c.journal().is_some());
        pump(&mut c, Intent::Pause);
        assert!(c.pause_complete());
        assert!(!c.runtime.backend.running);
        assert!(c.runtime.backend.current.is_empty());
    }
    #[test]
    fn externally_added_model_cannot_produce_a_completed_pause_or_replace_capture() {
        let fixture = Fixture::new();
        let mut c = harness(Mock::new(true, fixture.path()), None);
        c.advance(Intent::Pause, &[], Duration::ZERO, &mut || false)
            .unwrap();
        let mut external = c.runtime.backend.original.models[0].clone();
        external.identifier = "outside-capture".into();
        c.runtime
            .backend
            .current
            .insert(external.identifier.clone(), external);
        pump(&mut c, Intent::Pause);
        assert!(!c.pause_complete());
        assert_eq!(c.runtime.backend.current.len(), 1);
        assert!(c.runtime.backend.current.contains_key("outside-capture"));
        let snapshot = c.journal().unwrap().providers[0].payload.lm().unwrap();
        assert_eq!(snapshot.models.len(), 2);
        assert!(
            c.statuses()
                .values()
                .any(|status| status.error.contains("protection is incomplete"))
        );
    }
}
