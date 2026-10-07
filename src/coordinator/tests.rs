
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
            Kind::Ollama | Kind::Process => Payload::Other {
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
    fn plan(&mut self, entry: &Entry<Payload>, intent: Intent) -> Result<Planned<Payload, Work>> {
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
