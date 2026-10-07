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
    manual_conflict: bool,
    loadout_policy: bool,
    refuse_replacement: bool,
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
            manual_conflict: false,
            loadout_policy: false,
            refuse_replacement: false,
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
    fn recovery_models(&mut self) -> Result<Option<Vec<lmstudio::Resident>>> {
        Ok(self.loadout_policy.then(|| {
            self.current
                .values()
                .map(|m| lmstudio::Resident {
                    identifier: m.identifier.clone(),
                    model_key: m.model_key.clone(),
                    namespace: m.namespace.clone(),
                })
                .collect()
        }))
    }
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
        if self.loadout_policy {
            let journal: Journal = serde_json::from_slice(&fs::read(&self.path).unwrap()).unwrap();
            assert_eq!(journal.session.intent, Intent::Restore);
            validate_original(&self.original, journal.providers[0].payload.lm().unwrap()).unwrap();
            if self.refuse_replacement {
                bail!("fixture replacement failed");
            }
        } else {
            self.durable(id, Intent::Pause, "unloading");
        }
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
        if failure == Some(false) && self.manual_conflict {
            return Err(crate::provider::ManualRetryRequired("fixture TTL conflict".into()).into());
        }
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

#[test]
fn manual_model_conflict_finishes_healthy_models_then_stops_all_automatic_work() {
    let fixture = Fixture::new();
    let mut c = harness(Mock::new(true, fixture.path()), None);
    pump(&mut c, Intent::Pause);
    c.runtime.backend.fail_load = Some(("llm".into(), false));
    c.runtime.backend.manual_conflict = true;
    for _ in 0..24 {
        c.advance(Intent::Restore, &[], Duration::ZERO, &mut || false)
            .unwrap();
    }
    assert!(c.runtime.backend.current.contains_key("embedding"));
    assert!(!c.runtime.backend.current.contains_key("llm"));
    assert!(c.statuses().values().next().unwrap().manual_retry);
    let before = fs::read(fixture.path()).unwrap();
    let calls = c.runtime.backend.events.len();
    for _ in 0..24 {
        c.advance(Intent::Restore, &[], Duration::from_secs(1000), &mut || {
            false
        })
        .unwrap();
    }
    assert_eq!(c.runtime.backend.events.len(), calls);
    assert_eq!(fs::read(fixture.path()).unwrap(), before);
    let snapshot = c.journal().unwrap().providers[0].payload.lm().unwrap();
    assert_eq!(
        snapshot.models[0].load_config,
        c.runtime.backend.original.models[0].load_config
    );
    assert_eq!(snapshot.models[0].ttl_ms, Some(60000));
    let binding = c.runtime.binding().unwrap();
    let (mut runtime, store, mut memory) = c.into_parts();
    runtime.backend.fail_load = None;
    memory.request_retry();
    let mut c = Coordinator::resume(
        runtime,
        store,
        vec![binding],
        memory,
        Duration::from_secs(10),
    )
    .unwrap();
    pump(&mut c, Intent::Restore);
    assert!(c.journal().is_none());
}

