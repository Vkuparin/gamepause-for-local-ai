use super::*;
use crate::hermes::{FileIdentity, Model, Process};
use std::collections::BTreeMap;

pub(crate) fn original() -> Original {
    Original {
        home: r"D:\Fixture\Hermes".into(),
        enabled: true,
        running: true,
        router: Some(Process {
            pid: 123,
            start_time: 456,
        }),
        children: vec![],
        engine: Some(FileIdentity {
            path: r"D:\Fixture\Hermes\engine.exe".into(),
            sha256: "a".repeat(64),
        }),
        preferences_sha256: "b".repeat(64),
        control_endpoint: "127.0.0.1:34567".into(),
        router_endpoint: "127.0.0.1:34568".into(),
        models: vec![Model {
            id: "fixture-model".into(),
            context: 8192,
            files: vec![FileIdentity {
                path: r"D:\Fixture\Hermes\model.gguf".into(),
                sha256: "c".repeat(64),
            }],
            settings: BTreeMap::new(),
        }],
    }
}
#[derive(Default)]
struct Mock {
    calls: Vec<&'static str>,
    fail_verify: bool,
}
impl Backend for Mock {
    fn capture(&mut self) -> Result<Original> {
        self.calls.push("capture");
        Ok(original())
    }
    fn stop(&mut self, _: &Original) -> Result<()> {
        self.calls.push("stop");
        Ok(())
    }
    fn verify_paused(&mut self, _: &Original) -> Result<()> {
        self.calls.push("verify-pause");
        if self.fail_verify {
            bail!("still resident");
        }
        Ok(())
    }
    fn start(&mut self, _: &Original) -> Result<()> {
        self.calls.push("start");
        Ok(())
    }
    fn load(&mut self, _: &Original, _: &Model) -> Result<()> {
        self.calls.push("load");
        Ok(())
    }
    fn verify_restored(&mut self, _: &Original) -> Result<()> {
        self.calls.push("verify-restore");
        Ok(())
    }
}
#[test]
fn acknowledgement_does_not_complete_pause_and_restart_replays_checkpoint() {
    let bind = binding("hermes-fixture");
    let mut adapter = Adapter::new(Mock::default(), bind.clone()).unwrap();
    let entry = adapter.capture(&bind, &[]).unwrap();
    let saved_original = entry.payload.original.clone();
    let planned = adapter.plan(&entry, Intent::Pause).unwrap();
    assert_eq!(adapter.backend.calls, ["capture"]);
    // The checkpoint alone is enough to replay a stop whose response was lost.
    let restart = Entry {
        payload: planned.checkpoint,
        ..entry
    };
    let mut adapter = Adapter::new(Mock::default(), bind.clone()).unwrap();
    let planned = adapter.plan(&restart, Intent::Pause).unwrap();
    let stopped = adapter.execute(&bind, planned.work).unwrap();
    assert!(!stopped.payload.pause_complete);
    let entry = Entry {
        payload: stopped.payload,
        binding: bind.clone(),
        restore_complete: false,
    };
    adapter.backend.fail_verify = true;
    let verify = adapter.plan(&entry, Intent::Pause).unwrap();
    assert!(adapter.execute(&bind, verify.work).is_err());
    assert_eq!(entry.payload.original, saved_original);
    adapter.backend.fail_verify = false;
    let verify = adapter.plan(&entry, Intent::Pause).unwrap();
    let mut paused = adapter.execute(&bind, verify.work).unwrap().payload;
    assert!(paused.pause_complete);
    adapter.begin(&mut paused, Intent::Restore).unwrap();
    let mut entry = Entry {
        payload: paused,
        binding: bind.clone(),
        restore_complete: false,
    };
    for _ in 0..3 {
        let unit = adapter.plan(&entry, Intent::Restore).unwrap();
        let outcome = adapter.execute(&bind, unit.work).unwrap();
        entry.payload = outcome.payload;
        entry.restore_complete = outcome.restore_complete;
    }
    assert!(entry.restore_complete);
    assert_eq!(entry.payload.original, saved_original);
    assert_eq!(
        adapter.backend.calls,
        [
            "stop",
            "verify-pause",
            "verify-pause",
            "start",
            "load",
            "verify-restore"
        ]
    );
}
#[test]
fn original_capture_and_queued_binding_cannot_be_reassigned() {
    let bind = binding("hermes-fixture");
    let mut adapter = Adapter::new(Mock::default(), bind.clone()).unwrap();
    let entry = adapter.capture(&bind, &[]).unwrap();
    let mut changed = entry.payload.clone();
    changed.original.models[0].context += 1;
    assert!(entry.payload.validate_transition(&changed).is_err());
    let unit = adapter.plan(&entry, Intent::Pause).unwrap();
    assert!(
        adapter
            .execute(&binding("other-fixture"), unit.work)
            .is_err()
    );
    assert_eq!(adapter.backend.calls, ["capture"]);
}
