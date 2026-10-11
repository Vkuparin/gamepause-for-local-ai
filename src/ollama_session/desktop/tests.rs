use super::*;
#[test]
fn snapshot_rejects_unknown_native_contract_and_incomplete_ready_identity() {
    let capture = crate::ollama_session::tests::desktop_capture();
    let mut snapshot = Snapshot {
        current: capture.instance.clone(),
        original: capture,
        phase: Phase::Captured,
        external_restart: false,
    };
    snapshot.validate().unwrap();
    snapshot.original.version = "future".into();
    assert!(snapshot.validate().is_err());
    snapshot.original.version = SUPPORTED_VERSION.into();
    snapshot.current.server = None;
    assert!(snapshot.validate().is_err());
    snapshot.phase = Phase::Starting;
    snapshot.validate().unwrap();
    snapshot.phase = Phase::Ready;
    assert!(snapshot.validate().is_err());
}