fn loadout_harness(mut backend: Mock) -> Harness {
    backend.loadout_policy = true;
    let path = backend.path.clone();
    let snapshot = backend.original.clone();
    let runtime = LMRuntime::new(backend, Config::default(), false);
    let binding = runtime.binding().unwrap();
    let journal = Journal {
        schema: 3,
        session: recovery::Session {
            intent: Intent::Restore,
            games: vec![],
        },
        providers: vec![Entry {
            binding: Binding::capture(&Config::default(), &snapshot).unwrap(),
            payload: Payload::LMStudio(snapshot),
            restore_complete: false,
        }],
    };
    write_json(&path, &journal).unwrap();
    Coordinator::new(
        runtime,
        JournalFile(path),
        vec![binding],
        Some(journal),
        Duration::from_secs(10),
    )
    .unwrap()
}
#[test]
fn same_model_alias_settings_and_duplicate_count_are_accepted_without_mutation() {
    let fixture = Fixture::new();
    let mut backend = Mock::new(false, fixture.path());
    backend.original.models.retain(|m| m.namespace == "llm");
    let mut duplicate = backend.original.models[0].clone();
    duplicate.identifier = "llm:2".into();
    backend.original.models.push(duplicate);
    let mut resident = backend.original.models[0].clone();
    resident.identifier = "autoload".into();
    resident.ttl_ms = None;
    resident.load_config = json!({"fields":[]});
    resident.native_config = json!({"context_length":8192});
    backend.current = [(resident.identifier.clone(), resident.clone())].into();
    let mut c = loadout_harness(backend);
    pump(&mut c, Intent::Restore);
    assert!(c.journal().is_none());
    assert_eq!(c.runtime.backend.current.len(), 1);
    assert_eq!(c.runtime.backend.current["autoload"], resident);
    assert!(c.runtime.backend.events.is_empty());
    assert!(!c.runtime.backend.running);
    assert!(
        c.reports(Duration::ZERO)[0]
            .note
            .contains("kept current instance")
    );
}
#[test]
fn different_loadout_waits_for_confirmation_then_replaces_behind_checkpoints() {
    let fixture = Fixture::new();
    let mut backend = Mock::new(true, fixture.path());
    let mut other = backend.original.models[0].clone();
    other.model_key = "fixture/other@q8".into();
    other.identifier = "other".into();
    backend.current = [("other".into(), other)].into();
    let mut c = loadout_harness(backend);
    let original = c.runtime.backend.original.clone();
    pump(&mut c, Intent::Restore);
    assert!(c.runtime.backend.events.is_empty());
    assert!(c.statuses().values().next().unwrap().manual_retry);
    let offer = c.runtime.progress().loadout_offer().unwrap();
    let snapshot = c.journal().unwrap().providers[0].payload.lm().unwrap();
    validate_original(&original, snapshot).unwrap();
    for _ in 0..8 {
        c.advance(Intent::Restore, &[], Duration::from_secs(100), &mut || {
            false
        })
        .unwrap();
    }
    assert!(c.runtime.backend.events.is_empty());
    let bindings = vec![c.runtime.binding().unwrap()];
    let (runtime, store, mut memory) = c.into_parts();
    let mut progress = runtime.progress();
    progress.approve_replacement(&offer).unwrap();
    memory.request_retry();
    let mut c = Coordinator::resume(
        runtime.with_progress(progress),
        store,
        bindings,
        memory,
        Duration::from_secs(10),
    )
    .unwrap();
    pump(&mut c, Intent::Restore);
    assert!(c.journal().is_none());
    assert_eq!(c.runtime.backend.events.first().unwrap(), "unload:other");
    assert_eq!(c.runtime.backend.current.len(), 2);
    assert!(c.runtime.backend.current.contains_key("llm"));
}
#[test]
fn stale_replacement_and_restart_cannot_unload_changed_models() {
    for restart in [false, true] {
        let fixture = Fixture::new();
        let mut backend = Mock::new(true, fixture.path());
        backend.current.get_mut("llm").unwrap().model_key = "fixture/changed@q8".into();
        let mut c = loadout_harness(backend);
        pump(&mut c, Intent::Restore);
        let offer = c.runtime.progress().loadout_offer().unwrap();
        let bindings = vec![c.runtime.binding().unwrap()];
        let (runtime, store, mut memory) = c.into_parts();
        let mut progress = runtime.progress();
        progress.approve_replacement(&offer).unwrap();
        let mut runtime = runtime.with_progress(progress);
        runtime.backend.current.get_mut("llm").unwrap().model_key = "fixture/new@q4".into();
        memory.request_retry();
        let mut c = if restart {
            Coordinator::new(
                LMRuntime::new(runtime.backend, Config::default(), false),
                store,
                bindings,
                memory.journal().cloned(),
                Duration::from_secs(10),
            )
            .unwrap()
        } else {
            Coordinator::resume(runtime, store, bindings, memory, Duration::from_secs(10)).unwrap()
        };
        pump(&mut c, Intent::Restore);
        assert!(c.runtime.backend.events.is_empty());
        assert!(c.journal().is_some());
        assert!(c.runtime.progress().loadout_offer().is_some());
    }
}

#[test]
fn replacement_failure_keeps_original_recovery_and_power_revokes_approval() {
    let fixture = Fixture::new();
    let mut backend = Mock::new(true, fixture.path());
    backend.current.get_mut("llm").unwrap().model_key = "fixture/different@q8".into();
    backend.refuse_replacement = true;
    let mut c = loadout_harness(backend);
    pump(&mut c, Intent::Restore);
    let original = c.journal().unwrap().providers[0]
        .payload
        .lm()
        .unwrap()
        .clone();
    let offer = c.runtime.progress().loadout_offer().unwrap();
    let bindings = vec![c.runtime.binding().unwrap()];
    let (runtime, store, mut memory) = c.into_parts();
    let mut progress = runtime.progress();
    progress.approve_replacement(&offer).unwrap();
    let mut revoked = progress.clone();
    revoked.revoke_replacement();
    assert!(revoked.replacement.is_none());
    memory.request_retry();
    let mut c = Coordinator::resume(
        runtime.with_progress(progress),
        store,
        bindings,
        memory,
        Duration::from_secs(10),
    )
    .unwrap();
    pump(&mut c, Intent::Restore);
    assert!(c.journal().is_some());
    validate_original(
        &original,
        c.journal().unwrap().providers[0].payload.lm().unwrap(),
    )
    .unwrap();
    assert!(c.runtime.backend.events.is_empty());
}

#[test]
fn own_empty_loadout_restoration_preserves_original_server_lifecycle() {
    for running in [false, true] {
        let fixture = Fixture::new();
        let mut backend = Mock::new(running, fixture.path());
        backend.current.clear();
        backend.running = false;
        let mut c = loadout_harness(backend);
        pump(&mut c, Intent::Restore);
        assert!(c.journal().is_none());
        assert_eq!(c.runtime.backend.running, running);
        assert_eq!(c.runtime.backend.current.len(), 2);
    }
}
