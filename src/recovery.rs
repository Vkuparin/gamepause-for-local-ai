//! Versioned recovery storage. Transport payloads and model stages stay adapter-owned.
use crate::{
    config::{self, Config},
    discovery::Game,
    lmstudio::Snapshot,
    provider::{Guarantee, Kind},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::fs;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

const MAX_BYTES: usize = 16 * 1024 * 1024;
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    Reconcile,
    Pause,
    Restore,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub games: Vec<Game>,
    pub intent: Intent,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub id: String,
    pub kind: Kind,
    /// Original actual control route, which can differ from the configured port.
    pub endpoint: String,
    pub configured_endpoint: String,
    pub payload_version: u32,
    pub guarantee: Guarantee,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "snapshot",
    rename_all = "lowercase",
    deny_unknown_fields
)]
pub enum Payload {
    LMStudio(Snapshot),
    Ollama(crate::ollama_session::Snapshot),
}
impl Payload {
    pub fn lm(&self) -> Result<&Snapshot> {
        match self {
            Self::LMStudio(snapshot) => Ok(snapshot),
            Self::Ollama(_) => bail!("Expected LM Studio recovery payload; all recovery retained"),
        }
    }
    pub fn lm_mut(&mut self) -> Result<&mut Snapshot> {
        match self {
            Self::LMStudio(snapshot) => Ok(snapshot),
            Self::Ollama(_) => bail!("Expected LM Studio recovery payload; all recovery retained"),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry<P = Payload> {
    pub binding: Binding,
    pub payload: P,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restore_complete: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal<P = Payload> {
    pub schema: u32,
    pub session: Session,
    pub providers: Vec<Entry<P>>,
}

impl Binding {
    pub fn capture(config: &Config, snapshot: &Snapshot) -> Result<Self> {
        snapshot.validate_recovery()?;
        let provider = config
            .providers
            .iter()
            .find(|provider| provider.kind() == Kind::LMStudio)
            .context("LM Studio recovery provider is not configured; recovery retained")?;
        Ok(Self {
            id: provider.id().into(),
            kind: Kind::LMStudio,
            endpoint: format!("127.0.0.1:{}", snapshot.server["port"].as_u64().unwrap()),
            configured_endpoint: config::normalized_endpoint(provider.endpoint())?,
            payload_version: 1,
            guarantee: Guarantee::CapturedConfiguration,
        })
    }
    pub fn validate_route(&self, config: &Config) -> Result<()> {
        let configured = config.providers.iter().find(|provider| provider.id() == self.id).context("Recovery provider ID is missing; restore its original configuration. Recovery retained")?;
        if !configured.enabled()
            || configured.kind() != self.kind
            || config::normalized_endpoint(configured.endpoint())?
                != config::normalized_endpoint(&self.configured_endpoint)?
        {
            bail!(
                "{} recovery is pending; its provider was disabled or its endpoint/kind changed. Restore the original configuration; recovery retained",
                self.kind.name()
            );
        }
        Ok(())
    }
}
impl Journal {
    pub fn lm(binding: Binding, snapshot: Snapshot, intent: Intent) -> Self {
        Self {
            schema: 3,
            session: Session {
                games: snapshot.games.clone(),
                intent,
            },
            providers: vec![Entry {
                binding,
                payload: Payload::LMStudio(snapshot),
                restore_complete: false,
            }],
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.schema != 3 {
            bail!("Unsupported recovery format; source retained");
        }
        if !(1..=2).contains(&self.providers.len()) {
            bail!("Unsupported recovery provider set; source retained");
        }
        crate::coordinator::validate_bindings(self.providers.iter().map(|entry| &entry.binding))?;
        for entry in &self.providers {
            let binding = &entry.binding;
            if binding.payload_version != 1
                || (entry.restore_complete && self.session.intent != Intent::Restore)
            {
                bail!("Unsupported recovery payload version or completion intent; source retained");
            }
            match (&entry.payload, binding.kind, binding.guarantee) {
                (Payload::LMStudio(snapshot), Kind::LMStudio, Guarantee::CapturedConfiguration) => {
                    snapshot.validate_recovery()?;
                    if config::normalized_endpoint(&binding.endpoint)?
                        != format!("127.0.0.1:{}", snapshot.server["port"].as_u64().unwrap())
                        || serde_json::to_value(&self.session.games)?
                            != serde_json::to_value(&snapshot.games)?
                        || (self.session.intent == Intent::Restore && snapshot.pause_complete)
                        || (entry.restore_complete
                            && snapshot
                                .models
                                .iter()
                                .any(|model| model.stage != "restored"))
                    {
                        bail!("Recovery session/payload or endpoint mismatch; source retained");
                    }
                }
                (Payload::Ollama(snapshot), Kind::Ollama, Guarantee::SupportedFields) => {
                    snapshot.validate()?;
                    if config::normalized_endpoint(&binding.endpoint)?
                        != config::normalized_endpoint(&binding.configured_endpoint)?
                        || (self.session.intent == Intent::Restore && snapshot.pause_complete)
                        || (entry.restore_complete
                            && snapshot.models.iter().any(|model| {
                                !matches!(
                                    model.stage,
                                    crate::ollama_session::Stage::LoadAcknowledged
                                        | crate::ollama_session::Stage::Expired
                                )
                            }))
                    {
                        bail!("Ollama recovery binding/progress mismatch; source retained");
                    }
                }
                _ => bail!("Recovery provider kind/payload/guarantee mismatch; source retained"),
            }
        }
        Ok(())
    }
    pub fn into_lm(self) -> Result<(Binding, Snapshot, Intent)> {
        if self.providers.len() != 1 {
            bail!(
                "Multiple recovery providers cannot be projected into LM Studio; all recovery retained"
            );
        }
        let Entry {
            binding, payload, ..
        } = self
            .providers
            .into_iter()
            .next()
            .context("Recovery provider is missing")?;
        let Payload::LMStudio(snapshot) = payload else {
            bail!("Ollama recovery cannot be projected into LM Studio; all recovery retained");
        };
        Ok((binding, snapshot, self.session.intent))
    }
}
fn read_bounded(reader: impl Read, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        bail!("Recovery journal exceeds the size limit; source retained");
    }
    Ok(bytes)
}
fn backup(path: &Path, original: &[u8]) -> Result<()> {
    let backup = path.with_extension("v2.backup.json");
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup)
    {
        Ok(mut file) => {
            file.write_all(original)?;
            file.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if read_bounded(File::open(&backup)?, MAX_BYTES)? != original {
                bail!(
                    "Recovery backup contains different source data; authoritative journal retained"
                );
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
pub fn load(path: &Path, config: &Config) -> Result<Option<Journal>> {
    let original = match File::open(path) {
        Ok(file) => read_bounded(file, MAX_BYTES)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let value: serde_json::Value =
        serde_json::from_slice(&original).context("Invalid recovery journal; source retained")?;
    if value.is_null() {
        return Ok(None);
    }
    let legacy = value["schema"] == 2;
    let journal = if legacy {
        let mut snapshot: Snapshot = serde_json::from_value(value.clone())
            .context("Invalid legacy recovery payload; source retained")?;
        if value.get("games").is_some_and(|games| {
            *games != serde_json::to_value(&snapshot.games).unwrap_or_default()
        }) {
            bail!("Unknown legacy recovery game fields; source retained");
        }
        snapshot.normalize_legacy(config);
        snapshot.validate_recovery()?;
        Journal::lm(
            Binding::capture(config, &snapshot)?,
            snapshot,
            Intent::Reconcile,
        )
    } else if value["schema"] == 3 {
        let journal: Journal = serde_json::from_value(value.clone())
            .context("Invalid recovery envelope; source retained")?;
        // Refuse fields ignored by a nested adapter/game decoder rather than lose them.
        if serde_json::to_value(&journal)? != value {
            bail!("Incomplete or unknown recovery fields; source retained");
        }
        journal
    } else {
        bail!("Unsupported recovery format; source retained");
    };
    journal.validate()?;
    if config.mode == "active" {
        for entry in &journal.providers {
            // Disabled/missing experimental control is a retained obligation,
            // not a reason to prevent the healthy LM provider from recovering.
            if entry.restore_complete
                || (entry.binding.kind == Kind::Ollama
                    && !config
                        .providers
                        .iter()
                        .any(|provider| provider.id() == entry.binding.id && provider.enabled()))
            {
                continue;
            }
            entry.binding.validate_route(config)?;
        }
        if legacy {
            backup(path, &original)?;
            save(path, Some(&journal))
                .context("Recovery migration could not save; original journal retained")?;
        }
    }
    Ok(Some(journal))
}
pub fn save(path: &Path, journal: Option<&Journal>) -> Result<()> {
    if let Some(journal) = journal {
        journal.validate()?;
    }
    if serde_json::to_vec_pretty(&journal)?.len().saturating_add(1) > MAX_BYTES {
        bail!("Recovery journal exceeds the size limit; recovery retained");
    }
    config::write_json(path, &journal)
}

#[cfg(test)]
pub(crate) fn ollama_fixture() -> Entry {
    use crate::ollama_contract::{Catalog, CatalogEntry, Identity, ReplayCandidate, Resident};
    let identity = Identity {
        name: "fixture-ollama:latest".into(),
        digest: "a".repeat(64),
    };
    let resident = Resident {
        identity: identity.clone(),
        context_length: Some(4096),
        expires_at: Some("1970-01-01T00:05:00Z".into()),
    };
    let catalog = Catalog(
        [(
            identity.name.clone(),
            CatalogEntry {
                identity,
                remote: false,
            },
        )]
        .into_iter()
        .collect(),
    );
    let show = serde_json::json!({"details":{"format":"gguf"}, "capabilities":["completion"],
        "model_info":{"general.architecture":"fixture", "fixture.context_length":8192}})
    .to_string();
    let original = ReplayCandidate::capture(
        &resident,
        &catalog,
        show.as_bytes(),
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(100),
        crate::ollama_expiry::Policy::AbsoluteDeadline,
    )
    .unwrap();
    Entry {
        binding: Binding {
            id: "ollama-main".into(),
            kind: Kind::Ollama,
            endpoint: "127.0.0.1:11434".into(),
            configured_endpoint: "127.0.0.1:11434".into(),
            payload_version: 1,
            guarantee: Guarantee::SupportedFields,
        },
        payload: Payload::Ollama(crate::ollama_session::Snapshot {
            version: 1,
            source_revision: crate::ollama_contract::SOURCE_REVISION.into(),
            expiry_policy: crate::ollama_expiry::POLICY.into(),
            models: vec![crate::ollama_session::Model {
                original,
                stage: crate::ollama_session::Stage::Captured,
            }],
            unload_only: vec![],
            pause_complete: false,
        }),
        restore_complete: false,
    }
}

#[cfg(test)]
mod tests {
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
                    if let config::Provider::LMStudio { connection, .. } = &mut changed.providers[0]
                    {
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
                    raw["providers"][0]["payload"]["snapshot"]["models"][0]["stage"] =
                        "unknown".into()
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
                    if let crate::config::Provider::LMStudio { enabled, .. } =
                        &mut changed.providers[0]
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
}
