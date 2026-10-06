use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}
impl Appearance {
    pub fn index(self) -> usize {
        match self {
            Self::System => 0,
            Self::Light => 1,
            Self::Dark => 2,
        }
    }
    pub fn from_index(index: isize) -> Option<Self> {
        match index {
            0 => Some(Self::System),
            1 => Some(Self::Light),
            2 => Some(Self::Dark),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub poll_seconds: f64,
    pub discovery_seconds: f64,
    pub restore_delay_seconds: f64,
    pub retry_seconds: f64,
    pub mode: String,
    pub settings_version: u32,
    pub automation_enabled: bool,
    pub ignored_games: Vec<String>,
    /// Games that never pause AI on their own: GamePause asks instead, and
    /// AI keeps running until the user answers.
    pub ask_games: Vec<String>,
    pub providers: Vec<Provider>,
    pub advanced_settings_visible: bool,
    pub appearance: Appearance,
    pub notifications_enabled: bool,
    pub sound_enabled: bool,
    pub steam_roots: Vec<String>,
    pub epic_manifest_dirs: Vec<String>,
    pub game_roots: Vec<String>,
    pub extra_games: Vec<ExtraGame>,
    pub excluded_executables: Vec<String>,
    pub excluded_paths: Vec<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Current,
    VersionThree,
    Legacy,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtraGame {
    pub name: String,
    pub path: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Provider {
    LMStudio {
        id: String,
        enabled: bool,
        connection: LMConnection,
    },
    Ollama {
        id: String,
        enabled: bool,
        endpoint: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LMConnection {
    pub endpoint: String,
    pub lms_path: String,
    pub stop_server_during_gaming: bool,
}
impl Provider {
    pub fn id(&self) -> &str {
        match self {
            Self::LMStudio { id, .. } | Self::Ollama { id, .. } => id,
        }
    }
    pub fn enabled(&self) -> bool {
        match self {
            Self::LMStudio { enabled, .. } | Self::Ollama { enabled, .. } => *enabled,
        }
    }
    pub fn kind(&self) -> crate::provider::Kind {
        match self {
            Self::LMStudio { .. } => crate::provider::Kind::LMStudio,
            Self::Ollama { .. } => crate::provider::Kind::Ollama,
        }
    }
    pub fn endpoint(&self) -> &str {
        match self {
            Self::LMStudio { connection, .. } => &connection.endpoint,
            Self::Ollama { endpoint, .. } => endpoint,
        }
    }
}
fn default_providers() -> Vec<Provider> {
    vec![
        Provider::LMStudio {
            id: "lmstudio-main".into(),
            enabled: true,
            connection: LMConnection {
                endpoint: "127.0.0.1:1234".into(),
                lms_path: String::new(),
                stop_server_during_gaming: true,
            },
        },
        Provider::Ollama {
            id: "ollama-main".into(),
            // Unit tests must not reach a developer's real Ollama through
            // default settings; fixtures enable it against private endpoints.
            enabled: !cfg!(test),
            endpoint: "127.0.0.1:11434".into(),
        },
    ]
}
impl Default for Config {
    fn default() -> Self {
        Self {
            poll_seconds: 2.,
            discovery_seconds: 30.,
            restore_delay_seconds: 30.,
            retry_seconds: 30.,
            mode: "active".into(),
            settings_version: 4,
            automation_enabled: true,
            ignored_games: vec![],
            ask_games: vec![],
            providers: default_providers(),
            advanced_settings_visible: false,
            appearance: Appearance::System,
            notifications_enabled: true,
            sound_enabled: true,
            steam_roots: vec![],
            epic_manifest_dirs: vec![],
            game_roots: vec![],
            extra_games: vec![],
            excluded_executables: vec![],
            excluded_paths: vec![],
        }
    }
}
impl Config {
    /// The "ask" rule for a game folder or executable path.
    pub fn asks(&self, path: &str) -> bool {
        self.ask_games
            .iter()
            .any(|ask| crate::discovery::canonical(ask) == crate::discovery::canonical(path))
    }
    pub fn any_provider_enabled(&self) -> bool {
        self.providers.iter().any(Provider::enabled)
    }
    pub fn ollama_enabled(&self) -> bool {
        self.providers
            .iter()
            .any(|provider| provider.kind() == crate::provider::Kind::Ollama && provider.enabled())
    }
    /// Validated in-memory migration for read-only diagnostics. No files change.
    pub fn parse(text: &str) -> Result<Self> {
        Ok(Self::decode(text)?.0)
    }
    /// Returns the settings and the format they were read from. Version-2 and
    /// unversioned files pass through the version-3 shape on their way to 4.
    fn decode(text: &str) -> Result<(Self, Source)> {
        let mut raw: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .context("Invalid configuration")?;
        let legacy = raw.get("settings_version").is_none() || raw["settings_version"] == 2;
        if legacy {
            let unversioned = raw.get("settings_version").is_none();
            let map = raw
                .as_object_mut()
                .context("Configuration must be an object")?;
            if map
                .get("mode")
                .is_some_and(|mode| mode != "active" && mode != "observe")
            {
                bail!("Invalid legacy mode; source retained");
            }
            for key in [
                "providers",
                "advanced_settings_visible",
                "appearance",
                "notifications_enabled",
                "sound_enabled",
            ] {
                if map.contains_key(key) {
                    bail!("Version-3 field in legacy configuration; source retained");
                }
            }
            let mut providers = default_providers();
            if let Provider::LMStudio { connection, .. } = &mut providers[0] {
                if let Some(value) = map.remove("api_host") {
                    connection.endpoint = serde_json::from_value(value)?;
                }
                if let Some(value) = map.remove("lms_path") {
                    connection.lms_path = serde_json::from_value(value)?;
                }
                if let Some(value) = map.remove("stop_server_during_gaming") {
                    connection.stop_server_during_gaming = serde_json::from_value(value)?;
                }
            }
            map.insert("providers".into(), serde_json::to_value(providers)?);
            map.insert("settings_version".into(), 3.into());
            if unversioned {
                map.insert("mode".into(), "active".into());
            }
        }
        let version_three = raw["settings_version"] == 3;
        if version_three {
            let map = raw
                .as_object_mut()
                .context("Configuration must be an object")?;
            // Version 3 shipped with Ollama off by default. A file without the
            // interim marker never chose that, so it gets the current default;
            // a file with the marker keeps whatever its owner saved.
            let chosen = map.remove("ollama_default_applied").is_some();
            if !legacy
                && !chosen
                && let Some(providers) = map.get_mut("providers").and_then(|v| v.as_array_mut())
            {
                for provider in providers.iter_mut().filter(|p| p["kind"] == "ollama") {
                    provider["enabled"] = true.into();
                }
            }
            map.insert("settings_version".into(), 4.into());
        }
        let config: Self = serde_json::from_value(raw).context("Invalid configuration")?;
        config.validate()?;
        Ok((
            config,
            if legacy {
                Source::Legacy
            } else if version_three {
                Source::VersionThree
            } else {
                Source::Current
            },
        ))
    }
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            write_json(path, &Self::default())?;
        }
        let text = fs::read_to_string(path)?;
        let (config, source) = Self::decode(&text)?;
        let backup = match source {
            Source::Current => return Ok(config),
            Source::Legacy => path.with_extension("v2.backup.json"),
            Source::VersionThree => path.with_extension("v3.backup.json"),
        };
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)
        {
            Ok(mut file) => {
                file.write_all(text.as_bytes())?;
                file.sync_all()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if fs::read(&backup)? != text.as_bytes() {
                    bail!("Settings backup already contains different data; source retained");
                }
            }
            Err(e) => return Err(e.into()),
        }
        write_json(path, &config).context("Settings migration could not save; source retained")?;
        Ok(config)
    }
    pub fn lm(&self) -> Option<&LMConnection> {
        self.providers.iter().find_map(|provider| match provider {
            Provider::LMStudio { connection, .. } => Some(connection),
            _ => None,
        })
    }
    pub fn lm_mut(&mut self) -> Result<&mut LMConnection> {
        self.providers
            .iter_mut()
            .find_map(|provider| match provider {
                Provider::LMStudio { connection, .. } => Some(connection),
                _ => None,
            })
            .context("LM Studio provider is not configured")
    }
    pub fn lm_endpoint(&self) -> &str {
        self.lm().map_or("", |lm| lm.endpoint.as_str())
    }
    pub fn lms_path(&self) -> &str {
        self.lm().map_or("", |lm| lm.lms_path.as_str())
    }
    pub fn lm_enabled(&self) -> bool {
        self.providers.iter().any(|provider| {
            provider.kind() == crate::provider::Kind::LMStudio && provider.enabled()
        })
    }
    pub fn stop_server_during_gaming(&self) -> bool {
        self.lm().is_some_and(|lm| lm.stop_server_during_gaming)
    }
    pub fn validate_recovery_edit(&self, updated: &Self) -> Result<()> {
        // Loaded recovery has already validated this configured LM binding.
        // Recovery uses the journal's original route, not a newly selected port.
        if let Some(original) = self
            .providers
            .iter()
            .find(|p| p.kind() == crate::provider::Kind::LMStudio)
        {
            let replacement = updated.providers.iter().find(|p| p.id() == original.id());
            if replacement.is_none_or(|p| {
                !p.enabled()
                    || p.kind() != original.kind()
                    || normalized_endpoint(p.endpoint()).ok()
                        != normalized_endpoint(original.endpoint()).ok()
            }) {
                bail!(
                    "LM Studio recovery is pending; restore it before disabling, removing or reassigning its endpoint/ID"
                );
            }
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        for (name, value, min) in [
            ("poll_seconds", self.poll_seconds, 0.5),
            ("discovery_seconds", self.discovery_seconds, 10.),
            ("restore_delay_seconds", self.restore_delay_seconds, 0.),
            ("retry_seconds", self.retry_seconds, 5.),
        ] {
            if !value.is_finite() || value < min || value > 86400. {
                bail!("{name} must be finite and between {min} and 86400 seconds");
            }
        }
        if !["active", "observe"].contains(&self.mode.as_str()) {
            bail!("mode must be active or observe");
        }
        if self.settings_version != 4 {
            bail!("Unsupported settings version; configuration retained");
        }
        let mut ids = std::collections::HashSet::new();
        let mut kinds = std::collections::HashSet::new();
        let mut endpoints = std::collections::HashSet::new();
        if self.providers.len() > 2 {
            bail!("Only one endpoint per supported provider kind is allowed");
        }
        for provider in &self.providers {
            let id = provider.id();
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
                || !ids.insert(id)
            {
                bail!("Invalid or duplicate provider ID");
            }
            if !kinds.insert(provider.kind()) {
                bail!("Duplicate provider kind");
            }
            if !endpoints.insert(normalized_endpoint(provider.endpoint())?) {
                bail!("Duplicate provider endpoint");
            }
        }
        for game in &self.extra_games {
            if game.name.is_empty() || game.path.is_empty() {
                bail!("extra_games requires name and path");
            }
        }
        Ok(())
    }
}
pub fn normalized_endpoint(endpoint: &str) -> Result<String> {
    let (host, port) = endpoint
        .rsplit_once(':')
        .context("Provider endpoint must be local host:port")?;
    if !["127.0.0.1", "localhost"].contains(&host)
        || !port.bytes().all(|c| c.is_ascii_digit())
        || port.parse::<u16>().unwrap_or(0) == 0
    {
        bail!("Provider endpoint must be localhost or 127.0.0.1 with a valid port");
    }
    Ok(format!("127.0.0.1:{}", port.parse::<u16>()?))
}
pub fn data_directory() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("GamePause")
}
static SERIAL: AtomicU64 = AtomicU64::new(0);
pub fn write_json(path: &Path, value: &(impl Serialize + ?Sized)) -> Result<()> {
    let parent = path.parent().context("File has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".gamepause-{}-{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        let source = crate::wide(&temp.to_string_lossy());
        let target = crate::wide(&path.to_string_lossy());
        unsafe {
            if windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                source.as_ptr(),
                target.as_ptr(),
                windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                    | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        Ok(())
    })();
    let _ = fs::remove_file(temp);
    result
}
pub fn lock(folder: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(folder.join("instance.lock"))
        .context("GamePause is already running with this data directory")
}
#[cfg(test)]
mod tests {
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
        assert!(Config::parse(include_str!("../config.example.json")).is_ok());
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
}
