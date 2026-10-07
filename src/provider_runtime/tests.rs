use super::*;
use crate::{
    coordinator::{Coordinator, State, Step, Store},
    recovery::Journal,
};
use std::time::Duration;
trait Data: Clone {
    fn stage(&self) -> u8;
    fn set_stage(&mut self, stage: u8);
    fn original(&self) -> String;
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LMData {
    raw: String,
    stage: u8,
}
impl Data for LMData {
    fn stage(&self) -> u8 {
        self.stage
    }
    fn set_stage(&mut self, stage: u8) {
        self.stage = stage;
    }
    fn original(&self) -> String {
        self.raw.clone()
    }
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OllamaData {
    expires_at: u64,
    stage: u8,
}
impl Data for OllamaData {
    fn stage(&self) -> u8 {
        self.stage
    }
    fn set_stage(&mut self, stage: u8) {
        self.stage = stage;
    }
    fn original(&self) -> String {
        self.expires_at.to_string()
    }
}
#[derive(Clone)]
struct Fake<T> {
    value: T,
    capture_fail: bool,
    fail: Option<u8>,
    after_effect: bool,
    controls: usize,
}
impl<T: Data> Runtime for Fake<T> {
    type Payload = T;
    type Work = T;
    fn capture(&mut self, binding: &Binding, _: &[Game]) -> Result<Entry<T>> {
        if self.capture_fail {
            bail!("fixture capture failure");
        }
        Ok(Entry {
            binding: binding.clone(),
            payload: self.value.clone(),
            restore_complete: false,
        })
    }
    fn validate(&self, _: &Entry<T>, _: &[Game]) -> Result<()> {
        Ok(())
    }
    fn validate_transition(&self, old: &T, new: &T) -> Result<()> {
        if old.original() != new.original() {
            bail!("fixture originals changed");
        }
        Ok(())
    }
    fn begin(&self, payload: &mut T, _: Intent) -> Result<()> {
        payload.set_stage(0);
        Ok(())
    }
    fn complete(&self, payload: &T, intent: Intent) -> bool {
        intent == Intent::Pause && payload.stage() == 2
    }
    fn plan(&mut self, entry: &Entry<T>, intent: Intent) -> Result<Planned<T, T>> {
        let mut payload = entry.payload.clone();
        let stage = if intent == Intent::Pause {
            1
        } else if entry.payload.stage() >= 4 {
            5
        } else {
            3
        };
        payload.set_stage(stage);
        Ok(Planned {
            checkpoint: payload.clone(),
            work: payload,
        })
    }
    fn execute(&mut self, _: &Binding, mut work: T) -> Result<Outcome<T>> {
        let stage = work.stage();
        if self.fail == Some(stage) && !self.after_effect {
            bail!("fixture pre-effect failure");
        }
        self.controls += 1;
        work.set_stage(stage + 1);
        self.value = work.clone();
        if self.fail == Some(stage) {
            bail!("fixture lost response after effect");
        }
        Ok(Outcome {
            restore_complete: stage == 5,
            payload: work,
        })
    }
}
type TestRouter = Router<Fake<LMData>, Fake<OllamaData>>;
fn router() -> TestRouter {
    Router {
        lm: Fake {
            value: LMData {
                raw: "fixture raw LM settings".into(),
                stage: 0,
            },
            capture_fail: false,
            fail: None,
            after_effect: false,
            controls: 0,
        },
        ollama: Fake {
            value: OllamaData {
                expires_at: 1234567,
                stage: 0,
            },
            capture_fail: false,
            fail: None,
            after_effect: false,
            controls: 0,
        },
    }
}
fn bindings() -> Vec<Binding> {
    [Kind::LMStudio, Kind::Ollama]
        .into_iter()
        .map(|kind| {
            let (id, endpoint, guarantee) = match kind {
                Kind::LMStudio => ("lm", "127.0.0.1:1234", Guarantee::CapturedConfiguration),
                Kind::Ollama => ("ollama", "127.0.0.1:11434", Guarantee::SupportedFields),
                Kind::Process => unreachable!(),
            };
            Binding {
                id: id.into(),
                kind,
                endpoint: endpoint.into(),
                configured_endpoint: endpoint.into(),
                payload_version: 1,
                guarantee,
            }
        })
        .collect()
}
#[derive(Default)]
struct Disk {
    journal: Option<Journal<Routed<LMData, OllamaData>>>,
    fail: bool,
    fail_clear: bool,
    writes: usize,
}
impl Store<Routed<LMData, OllamaData>> for Disk {
    fn save(&mut self, journal: Option<&Journal<Routed<LMData, OllamaData>>>) -> Result<()> {
        self.writes += 1;
        if self.fail || (self.fail_clear && journal.is_none()) {
            bail!("fixture persistence failure");
        }
        self.journal = journal
            .map(|journal| serde_json::from_slice(&serde_json::to_vec(journal).unwrap()).unwrap());
        Ok(())
    }
}
#[test]
fn failed_clear_remains_ready_after_continuation_without_repeating_control() {
    let retry = Duration::from_secs(10);
    let mut c = Coordinator::new(router(), Disk::default(), bindings(), None, retry).unwrap();
    for _ in 0..6 {
        c.advance(Intent::Pause, &[], Duration::ZERO, &mut || false)
            .unwrap();
    }
    c.store.fail_clear = true;
    let mut failed_clear = false;
    for _ in 0..8 {
        if c.advance(Intent::Restore, &[], Duration::ZERO, &mut || false)
            .is_err()
        {
            failed_clear = true;
            break;
        }
    }
    assert!(failed_clear);
    let controls = (c.runtime.lm.controls, c.runtime.ollama.controls);
    let (runtime, mut store, memory) = c.into_parts();
    assert!(memory.persistence_pending());
    assert!(memory.ready(Intent::Restore, retry));
    assert!(
        memory
            .journal()
            .unwrap()
            .providers
            .iter()
            .all(|entry| entry.restore_complete)
    );
    store.fail_clear = false;
    let mut resumed = Coordinator::resume(runtime, store, bindings(), memory, retry).unwrap();
    assert_eq!(
        resumed
            .advance(Intent::Restore, &[], retry, &mut || false)
            .unwrap(),
        Step::Completed
    );
    assert_eq!(
        controls,
        (resumed.runtime.lm.controls, resumed.runtime.ollama.controls)
    );
    assert!(resumed.journal().is_none());
}
#[test]
fn continuation_preserves_independent_deadlines_and_healthy_completion() {
    let mut runtime = router();
    runtime.ollama.fail = Some(1);
    let retry = Duration::from_secs(10);
    let mut c = Coordinator::new(runtime, Disk::default(), bindings(), None, retry).unwrap();
    for _ in 0..8 {
        c.advance(Intent::Pause, &[], Duration::ZERO, &mut || false)
            .unwrap();
    }
    let healthy = c.runtime.lm.controls;
    let writes = c.store.writes;
    let (mut runtime, store, memory) = c.into_parts();
    assert!(!memory.ready(Intent::Pause, Duration::from_secs(9)));
    assert!(memory.ready(Intent::Pause, retry));
    assert!(
        memory.ready(Intent::Restore, Duration::ZERO),
        "direction change bypasses old pause backoff"
    );
    runtime.ollama.fail = None;
    let mut resumed = Coordinator::resume(runtime, store, bindings(), memory, retry).unwrap();
    assert_eq!(
        resumed
            .advance(Intent::Pause, &[], Duration::from_secs(9), &mut || false)
            .unwrap(),
        Step::Idle
    );
    assert_eq!(resumed.store.writes, writes);
    assert_eq!(resumed.runtime.lm.controls, healthy);
    resumed
        .advance(Intent::Pause, &[], retry, &mut || false)
        .unwrap();
    assert!(resumed.pause_complete());
    assert_eq!(resumed.runtime.lm.controls, healthy);
    let (_, _, memory) = resumed.into_parts();
    assert!(!memory.ready(Intent::Pause, retry));
}
#[test]
fn continuation_retains_global_failed_write_gate_and_manual_retry() {
    let retry = Duration::from_secs(10);
    let disk = Disk {
        fail: true,
        ..Default::default()
    };
    let mut c = Coordinator::new(router(), disk, bindings(), None, retry).unwrap();
    assert!(
        c.advance(Intent::Pause, &[], Duration::ZERO, &mut || false)
            .is_err()
    );
    let (runtime, store, memory) = c.into_parts();
    assert!(memory.persistence_pending());
    let mut resumed = Coordinator::resume(runtime, store, bindings(), memory, retry).unwrap();
    assert!(
        resumed
            .advance(Intent::Pause, &[], retry, &mut || false)
            .is_err()
    );
    assert_eq!(
        resumed.runtime.lm.controls + resumed.runtime.ollama.controls,
        0
    );
    resumed.store.fail = false;
    resumed.runtime.ollama.capture_fail = true;
    for _ in 0..8 {
        resumed
            .advance(Intent::Pause, &[], retry, &mut || false)
            .unwrap();
    }
    let (mut runtime, store, mut memory) = resumed.into_parts();
    assert!(!memory.ready(Intent::Pause, Duration::from_secs(11)));
    memory.request_retry();
    assert!(memory.ready(Intent::Pause, Duration::from_secs(11)));
    runtime.ollama.capture_fail = false;
    let mut resumed = Coordinator::resume(runtime, store, bindings(), memory, retry).unwrap();
    for _ in 0..4 {
        resumed
            .advance(Intent::Pause, &[], Duration::from_secs(11), &mut || false)
            .unwrap();
    }
    assert!(resumed.pause_complete());
}
#[test]
fn distinct_adapters_continue_healthy_work_through_each_routed_failure_and_retry() {
    for lm_fails in [true, false] {
        for point in [0, 1, 3, 5] {
            for after_effect in [false, true] {
                let mut runtime = router();
                if point == 0 {
                    if lm_fails {
                        runtime.lm.capture_fail = true;
                    } else {
                        runtime.ollama.capture_fail = true;
                    }
                } else if point == 1 {
                    if lm_fails {
                        runtime.lm.fail = Some(point);
                        runtime.lm.after_effect = after_effect;
                    } else {
                        runtime.ollama.fail = Some(point);
                        runtime.ollama.after_effect = after_effect;
                    }
                }
                let mut c = Coordinator::new(
                    runtime,
                    Disk::default(),
                    bindings(),
                    None,
                    Duration::from_secs(10),
                )
                .unwrap();
                for _ in 0..8 {
                    c.advance(Intent::Pause, &[], Duration::ZERO, &mut || false)
                        .unwrap();
                }
                let (failed, healthy) = if lm_fails {
                    ("lm", "ollama")
                } else {
                    ("ollama", "lm")
                };
                assert_eq!(c.statuses()[healthy].state, State::Paused);
                if point <= 1 {
                    assert_eq!(c.statuses()[failed].state, State::Failed);
                    assert!(!c.pause_complete());
                    let controls = (c.runtime.lm.controls, c.runtime.ollama.controls);
                    assert_eq!(
                        c.advance(Intent::Pause, &[], Duration::from_secs(9), &mut || false)
                            .unwrap(),
                        Step::Idle
                    );
                    assert_eq!(controls, (c.runtime.lm.controls, c.runtime.ollama.controls));
                    c.runtime.lm.capture_fail = false;
                    c.runtime.ollama.capture_fail = false;
                    c.runtime.lm.fail = None;
                    c.runtime.ollama.fail = None;
                    for _ in 0..5 {
                        c.advance(Intent::Pause, &[], Duration::from_secs(10), &mut || false)
                            .unwrap();
                    }
                    assert!(c.pause_complete());
                    let healthy_after = if lm_fails {
                        c.runtime.ollama.controls
                    } else {
                        c.runtime.lm.controls
                    };
                    let healthy_before = if lm_fails { controls.1 } else { controls.0 };
                    assert_eq!(
                        healthy_after, healthy_before,
                        "completed healthy provider stays quiet"
                    );
                }
                if point >= 3 {
                    if lm_fails {
                        c.runtime.lm.fail = Some(point);
                        c.runtime.lm.after_effect = after_effect;
                    } else {
                        c.runtime.ollama.fail = Some(point);
                        c.runtime.ollama.after_effect = after_effect;
                    }
                }
                for _ in 0..8 {
                    c.advance(Intent::Restore, &[], Duration::from_secs(11), &mut || false)
                        .unwrap();
                }
                assert_eq!(c.statuses()[healthy].state, State::Restored);
                if point >= 3 {
                    assert!(c.journal().is_some());
                    let reports = c.reports(Duration::from_secs(11));
                    assert!(reports.iter().any(|report| report.id == failed
                        && report.pending
                        && report.state == State::Failed));
                    assert!(reports.iter().any(|report| report.id == healthy
                        && !report.pending
                        && report.state == State::Restored));
                    c.runtime.lm.fail = None;
                    c.runtime.ollama.fail = None;
                    for _ in 0..6 {
                        c.advance(Intent::Restore, &[], Duration::from_secs(21), &mut || false)
                            .unwrap();
                    }
                }
                assert!(c.journal().is_none());
                assert!(c.store.journal.is_none());
                assert_eq!(c.runtime.lm.value.raw, "fixture raw LM settings");
                assert_eq!(c.runtime.ollama.value.expires_at, 1234567);
            }
        }
    }
}
#[test]
fn mismatched_payload_and_reassigned_queued_work_refuse_before_control() {
    let mut runtime = router();
    let bindings = bindings();
    let wrong = Entry {
        binding: bindings[0].clone(),
        payload: Routed::Ollama(runtime.ollama.value.clone()),
        restore_complete: false,
    };
    assert!(runtime.validate(&wrong, &[]).is_err());
    assert!(runtime.plan(&wrong, Intent::Pause).is_err());
    let entry = runtime.capture(&bindings[0], &[]).unwrap();
    let planned = runtime.plan(&entry, Intent::Pause).unwrap();
    let mut reassigned = bindings[0].clone();
    reassigned.id = "different-id".into();
    assert!(runtime.execute(&reassigned, planned.work).is_err());
    assert_eq!(runtime.lm.controls, 0);
    assert_eq!(runtime.ollama.controls, 0);
    assert!(
        runtime
            .validate_transition(&entry.payload, &wrong.payload)
            .is_err()
    );
}
#[test]
fn typed_envelope_restart_preserves_distinct_originals_after_partial_restore() {
    let mut c = Coordinator::new(
        router(),
        Disk::default(),
        bindings(),
        None,
        Duration::from_secs(10),
    )
    .unwrap();
    for _ in 0..6 {
        c.advance(Intent::Pause, &[], Duration::ZERO, &mut || false)
            .unwrap();
    }
    c.runtime.ollama.fail = Some(5);
    for _ in 0..8 {
        c.advance(Intent::Restore, &[], Duration::ZERO, &mut || false)
            .unwrap();
    }
    let saved = c.store.journal.clone().unwrap();
    assert!(
        saved
            .providers
            .iter()
            .any(|entry| entry.binding.kind == Kind::LMStudio && entry.restore_complete)
    );
    assert!(
        saved
            .providers
            .iter()
            .any(|entry| entry.binding.kind == Kind::Ollama && !entry.restore_complete)
    );
    let mut runtime = c.runtime.clone();
    runtime.ollama.fail = None;
    let mut restarted = Coordinator::new(
        runtime,
        Disk::default(),
        bindings(),
        Some(saved),
        Duration::from_secs(10),
    )
    .unwrap();
    for _ in 0..8 {
        restarted
            .advance(Intent::Restore, &[], Duration::ZERO, &mut || false)
            .unwrap();
    }
    assert!(restarted.journal().is_none());
    assert_eq!(restarted.runtime.lm.value.raw, "fixture raw LM settings");
    assert_eq!(restarted.runtime.ollama.value.expires_at, 1234567);
}
