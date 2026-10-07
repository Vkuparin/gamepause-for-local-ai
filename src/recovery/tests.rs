use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
static SERIAL: AtomicU64 = AtomicU64::new(0);
struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "gamepause-journal-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        )))
    }
    fn path(&self) -> std::path::PathBuf {
        self.0.join("state.json")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn snapshot(stage: &str, running: bool) -> Snapshot {
    Snapshot { schema:2, server:json!({"running":running,"port":4321,"original_extra":"preserved"}), server_stopped:true, pause_complete:false,
            games:vec![Game::new("Custom", "fixture", "Fixture game", r"D:\Fixture Games\play.exe")],
            models:["llm","embedding"].iter().map(|namespace| crate::lmstudio::Model { identifier:(*namespace).into(), model_key:format!("fixture/{namespace}@q4"), base_key:format!("fixture/{namespace}"), namespace:(*namespace).into(), ttl_ms:Some(60000), load_config:json!({"fields":[{"key":"context","value":4096},{"key":"future_raw","value":{"nested":[1,true]}}]}), native_config:json!({"context_length":4096,"opaque_native":{"kept":true}}), stage:stage.into() }).collect() }
}
#[test]
fn typed_mixed_journal_round_trips_without_dropping_disabled_ollama() {
    let fixture = Fixture::new();
    let config = Config::default();
    let payload = snapshot("restored", true);
    let mut journal = Journal::lm(
        Binding::capture(&config, &payload).unwrap(),
        payload,
        Intent::Restore,
    );
    journal.providers[0].restore_complete = true;
    journal.providers.push(ollama_fixture());
    save(&fixture.path(), Some(&journal)).unwrap();
    let bytes = fs::read(fixture.path()).unwrap();
    let loaded = load(&fixture.path(), &config).unwrap().unwrap();
    assert_eq!(loaded, journal);
    assert!(
        loaded.into_lm().is_err(),
        "single-provider projection must never discard the second obligation"
    );
    assert_eq!(fs::read(fixture.path()).unwrap(), bytes);
    assert!(!fixture.path().with_extension("v2.backup.json").exists());
    for case in 0..3 {
        let mut changed = config.clone();
        match case {
            0 => {
                if let config::Provider::LMStudio { enabled, .. } = &mut changed.providers[0] {
                    *enabled = false;
                }
            }
            1 => changed
                .providers
                .retain(|provider| provider.kind() != Kind::LMStudio),
            _ => {
                if let config::Provider::LMStudio { connection, .. } = &mut changed.providers[0] {
                    connection.endpoint = "127.0.0.1:5432".into();
                }
            }
        }
        assert_eq!(load(&fixture.path(), &changed).unwrap().unwrap(), journal);
        assert_eq!(fs::read(fixture.path()).unwrap(), bytes);
        let mut unresolved = journal.clone();
        unresolved.providers[0].restore_complete = false;
        save(&fixture.path(), Some(&unresolved)).unwrap();
        let pending_bytes = fs::read(fixture.path()).unwrap();
        assert!(load(&fixture.path(), &changed).is_err(), "case {case}");
        assert_eq!(fs::read(fixture.path()).unwrap(), pending_bytes);
        save(&fixture.path(), Some(&journal)).unwrap();
    }
}
#[test]
fn invalid_mixed_payloads_and_failed_writes_preserve_both_originals() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let config = Config::default();
    let payload = snapshot("unloaded", true);
    let mut journal = Journal::lm(
        Binding::capture(&config, &payload).unwrap(),
        payload,
        Intent::Pause,
    );
    journal.providers.push(ollama_fixture());
    save(&fixture.path(), Some(&journal)).unwrap();
    let bytes = fs::read(fixture.path()).unwrap();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(fixture.path())
        .unwrap();
    assert!(save(&fixture.path(), Some(&journal)).is_err());
    assert_eq!(fs::read(fixture.path()).unwrap(), bytes);
    drop(held);
    for case in 0..5 {
        let mut invalid = journal.clone();
        match case {
            0 => {
                invalid.providers[1].binding.endpoint =
                    invalid.providers[0].binding.endpoint.clone()
            }
            1 => invalid.providers[1].binding.payload_version = 999,
            2 => invalid.providers[1].payload = invalid.providers[0].payload.clone(),
            3 => invalid.providers[1].restore_complete = true,
            _ => {
                if let Payload::Ollama(snapshot) = &mut invalid.providers[1].payload {
                    snapshot.expiry_policy = "future".into();
                }
            }
        }
        assert!(
            save(&fixture.path(), Some(&invalid)).is_err(),
            "case {case}"
        );
        assert_eq!(fs::read(fixture.path()).unwrap(), bytes);
    }
}
#[test]
fn legacy_migration_preserves_every_stage_raw_field_game_and_original_route() {
    for stage in ["planned", "unloading", "unloaded", "restoring", "restored"] {
        for running in [false, true] {
            let fixture = Fixture::new();
            let source = snapshot(stage, running);
            config::write_json(&fixture.path(), &source).unwrap();
            let original = fs::read(fixture.path()).unwrap();
            let config = Config::default();
            let journal = load(&fixture.path(), &config).unwrap().unwrap();
            assert_eq!(journal.schema, 3);
            let (binding, migrated, intent) = journal.into_lm().unwrap();
            assert_eq!(intent, Intent::Reconcile);
            assert_eq!(binding.endpoint, "127.0.0.1:4321");
            assert_eq!(binding.configured_endpoint, "127.0.0.1:1234");
            assert_eq!(
                serde_json::to_value(migrated).unwrap(),
                serde_json::to_value(source).unwrap()
            );
            assert_eq!(
                fs::read(fixture.path().with_extension("v2.backup.json")).unwrap(),
                original
            );
            let saved = fs::read(fixture.path()).unwrap();
            load(&fixture.path(), &config).unwrap();
            assert_eq!(fs::read(fixture.path()).unwrap(), saved);
        }
    }
}
#[test]
fn observation_does_not_migrate_or_execute_recovery() {
    let fixture = Fixture::new();
    config::write_json(&fixture.path(), &snapshot("unloading", false)).unwrap();
    let original = fs::read(fixture.path()).unwrap();
    let config = Config {
        mode: "observe".into(),
        ..Default::default()
    };
    assert!(load(&fixture.path(), &config).unwrap().is_some());
    assert_eq!(fs::read(fixture.path()).unwrap(), original);
    assert!(!fixture.path().with_extension("v2.backup.json").exists());
}
#[test]
fn failed_atomic_migration_and_backup_conflict_retain_authoritative_source() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    config::write_json(&fixture.path(), &snapshot("unloaded", false)).unwrap();
    let original = fs::read(fixture.path()).unwrap();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(fixture.path())
        .unwrap();
    assert!(load(&fixture.path(), &Config::default()).is_err());
    assert_eq!(fs::read(fixture.path()).unwrap(), original);
    assert_eq!(
        fs::read(fixture.path().with_extension("v2.backup.json")).unwrap(),
        original
    );
    drop(held);
    assert!(load(&fixture.path(), &Config::default()).unwrap().is_some());
    config::write_json(&fixture.path(), &snapshot("restoring", true)).unwrap();
    let changed = fs::read(fixture.path()).unwrap();
    assert!(load(&fixture.path(), &Config::default()).is_err());
    assert_eq!(fs::read(fixture.path()).unwrap(), changed);
    assert_eq!(
        fs::read(fixture.path().with_extension("v2.backup.json")).unwrap(),
        original
    );
}
#[test]
fn corrupt_unknown_or_mismatched_envelopes_never_replace_source() {
    let config = Config::default();
    let payload = snapshot("restoring", false);
    let source = serde_json::to_value(Journal::lm(
        Binding::capture(&config, &payload).unwrap(),
        payload,
        Intent::Restore,
    ))
    .unwrap();
    for change in 0..10 {
        let fixture = Fixture::new();
        let mut raw = source.clone();
        match change {
            0 => raw["schema"] = 99.into(),
            1 => raw["providers"][0]["binding"]["payload_version"] = 2.into(),
            2 => raw["providers"][0]["binding"]["guarantee"] = "monitor_only".into(),
            3 => raw["providers"][0]["binding"]["endpoint"] = "127.0.0.1:1234".into(),
            4 => raw["providers"][0]["payload"]["kind"] = "ollama".into(),
            5 => raw["session"]["gameplay_override"] = true.into(),
            6 => raw["session"]["games"] = json!([]),
            7 => {
                raw["providers"][0]["payload"]["snapshot"]["models"][0]["stage"] = "unknown".into()
            }
            8 => {
                let second = raw["providers"][0].clone();
                raw["providers"].as_array_mut().unwrap().push(second);
            }
            _ => raw["session"]["games"][0]["unknown_game_field"] = true.into(),
        }
        config::write_json(&fixture.path(), &raw).unwrap();
        let original = fs::read(fixture.path()).unwrap();
        assert!(load(&fixture.path(), &config).is_err(), "case {change}");
        assert_eq!(fs::read(fixture.path()).unwrap(), original);
        assert!(!fixture.path().with_extension("v2.backup.json").exists());
    }
}
#[test]
fn persisted_binding_refuses_missing_disabled_or_reassigned_provider() {
    let fixture = Fixture::new();
    let config = Config::default();
    let payload = snapshot("unloaded", true);
    save(
        &fixture.path(),
        Some(&Journal::lm(
            Binding::capture(&config, &payload).unwrap(),
            payload,
            Intent::Pause,
        )),
    )
    .unwrap();
    let original = fs::read(fixture.path()).unwrap();
    for change in 0..3 {
        let mut changed = config.clone();
        match change {
            0 => {
                changed.providers.remove(0);
            }
            1 => {
                if let crate::config::Provider::LMStudio { enabled, .. } = &mut changed.providers[0]
                {
                    *enabled = false;
                }
            }
            _ => changed.lm_mut().unwrap().endpoint = "127.0.0.1:4321".into(),
        }
        assert!(load(&fixture.path(), &changed).is_err());
        assert_eq!(fs::read(fixture.path()).unwrap(), original);
    }
    let mut equivalent = config.clone();
    equivalent.lm_mut().unwrap().endpoint = "localhost:01234".into();
    assert!(load(&fixture.path(), &equivalent).is_ok());
}
#[test]
fn bounded_input_refuses_overflow_after_one_detection_byte() {
    let bytes = vec![b' '; 100];
    let mut reader = std::io::Cursor::new(bytes);
    assert!(read_bounded(&mut reader, 5).is_err());
    assert_eq!(reader.position(), 6);
}
#[test]
fn optional_provider_completion_retains_old_envelopes_and_rejects_inconsistent_intent() {
    let fixture = Fixture::new();
    let config = Config::default();
    let payload = snapshot("restored", false);
    let mut journal = Journal::lm(
        Binding::capture(&config, &payload).unwrap(),
        payload,
        Intent::Restore,
    );
    let old = serde_json::to_value(&journal).unwrap();
    assert!(old["providers"][0].get("restore_complete").is_none());
    config::write_json(&fixture.path(), &old).unwrap();
    assert!(!load(&fixture.path(), &config).unwrap().unwrap().providers[0].restore_complete);
    journal.providers[0].restore_complete = true;
    save(&fixture.path(), Some(&journal)).unwrap();
    assert!(load(&fixture.path(), &config).unwrap().unwrap().providers[0].restore_complete);
    let original = fs::read(fixture.path()).unwrap();
    journal.session.intent = Intent::Pause;
    assert!(save(&fixture.path(), Some(&journal)).is_err());
    assert_eq!(fs::read(fixture.path()).unwrap(), original);
}
