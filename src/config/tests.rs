use super::*;
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "gamepause-settings-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        )))
    }
    fn path(&self) -> PathBuf {
        self.0.join("config.json")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn appearance_defaults_for_existing_settings_and_round_trips_each_choice() {
    let old = serde_json::json!({"settings_version": 3});
    assert_eq!(
        Config::parse(&old.to_string()).unwrap().appearance,
        Appearance::System
    );
    for appearance in [Appearance::System, Appearance::Light, Appearance::Dark] {
        let config = Config {
            appearance,
            ..Default::default()
        };
        assert_eq!(
            Config::parse(&serde_json::to_string(&config).unwrap())
                .unwrap()
                .appearance,
            appearance
        );
    }
    assert!(Config::parse(r#"{"settings_version":3,"appearance":"invalid"}"#).is_err());
}

#[test]
fn version_two_preserves_choices_and_backups_are_not_authority() {
    let fixture = Fixture::new();
    let legacy = serde_json::json!({"settings_version":2,"mode":"observe","automation_enabled":false,"api_host":"localhost:4321","lms_path":"D:\\Fixture\\lms.exe","stop_server_during_gaming":false,"restore_delay_seconds":41,"ignored_games":["D:\\Fixture\\game.exe"],"extra_games":[{"name":"Fixture","path":"D:\\Fixture\\game.exe"}],"excluded_paths":["D:\\Fixture\\Tools"],"excluded_executables":["tool.exe"],"steam_roots":["D:\\Fixture Steam"]});
    write_json(&fixture.path(), &legacy).unwrap();
    let original = fs::read(fixture.path()).unwrap();
    let mut migrated = Config::load(&fixture.path()).unwrap();
    assert_eq!(migrated.settings_version, 4);
    assert_eq!(migrated.mode, "observe");
    assert!(!migrated.automation_enabled);
    assert_eq!(migrated.lm_endpoint(), "localhost:4321");
    assert_eq!(migrated.lms_path(), r"D:\Fixture\lms.exe");
    assert!(!migrated.stop_server_during_gaming());
    assert_eq!(migrated.restore_delay_seconds, 41.);
    for key in [
        "ignored_games",
        "extra_games",
        "excluded_paths",
        "excluded_executables",
        "steam_roots",
    ] {
        assert_eq!(serde_json::to_value(&migrated).unwrap()[key], legacy[key]);
    }
    assert!(!migrated.advanced_settings_visible);
    assert!(migrated.notifications_enabled && migrated.sound_enabled);
    assert!(!migrated.providers[1].enabled());
    assert_eq!(
        fs::read(fixture.path().with_extension("v2.backup.json")).unwrap(),
        original
    );
    migrated.advanced_settings_visible = true;
    migrated.notifications_enabled = false;
    migrated.sound_enabled = false;
    write_json(&fixture.path(), &migrated).unwrap();
    fs::write(
        fixture.path().with_extension("v2.backup.json"),
        b"not active settings",
    )
    .unwrap();
    let restarted = Config::load(&fixture.path()).unwrap();
    assert!(restarted.advanced_settings_visible);
    assert!(!restarted.notifications_enabled);
    assert!(!restarted.sound_enabled);
    assert_eq!(restarted.providers, migrated.providers);
}
#[test]
fn invalid_migration_retains_source_without_backup() {
    for raw in [
        serde_json::json!({"settings_version":99}),
        serde_json::json!({"settings_version":2,"api_host":"remote:1234"}),
        serde_json::json!({"mode":"typo"}),
        serde_json::json!({"settings_version":2,"unknown":true}),
        serde_json::json!({"providers":[]}),
    ] {
        let fixture = Fixture::new();
        write_json(&fixture.path(), &raw).unwrap();
        let original = fs::read(fixture.path()).unwrap();
        assert!(Config::load(&fixture.path()).is_err());
        assert_eq!(fs::read(fixture.path()).unwrap(), original);
        assert!(!fixture.path().with_extension("v2.backup.json").exists());
    }
}
#[test]
fn failed_atomic_migration_retains_legacy_source_and_retry_uses_same_backup() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    write_json(
        &fixture.path(),
        &serde_json::json!({"settings_version":2,"automation_enabled":false}),
    )
    .unwrap();
    let original = fs::read(fixture.path()).unwrap();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(fixture.path())
        .unwrap();
    assert!(Config::load(&fixture.path()).is_err());
    assert_eq!(fs::read(fixture.path()).unwrap(), original);
    assert_eq!(
        fs::read(fixture.path().with_extension("v2.backup.json")).unwrap(),
        original
    );
    drop(held);
    assert!(!Config::load(&fixture.path()).unwrap().automation_enabled);
    let conflict = Fixture::new();
    write_json(&conflict.path(), &serde_json::json!({"settings_version":2})).unwrap();
    fs::write(
        conflict.path().with_extension("v2.backup.json"),
        b"different legacy source",
    )
    .unwrap();
    let original = fs::read(conflict.path()).unwrap();
    assert!(Config::load(&conflict.path()).is_err());
    assert_eq!(fs::read(conflict.path()).unwrap(), original);
}
#[test]
fn version_three_migrates_with_backup_and_takes_the_ollama_default_once() {
    let fixture = Fixture::new();
    let mut three = serde_json::to_value(Config::default()).unwrap();
    three["settings_version"] = 3.into();
    three["providers"][1]["enabled"] = false.into();
    three["restore_delay_seconds"] = 41.into();
    write_json(&fixture.path(), &three).unwrap();
    let original = fs::read(fixture.path()).unwrap();
    let backup = fixture.path().with_extension("v3.backup.json");
    // Doctor's in-memory parse migrates without writing.
    let text = String::from_utf8(original.clone()).unwrap();
    let parsed = Config::parse(&text).unwrap();
    assert!(parsed.ollama_enabled() && parsed.settings_version == 4);
    assert_eq!(fs::read(fixture.path()).unwrap(), original);
    assert!(!backup.exists());
    let mut loaded = Config::load(&fixture.path()).unwrap();
    assert!(loaded.ollama_enabled());
    assert_eq!(loaded.restore_delay_seconds, 41.);
    assert_eq!(fs::read(&backup).unwrap(), original);
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.path()).unwrap()).unwrap();
    assert_eq!(saved["settings_version"], 4);
    assert!(saved.get("ollama_default_applied").is_none());
    // Version 4 keeps a later choice to turn Ollama off.
    if let Provider::Ollama { enabled, .. } = &mut loaded.providers[1] {
        *enabled = false;
    }
    write_json(&fixture.path(), &loaded).unwrap();
    assert!(!Config::load(&fixture.path()).unwrap().ollama_enabled());
    // An interim version-3 file that carries the marker already chose.
    let mut chosen = three.clone();
    chosen["ollama_default_applied"] = true.into();
    assert!(!Config::parse(&chosen.to_string()).unwrap().ollama_enabled());
    // A different existing backup refuses migration and keeps the source.
    let conflict = Fixture::new();
    write_json(&conflict.path(), &three).unwrap();
    fs::write(conflict.path().with_extension("v3.backup.json"), b"other").unwrap();
    assert!(Config::load(&conflict.path()).is_err());
    assert_eq!(fs::read(conflict.path()).unwrap(), original);
    // The marker is not a version-4 field.
    let mut four = serde_json::to_value(Config::default()).unwrap();
    four["ollama_default_applied"] = true.into();
    assert!(Config::parse(&four.to_string()).is_err());
}
#[test]
fn ollama_endpoint_and_enablement_persist() {
    let fixture = Fixture::new();
    let mut config = Config::default();
    if let Provider::Ollama {
        enabled, endpoint, ..
    } = &mut config.providers[1]
    {
        *enabled = true;
        *endpoint = "localhost:24680".into();
    }
    config.validate().unwrap();
    write_json(&fixture.path(), &config).unwrap();
    let loaded = Config::load(&fixture.path()).unwrap();
    assert!(loaded.providers[1].enabled());
    assert_eq!(
        normalized_endpoint(loaded.providers[1].endpoint()).unwrap(),
        "127.0.0.1:24680"
    );
    assert!(!Config::default().providers[1].enabled());
}
#[test]
fn providers_refuse_ambiguous_identity_and_nonlocal_or_unknown_control() {
    let base = serde_json::to_value(Config::default()).unwrap();
    for change in [
        serde_json::json!({"id":""}),
        serde_json::json!({"id":"lmstudio-main"}),
        serde_json::json!({"endpoint":"localhost:1234"}),
        serde_json::json!({"endpoint":"https://127.0.0.1:11434"}),
        serde_json::json!({"endpoint":"remote:11434"}),
        serde_json::json!({"typo":false}),
        serde_json::json!({"kind":"unsupported"}),
    ] {
        let mut raw = base.clone();
        for (key, value) in change.as_object().unwrap() {
            raw["providers"][1][key] = value.clone();
        }
        assert!(
            serde_json::from_value::<Config>(raw)
                .and_then(|c| c.validate().map_err(serde::de::Error::custom))
                .is_err()
        );
    }
    let mut duplicate_kind = Config::default();
    duplicate_kind.providers[1] = duplicate_kind.providers[0].clone();
    if let Provider::LMStudio { id, connection, .. } = &mut duplicate_kind.providers[1] {
        *id = "another".into();
        connection.endpoint = "127.0.0.1:5678".into();
    }
    assert!(duplicate_kind.validate().is_err());
    for endpoint in [
        "127.0.0.1:0",
        "localhost:65536",
        "localhost:+12",
        "localhost:12/path",
        "[::1]:1234",
    ] {
        assert!(normalized_endpoint(endpoint).is_err());
    }
    assert_eq!(
        normalized_endpoint("localhost:01234").unwrap(),
        "127.0.0.1:1234"
    );
}
#[test]
fn config_validation() {
    assert!(Config::parse(include_str!("../../config.example.json")).is_ok());
    let mut c = Config::default();
    assert!(c.validate().is_ok());
    c.lm_mut().unwrap().endpoint = "example.com:1234".into();
    assert!(c.validate().is_err());
    c = Config::default();
    c.poll_seconds = f64::NAN;
    assert!(c.validate().is_err());
}
#[test]
fn unknown_settings_refused() {
    assert!(serde_json::from_str::<Config>(r#"{"poll_secnds":2}"#).is_err());
}
#[test]
fn legacy_observation_migrates_once_and_user_disable_survives_restart() {
    let path = std::env::temp_dir()
        .join(format!("gamepause-migration-{}", std::process::id()))
        .join("config.json");
    write_json(&path, &serde_json::json!({"mode":"observe","restore_delay_seconds":42,"excluded_paths":["D:\\Games\\Tool"]})).unwrap();
    let mut config = Config::load(&path).unwrap();
    assert_eq!(config.mode, "active");
    assert!(config.automation_enabled);
    assert_eq!(config.restore_delay_seconds, 42.);
    assert_eq!(config.excluded_paths.len(), 1);
    config.automation_enabled = false;
    write_json(&path, &config).unwrap();
    assert!(!Config::load(&path).unwrap().automation_enabled);
    fs::remove_file(path).unwrap();
}
#[test]
fn atomic_replace() {
    let p = std::env::temp_dir()
        .join(format!("gamepause-test-{}", std::process::id()))
        .join("state.json");
    write_json(&p, &serde_json::json!({"first":1})).unwrap();
    write_json(&p, &serde_json::json!({"second":2})).unwrap();
    assert!(fs::read_to_string(&p).unwrap().contains("second"));
    fs::remove_file(p).unwrap();
}

#[test]
fn hotkey_text_needs_a_real_modifier_and_exactly_one_key() {
    assert_eq!(parse_hotkey("").unwrap(), None);
    assert_eq!(parse_hotkey("  ").unwrap(), None);
    assert_eq!(parse_hotkey("Ctrl+Alt+P").unwrap(), Some((3, 0x50)));
    assert_eq!(parse_hotkey("ctrl + shift + f9").unwrap(), Some((6, 0x78)));
    assert_eq!(parse_hotkey("Win+Pause").unwrap(), Some((8, 0x13)));
    assert_eq!(parse_hotkey("Alt+7").unwrap(), Some((1, 0x37)));
    for invalid in [
        "P", "Shift+P", "Ctrl", "Ctrl+Alt", "Ctrl+P+Q", "Ctrl+F25", "Ctrl+é", "Ctrl++P",
    ] {
        assert!(parse_hotkey(invalid).is_err(), "{invalid}");
    }
    let config = Config {
        pause_hotkey: "Shift+P".into(),
        ..Default::default()
    };
    assert!(config.validate().is_err());
    assert!(
        Config::parse(r#"{"settings_version":4}"#)
            .unwrap()
            .pause_hotkey
            .is_empty()
    );
}
