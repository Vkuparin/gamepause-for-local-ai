use crate::{
    config::{Config, write_json},
    lmstudio::{Backend, Snapshot, compare_fields},
};
use anyhow::{Result, bail};
use std::{fs, path::PathBuf};

/// Per-step outcome of the P2-1 round-trip verify, so the dashboard and the
/// `gamepause verify` CLI can render exactly what happened at each stage
/// (capture / unload / verify-unloaded / restore / field-compare).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VerifyStep {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// The result of one round-trip verify run.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VerifyReport {
    pub steps: Vec<VerifyStep>,
    pub ok: bool,
    pub summary: String,
}

pub struct Engine<B: Backend> {
    pub backend: B,
    pub config: Config,
    pub state: Option<Snapshot>,
    pub path: PathBuf,
    pub message: String,
    pub last_error: String,
    pub disabled: bool,
    pub manual_pause: bool,
    pub remembered_games: Vec<crate::discovery::Game>,
    /// Set by the two restore call sites just before `attempt`; consumed by
    /// `attempt` to emit the explicit "Restore failed" message (P0-4). Kept
    /// Separate from `last_error` so a *pause* failure is never mislabelled as
    /// a restore failure.
    pub restore_failed: bool,
    /// Latest report from the journalled, disruptive round-trip operation.
    pub verify_report: Option<crate::engine::VerifyReport>,
    pub pause_completions: u64,
    pub restore_completions: u64,
    quiet_since: Option<f64>,
    retry_at: f64,
}

fn validate_snapshot(snapshot: &Snapshot) -> Result<()> {
    if !snapshot.server["running"].is_boolean()
        || snapshot.server["port"]
            .as_u64()
            .is_none_or(|port| port == 0 || port > 65535)
    {
        bail!("Invalid recovery server settings; recovery retained");
    }
    if snapshot.schema != 2 {
        bail!("Unsupported recovery format; preserve state.json and inspect manually");
    }
    {
        if snapshot
            .games
            .iter()
            .any(|g| g.name.is_empty() || !std::path::Path::new(&g.path).is_absolute())
        {
            bail!("Invalid recovery game location; recovery retained");
        }
        let mut identifiers = std::collections::BTreeSet::new();
        for model in &snapshot.models {
            if model.identifier.is_empty()
                || model.model_key.is_empty()
                || model.base_key.is_empty()
                || !identifiers.insert(&model.identifier)
                || !["llm", "embedding"].contains(&model.namespace.as_str())
                || !["planned", "unloading", "unloaded", "restoring", "restored"]
                    .contains(&model.stage.as_str())
                || !model.load_config["fields"].is_array()
                || !model.native_config.is_object()
            {
                bail!("Invalid recovery model; preserve state.json and inspect manually");
            }
        }
    }
    Ok(())
}

impl<B: Backend> Engine<B> {
    pub fn new(config: Config, backend: B, path: PathBuf) -> Result<Self> {
        let mut state: Option<Snapshot> = match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        if let Some(snapshot) = &mut state {
            // Older schema-2 empty snapshots omitted the port when the server was stopped.
            if snapshot.server["running"] == false
                && snapshot.models.is_empty()
                && snapshot.server.get("port").is_none()
            {
                snapshot.server["port"] = serde_json::json!(
                    config
                        .api_host
                        .rsplit_once(':')
                        .and_then(|(_, port)| port.parse::<u16>().ok())
                        .unwrap_or(1234)
                );
            }
            validate_snapshot(snapshot)?;
        }
        let remembered_games = state.as_ref().map(|s| s.games.clone()).unwrap_or_default();
        Ok(Self {
            remembered_games,
            backend,
            config,
            state,
            path,
            message: "Observing games".into(),
            last_error: String::new(),
            disabled: false,
            manual_pause: false,
            restore_failed: false,
            verify_report: None,
            pause_completions: 0,
            restore_completions: 0,
            quiet_since: None,
            retry_at: 0.,
        })
    }
    fn save(&self) -> Result<()> {
        write_json(&self.path, &self.state)
    }
    pub fn settings_changed(&mut self) {
        self.retry_at = 0.;
        self.disabled = false;
    }
    pub fn remember_games(&mut self, games: Vec<crate::discovery::Game>) -> Result<()> {
        if self.state.is_none() {
            self.remembered_games = games;
            return Ok(());
        }
        let mut changed = false;
        for game in games {
            if !self.remembered_games.iter().any(|g| {
                crate::discovery::canonical(&g.path) == crate::discovery::canonical(&game.path)
            }) {
                self.remembered_games.push(game);
                changed = true;
            }
        }
        if changed {
            self.state.as_mut().unwrap().games = self.remembered_games.clone();
            self.save()?;
        }
        Ok(())
    }
    pub fn step(&mut self, gaming: bool, now: f64, cancelled: &mut dyn FnMut() -> bool) {
        if self.disabled {
            self.message = "Detection disabled; recovery retained".into();
            return;
        }
        if self.config.mode == "observe" {
            self.message = if gaming {
                "Would pause AI: game detected"
            } else {
                "Observing games; AI unchanged"
            }
            .into();
            return;
        }
        if !self.config.automation_enabled && !self.manual_pause {
            if self.state.is_none() {
                self.message = "Automatic pausing is off".into();
                return;
            }
            if gaming {
                self.quiet_since = None;
                self.message =
                    "Automatic pausing is off; saved AI will return after the game exits".into();
                return;
            }
        }
        if gaming || self.manual_pause {
            self.quiet_since = None;
            if now >= self.retry_at {
                let result = self.pause();
                self.attempt(result, now);
            }
        } else if self.state.is_some() {
            let since = *self.quiet_since.get_or_insert(now);
            let remaining = self.config.restore_delay_seconds - (now - since);
            if remaining > 0. {
                self.message = format!("Restoring AI in {}s", remaining.ceil() as u64);
            } else if now >= self.retry_at {
                let result = self.restore(cancelled);
                self.restore_failed = true;
                self.attempt(result, now);
            }
        } else {
            self.last_error.clear();
            self.retry_at = 0.;
            self.message = "AI available".into();
        }
    }
    pub fn attempt(&mut self, result: Result<()>, now: f64) {
        // Consume the flag so it reflects "the last operation was a restore
        // attempt" and never lingers into a later pause.
        let was_restore = self.restore_failed;
        self.restore_failed = false;
        match result {
            Ok(()) => {
                self.last_error.clear();
                self.retry_at = 0.;
            }
            Err(e) => {
                self.last_error = format!("{e:#}");
                self.message = if was_restore {
                    format!("Restore failed — AI not restored: {}", self.last_error)
                } else {
                    format!("Needs attention: {}", self.last_error)
                };
                self.retry_at = now + self.config.retry_seconds;
            }
        }
    }
    pub fn pause(&mut self) -> Result<()> {
        self.pause_guarded(&mut || false)
    }
    fn pause_guarded(&mut self, cancelled: &mut dyn FnMut() -> bool) -> Result<()> {
        if self.state.as_ref().is_some_and(|s| s.pause_complete) {
            self.message = "AI paused for gaming".into();
            return Ok(());
        }
        if self.state.is_none() {
            let snapshot = self.backend.snapshot()?;
            validate_snapshot(&snapshot)?;
            self.state = Some(snapshot);
            self.state.as_mut().unwrap().games = self.remembered_games.clone();
            self.save()?;
        }
        if self.config.stop_server_during_gaming
            && (self.state.as_ref().unwrap().server["running"] == true
                || self.state.as_ref().unwrap().server_stopped)
        {
            self.state.as_mut().unwrap().server_stopped = true;
            self.save()?;
            self.backend.stop_server()?;
        }
        let count = self.state.as_ref().unwrap().models.len();
        for index in 0..count {
            if cancelled() {
                bail!("Game detected; recovery retained");
            }
            let model = self.state.as_ref().unwrap().models[index].clone();
            if ["planned", "unloading", "restoring", "restored"].contains(&model.stage.as_str()) {
                let loaded = self.backend.loaded()?;
                self.state.as_mut().unwrap().models[index].stage = "unloading".into();
                self.save()?;
                if loaded.iter().any(|m| m["identifier"] == model.identifier) {
                    self.backend.unload(&model.identifier)?;
                }
                self.state.as_mut().unwrap().models[index].stage = "unloaded".into();
                self.save()?;
            }
        }
        self.state.as_mut().unwrap().pause_complete = true;
        self.save()?;
        self.pause_completions += 1;
        self.message = "AI paused for gaming".into();
        Ok(())
    }
    pub fn restore(&mut self, cancelled: &mut dyn FnMut() -> bool) -> Result<()> {
        self.restore_checked(cancelled, false)
    }
    fn restore_checked(
        &mut self,
        cancelled: &mut dyn FnMut() -> bool,
        compare: bool,
    ) -> Result<()> {
        if self.state.is_none() {
            return Ok(());
        }
        if cancelled() {
            self.message = "Game restarted; restoration deferred".into();
            return Ok(());
        }
        self.state.as_mut().unwrap().pause_complete = false;
        self.save()?;
        let needs_server = self
            .state
            .as_ref()
            .is_some_and(|state| state.server_stopped || !state.models.is_empty());
        if needs_server {
            // Persist control-server intent so a game interrupt can close a server we opened.
            self.state.as_mut().unwrap().server_stopped = true;
            self.save()?;
            let state = self.state.as_ref().unwrap();
            self.backend
                .ensure_server(state.server["port"].as_u64().unwrap_or(1234) as u16)?;
        }
        let mut failures = Vec::new();
        for index in 0..self.state.as_ref().unwrap().models.len() {
            if cancelled() {
                self.message = "Game restarted; restoration interrupted".into();
                return Ok(());
            }
            let model = self.state.as_ref().unwrap().models[index].clone();
            if ["unloading", "unloaded", "restoring", "restored"].contains(&model.stage.as_str()) {
                self.state.as_mut().unwrap().models[index].stage = "restoring".into();
                self.save()?;
                let restored = self.backend.restore(&model).and_then(|()| {
                    if compare {
                        compare_fields(&model.load_config, &self.backend.read_config(&model)?)?;
                    }
                    Ok(())
                });
                if let Err(error) = restored {
                    failures.push(format!("{}: {error:#}", model.identifier));
                    continue;
                }
                self.state.as_mut().unwrap().models[index].stage = "restored".into();
                self.save()?;
            }
        }
        if cancelled() {
            self.message = "Game restarted; restoration interrupted".into();
            return Ok(());
        }
        if !failures.is_empty() {
            bail!("{}; recovery retained", failures.join("; "));
        }
        if self.state.as_ref().unwrap().server["running"] != true
            && self.state.as_ref().unwrap().server_stopped
        {
            self.backend.stop_server()?;
        }
        // Do not forget pending recovery until clearing the disk succeeds.
        write_json(&self.path, &Option::<Snapshot>::None)?;
        self.state = None;
        self.remembered_games.clear();
        self.quiet_since = None;
        self.restore_completions += 1;
        self.message = "AI restored".into();
        Ok(())
    }
    /// Round-trip the live models through the same durable journal as gaming.
    /// The caller supplies a fail-closed game/quit guard checked between stages.
    pub fn verify_round_trip(&mut self, cancelled: &mut dyn FnMut() -> bool) -> VerifyReport {
        let mut steps = Vec::new();
        let mut phase = "guard";
        let result = (|| -> Result<()> {
            if self.config.mode != "active"
                || self.disabled
                || self.manual_pause
                || self.state.is_some()
            {
                bail!(
                    "Verification unavailable in observe/disabled/paused mode or while recovery is pending"
                );
            }
            if cancelled() {
                bail!(
                    "Verification unavailable while a game is running or detection is unavailable"
                );
            }
            phase = "capture";
            let snapshot = self.backend.snapshot()?;
            validate_snapshot(&snapshot)?;
            // A newly created transaction is schema 2 and remains readable by older builds.
            self.state = Some(snapshot);
            self.save()?;
            steps.push(VerifyStep {
                name: "capture".into(),
                ok: true,
                detail: "Snapshot persisted before unloading".into(),
            });
            let unloaded = self.pause_guarded(cancelled);
            steps.push(VerifyStep {
                name: "unload".into(),
                ok: unloaded.is_ok(),
                detail: unloaded
                    .as_ref()
                    .err()
                    .map(|e| format!("{e:#}"))
                    .unwrap_or_else(|| "Captured models unloaded".into()),
            });
            // Even a partial unload must be recovered; retain the original failure.
            let check = if unloaded.is_ok() {
                self.backend.loaded().and_then(|models| {
                    if !models.is_empty() {
                        bail!("Models still loaded after unload");
                    }
                    Ok(())
                })
            } else {
                Ok(())
            };
            if unloaded.is_ok() {
                steps.push(VerifyStep {
                    name: "verify-unloaded".into(),
                    ok: check.is_ok(),
                    detail: check
                        .as_ref()
                        .err()
                        .map(|e| format!("{e:#}"))
                        .unwrap_or_else(|| "Inventory empty".into()),
                });
            }
            let recovered = self.restore_checked(cancelled, true).and_then(|()| {
                if self.state.is_some() {
                    bail!("Game detected; restoration deferred, recovery pending");
                }
                Ok(())
            });
            steps.push(VerifyStep {
                name: "restore".into(),
                ok: recovered.is_ok(),
                detail: recovered
                    .as_ref()
                    .err()
                    .map(|e| format!("{e:#}"))
                    .unwrap_or_else(|| "Models and original server state restored".into()),
            });
            steps.push(VerifyStep {
                name: "verify-fields".into(),
                ok: recovered.is_ok(),
                detail: recovered
                    .as_ref()
                    .err()
                    .map(|e| format!("{e:#}"))
                    .unwrap_or_else(|| {
                        "Load configuration verified before clearing recovery".into()
                    }),
            });
            unloaded?;
            check?;
            recovered
        })();
        if let Err(error) = result {
            if steps.is_empty() {
                steps.push(VerifyStep {
                    name: phase.into(),
                    ok: false,
                    detail: format!("{error:#}"),
                });
            }
            self.last_error = format!("{error:#}");
        }
        let ok = !steps.is_empty() && steps.iter().all(|s| s.ok);
        if ok {
            self.last_error.clear();
        }
        let summary = if ok {
            "Round-trip verify passed".into()
        } else {
            format!(
                "Round-trip verify failed{}: {}",
                if self.state.is_some() {
                    " — recovery pending"
                } else {
                    ""
                },
                self.last_error
            )
        };
        let report = VerifyReport { steps, ok, summary };
        self.verify_report = Some(report.clone());
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lmstudio::Model;
    use serde_json::{Value, json};
    use std::{
        collections::BTreeSet,
        sync::atomic::{AtomicU64, Ordering},
    };
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    #[derive(Clone)]
    struct Fake {
        current: BTreeSet<String>,
        running: bool,
        port: u16,
        fail_start: bool,
        events: Vec<String>,
        fail_unload: Option<String>,
        fail_restore: Option<String>,
        fail_stop: bool,
        fail_read: Option<String>,
        fail_loaded_on: Option<usize>,
        loaded_calls: usize,
        lock_after_unload: bool,
        lock_after_restore: bool,
        lock_after_stop: bool,
        held_lock: Option<std::sync::Arc<std::fs::File>>,
        journal: PathBuf,
        /// When set, `read_config` returns a value that differs from the
        /// captured `load_config` (temperature 0.999 vs 0.7), so the P2-1
        /// round-trip verify reports the exact failing field.
        mutate_read: bool,
    }
    impl Fake {
        fn new() -> Self {
            Self {
                current: ["chat".into(), "embed".into()].into_iter().collect(),
                running: true,
                port: 1234,
                fail_start: false,
                events: vec![],
                fail_unload: None,
                fail_restore: None,
                fail_stop: false,
                fail_read: None,
                fail_loaded_on: None,
                loaded_calls: 0,
                lock_after_unload: false,
                lock_after_restore: false,
                lock_after_stop: false,
                held_lock: None,
                journal: PathBuf::new(),
                mutate_read: false,
            }
        }
    }
    impl Fake {
        fn lock_journal(&mut self) {
            use std::os::windows::fs::OpenOptionsExt;
            self.held_lock = Some(std::sync::Arc::new(
                std::fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(&self.journal)
                    .unwrap(),
            ));
        }
    }
    impl Backend for Fake {
        fn snapshot(&mut self) -> Result<Snapshot> {
            self.events.push("snapshot".into());
            Ok(Snapshot {
                games: vec![],
                schema: 2,
                server: json!({"running":self.running,"port":self.port}),
                server_stopped: false,
                pause_complete: false,
                models: self
                    .current
                    .iter()
                    .map(|id| Model {
                        identifier: id.clone(),
                        base_key: id.clone(),
                        model_key: id.clone(),
                        namespace: "llm".into(),
                        ttl_ms: None,
                        load_config: json!({"fields":[{"key":"temperature","value":0.7}]}),
                        native_config: json!({}),
                        stage: "planned".into(),
                    })
                    .collect(),
            })
        }
        fn read_config(&mut self, model: &Model) -> Result<Value> {
            if !self.running || self.fail_read.as_deref() == Some(&model.identifier) {
                bail!("Configuration read failed");
            }
            // Round-trip read-back: normally identical to the captured
            // load_config; with `mutate_read` the temperature drifts so the
            // verify field-compare reports exactly that field.
            let value = if self.mutate_read { 0.999 } else { 0.7 };
            Ok(json!({"fields":[{"key":"temperature","value":value}]}))
        }
        fn loaded(&mut self) -> Result<Vec<Value>> {
            self.loaded_calls += 1;
            if self.fail_loaded_on == Some(self.loaded_calls) {
                bail!("Inventory read failed");
            }
            Ok(self
                .current
                .iter()
                .map(|id| json!({"identifier":id}))
                .collect())
        }
        fn stop_server(&mut self) -> Result<()> {
            self.events.push("stop".into());
            if self.fail_stop {
                bail!("stop failed");
            }
            self.running = false;
            if self.lock_after_stop {
                self.lock_journal();
            }
            Ok(())
        }
        fn start_server(&mut self, port: u16) -> Result<()> {
            self.events.push("start".into());
            if self.fail_start {
                bail!("Server start failed");
            }
            self.port = port;
            self.running = true;
            Ok(())
        }
        fn ensure_server(&mut self, port: u16) -> Result<()> {
            if self.running && self.port != port {
                bail!("Server port changed");
            }
            if !self.running {
                self.start_server(port)?;
            }
            Ok(())
        }
        fn unload(&mut self, id: &str) -> Result<()> {
            // The "unloading" journal-entry assertion documents the pause-flow
            // invariant. It only applies when a recovery journal actually
            // exists; a self-contained verify (P2-1) unloads without one.
            if self.journal.as_os_str().is_empty() || self.journal.exists() {
                let disk: Option<Snapshot> =
                    serde_json::from_str(&fs::read_to_string(&self.journal)?).unwrap();
                assert!(
                    disk.unwrap()
                        .models
                        .iter()
                        .any(|m| m.identifier == id && m.stage == "unloading")
                );
            }
            self.events.push(format!("unload:{id}"));
            if self.fail_unload.as_deref() == Some(id) {
                bail!("unload failed");
            }
            self.current.remove(id);
            if self.lock_after_unload {
                self.lock_journal();
            }
            Ok(())
        }
        fn restore(&mut self, m: &Model) -> Result<()> {
            if !self.running {
                bail!("REST/WS server is stopped");
            }
            self.events.push(format!("restore:{}", m.identifier));
            if self.fail_restore.as_deref() == Some(&m.identifier) {
                bail!("restore failed");
            }
            self.current.insert(m.identifier.clone());
            if self.lock_after_restore {
                self.lock_journal();
            }
            Ok(())
        }
    }
    fn engine(mut backend: Fake) -> Engine<Fake> {
        let folder = std::env::temp_dir().join(format!(
            "gamepause-engine-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        // Windows can reuse a PID from an earlier test run whose journal remains.
        // Require a fresh directory rather than accepting an existing recovery file.
        fs::create_dir(&folder).unwrap();
        backend.journal = folder.join("state.json");
        Engine::new(
            Config {
                mode: "active".into(),
                ..Default::default()
            },
            backend,
            folder.join("state.json"),
        )
        .unwrap()
    }
    #[test]
    fn both_models_and_grace() {
        let mut e = engine(Fake::new());
        e.step(true, 0., &mut || false);
        assert!(e.backend.current.is_empty());
        assert!(!e.backend.running);
        e.step(false, 2., &mut || false);
        e.step(false, 31., &mut || false);
        assert!(e.backend.current.is_empty());
        e.step(false, 32., &mut || false);
        assert_eq!(e.backend.current.len(), 2);
        assert!(e.backend.running);
        assert!(e.state.is_none());
    }
    #[test]
    fn completed_pause_does_no_io() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        let events = e.backend.events.clone();
        let bytes = fs::read(&e.path).unwrap();
        e.path = PathBuf::from(r"Z:\unwritable-test-target\state.json");
        e.pause().unwrap();
        assert_eq!(e.backend.events, events);
        assert!(!bytes.is_empty());
    }
    #[test]
    fn automation_off_does_not_start_pause_but_recovers_existing_session() {
        let mut e = engine(Fake::new());
        e.config.automation_enabled = false;
        e.step(true, 0., &mut || false);
        assert!(e.backend.events.is_empty());
        e.config.automation_enabled = true;
        e.step(true, 1., &mut || false);
        let saved = fs::read(&e.path).unwrap();
        e.config.automation_enabled = false;
        e.step(true, 200., &mut || false);
        assert_eq!(saved, fs::read(&e.path).unwrap());
        assert!(e.backend.current.is_empty());
        e.step(false, 201., &mut || false);
        e.step(false, 231., &mut || false);
        assert!(e.state.is_none());
        assert_eq!(e.backend.current.len(), 2);
    }
    #[test]
    fn abandoned_pause_error_clears_when_no_game_or_recovery_remains() {
        let mut e = engine(Fake::new());
        e.last_error = "AI is busy".into();
        e.step(false, 0., &mut || false);
        assert!(e.last_error.is_empty());
        assert_eq!(e.message, "AI available");
    }
    #[test]
    fn recovery_remembers_game_paths_across_registration_changes_and_restart() {
        let mut e = engine(Fake::new());
        let game = crate::discovery::Game::new("Custom", "Game", "Game", r"D:\Game\play.exe");
        e.remember_games(vec![game.clone()]).unwrap();
        e.pause().unwrap();
        e.remember_games(vec![]).unwrap();
        let resumed = Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
        assert_eq!(resumed.remembered_games.len(), 1);
        assert_eq!(resumed.remembered_games[0].path, game.path);
    }
    #[test]
    fn partial_unload_preserves_snapshot() {
        let mut b = Fake::new();
        b.fail_unload = Some("embed".into());
        let mut e = engine(b);
        e.step(true, 0., &mut || false);
        assert_eq!(e.backend.current.len(), 1);
        assert_eq!(e.state.as_ref().unwrap().models.len(), 2);
        e.backend.fail_unload = None;
        e.step(true, 31., &mut || false);
        assert!(e.backend.current.is_empty());
        e.restore(&mut || false).unwrap();
        assert_eq!(e.backend.current.len(), 2);
    }
    #[test]
    fn partial_restore_recovers_on_restart() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        e.backend.fail_restore = Some("embed".into());
        assert!(e.restore(&mut || false).is_err());
        assert_eq!(e.backend.current.len(), 1);
        let mut backend = e.backend.clone();
        backend.fail_restore = None;
        let mut resumed = Engine::new(e.config.clone(), backend, e.path.clone()).unwrap();
        resumed.restore(&mut || false).unwrap();
        assert_eq!(resumed.backend.current.len(), 2);
        assert!(resumed.state.is_none());
    }
    #[test]
    fn observation_never_controls_ai() {
        let mut e = engine(Fake::new());
        e.config.mode = "observe".into();
        e.step(true, 0., &mut || false);
        e.step(false, 100., &mut || false);
        assert!(e.backend.events.is_empty());
        assert!(!e.path.exists());
    }
    #[test]
    fn empty_snapshot_never_loads_default() {
        let mut b = Fake::new();
        b.current.clear();
        b.running = false;
        let mut e = engine(b);
        e.pause().unwrap();
        e.restore(&mut || false).unwrap();
        assert!(e.backend.current.is_empty());
        assert!(!e.backend.running);
        assert!(!e.backend.events.iter().any(|v| v.starts_with("restore:")));
    }
    #[test]
    fn switching_games_resets_timer() {
        let mut e = engine(Fake::new());
        e.step(true, 0., &mut || false);
        e.step(false, 1., &mut || false);
        e.step(true, 29., &mut || false);
        e.step(false, 30., &mut || false);
        e.step(false, 59., &mut || false);
        assert!(e.backend.current.is_empty());
        e.step(false, 60., &mut || false);
        assert_eq!(e.backend.current.len(), 2);
    }
    #[test]
    fn disabled_keeps_journal() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        let bytes = fs::read(&e.path).unwrap();
        e.disabled = true;
        e.step(false, 100., &mut || false);
        assert_eq!(bytes, fs::read(&e.path).unwrap());
    }
    #[test]
    fn manual_pause_holds_session() {
        let mut e = engine(Fake::new());
        e.manual_pause = true;
        e.step(false, 0., &mut || false);
        e.step(false, 100., &mut || false);
        assert!(e.backend.current.is_empty());
        e.manual_pause = false;
        e.step(false, 101., &mut || false);
        e.step(false, 131., &mut || false);
        assert_eq!(e.backend.current.len(), 2);
    }
    #[test]
    fn stop_failure_retries_safely() {
        let mut b = Fake::new();
        b.fail_stop = true;
        let mut e = engine(b);
        e.step(true, 0., &mut || false);
        assert_eq!(e.backend.current.len(), 2);
        assert!(e.state.as_ref().unwrap().server_stopped);
        e.backend.fail_stop = false;
        e.step(true, 31., &mut || false);
        assert!(e.backend.current.is_empty());
        assert_eq!(
            e.backend.events.iter().filter(|s| *s == "snapshot").count(),
            1
        );
    }
    #[test]
    fn restart_interrupts_between_models() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        let mut calls = 0;
        e.restore(&mut || {
            calls += 1;
            calls > 2
        })
        .unwrap();
        assert_eq!(e.backend.current.len(), 1);
        assert!(e.state.is_some());
        e.step(true, 10., &mut || false);
        assert!(e.backend.current.is_empty());
    }
    #[test]
    fn restart_before_restore_keeps_server_off() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        e.restore(&mut || true).unwrap();
        assert!(!e.backend.running);
        assert!(e.backend.current.is_empty());
    }
    #[test]
    fn unknown_journal_format_refused() {
        let e = engine(Fake::new());
        fs::write(
            &e.path,
            r#"{"schema":9,"server":{},"server_stopped":false,"models":[]}"#,
        )
        .unwrap();
        assert!(Engine::new(e.config, e.backend, e.path).is_err());
    }
    #[test]
    fn failed_restore_keeps_journal_and_surfaces_explicit_status() {
        // P0-4: a failed Engine::restore must (a) leave the pending journal
        // intact and resumable, and (b) surface the explicit
        // "Restore failed — AI not restored" status rather than a generic
        // error line. A *pause* failure must NOT be mislabelled as a restore
        // failure.
        let mut b = Fake::new();
        b.fail_restore = Some("embed".into());
        let mut e = engine(b);
        e.pause().unwrap();
        // First restore attempt fails on "embed"; the journal must survive.
        let result = e.restore(&mut || false);
        assert!(result.is_err());
        e.restore_failed = true;
        e.attempt(result, 100.);
        // (b) explicit, non-surprising status.
        assert!(
            e.message.starts_with("Restore failed — AI not restored"),
            "expected the explicit restore-failure status, got {:?}",
            e.message
        );
        // (a) journal retained on disk and still a valid, resumable recovery.
        let bytes = fs::read(&e.path).unwrap();
        assert!(!bytes.is_empty());
        assert!(
            e.state.is_some(),
            "state must be retained after a failed restore"
        );
        let mut resumed = Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
        assert_eq!(resumed.state.as_ref().unwrap().models.len(), 2);
        // Recovery resumes where it stopped and completes cleanly.
        resumed.backend.fail_restore = None;
        resumed.restore(&mut || false).unwrap();
        assert_eq!(resumed.backend.current.len(), 2);
        assert!(resumed.state.is_none());
    }
    #[test]
    fn pause_failure_is_not_labeled_a_restore_failure() {
        let mut b = Fake::new();
        b.fail_unload = Some("embed".into());
        let mut e = engine(b);
        // step() internally drives pause(); the failure must be reported as
        // "Needs attention", never "Restore failed".
        e.step(true, 0., &mut || false);
        assert!(
            !e.message.starts_with("Restore failed"),
            "a pause failure must not be mislabelled as a restore failure, got {:?}",
            e.message
        );
        assert!(
            e.message.starts_with("Needs attention"),
            "got {:?}",
            e.message
        );
        // A later successful operation must not leave the flag set either.
        assert!(
            !e.restore_failed,
            "restore_failed must not linger after a pause failure"
        );
    }
    #[test]
    fn invalid_stage_retains_journal_instead_of_clearing_recovery() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        e.state.as_mut().unwrap().models[0].stage = "unknown".into();
        e.save().unwrap();
        let original = fs::read(&e.path).unwrap();
        assert!(Engine::new(e.config, e.backend, e.path.clone()).is_err());
        assert_eq!(fs::read(e.path).unwrap(), original);
    }
    // P2-1 acceptance: with the mock round-tripping cleanly, verify reports
    // success and exercises every logical step.
    #[test]
    fn verify_reports_success_when_mock_round_trips() {
        let mut e = engine(Fake::new());
        let report = e.verify_round_trip(&mut || false);
        assert!(
            report.ok,
            "expected a successful round-trip, got: {}",
            report.summary
        );
        let names: Vec<&str> = report.steps.iter().map(|s| s.name.as_str()).collect();
        // Capture, unload and verify-fields are always present; the server was
        // running in the Fake, so verify-unloaded is exercised too.
        for expected in [
            "capture",
            "unload",
            "verify-unloaded",
            "restore",
            "verify-fields",
        ] {
            assert!(
                names.contains(&expected),
                "missing step {expected}; got {names:?}"
            );
        }
        // The engine also records the report for the dashboard/CLI to render.
        assert_eq!(e.verify_report.as_ref().map(|r| r.ok), Some(true));
    }
    // P2-1 acceptance: when the mock returns a mutated config on restore,
    // verify reports the EXACT failing field, not an opaque error.
    #[test]
    fn verify_reports_exact_failing_field_when_config_mutated() {
        let mut e = engine(Fake::new());
        e.backend.mutate_read = true;
        let report = e.verify_round_trip(&mut || false);
        assert!(
            !report.ok,
            "expected a failing round-trip, got: {}",
            report.summary
        );
        // The field-compare step is the one that fails…
        let failing = report.steps.iter().find(|s| !s.ok).expect("a failing step");
        assert!(
            report
                .steps
                .iter()
                .any(|s| s.name == "verify-fields" && !s.ok)
        );
        // …and it names the exact field that drifted (temperature).
        assert!(
            failing.detail.contains("temperature"),
            "detail must name the failing field, got: {:?}",
            failing.detail
        );
        assert!(
            failing.detail.contains("differs"),
            "detail should carry compare_fields' message, got: {:?}",
            failing.detail
        );
    }
    #[test]
    fn verify_guards_do_not_mutate_or_replace_recovery() {
        for mode in 0..5 {
            let mut e = engine(Fake::new());
            match mode {
                0 => e.config.mode = "observe".into(),
                1 => e.manual_pause = true,
                2 => e.disabled = true,
                3 => {
                    e.pause().unwrap();
                }
                _ => (),
            }
            let before = e.backend.events.clone();
            let disk = fs::read(&e.path).ok();
            assert!(!e.verify_round_trip(&mut || mode == 4).ok);
            assert_eq!(before, e.backend.events);
            assert_eq!(disk, fs::read(&e.path).ok());
        }
    }
    #[test]
    fn verify_restores_original_server_state_and_avoids_redundant_start() {
        for running in [false, true] {
            let mut b = Fake::new();
            b.running = running;
            let mut e = engine(b);
            e.config.stop_server_during_gaming = false;
            assert!(e.verify_round_trip(&mut || false).ok);
            assert_eq!(e.backend.running, running);
            assert_eq!(e.backend.current.len(), 2);
            assert_eq!(
                e.backend.events.iter().filter(|s| *s == "start").count(),
                usize::from(!running)
            );
        }
    }
    #[test]
    fn verify_partial_unload_and_inventory_failure_attempt_cleanup() {
        for failure in 0..3 {
            let mut b = Fake::new();
            match failure {
                0 => b.fail_unload = Some("chat".into()),
                1 => b.fail_unload = Some("embed".into()),
                _ => b.fail_loaded_on = Some(3),
            }
            let mut e = engine(b);
            assert!(!e.verify_round_trip(&mut || false).ok);
            assert_eq!(e.backend.current.len(), 2);
            assert!(
                e.state.is_none(),
                "cleanup succeeded despite original failure"
            );
        }
    }
    #[test]
    fn verify_restore_or_read_failure_recovers_other_models_and_survives_restart() {
        for read in [false, true] {
            let mut b = Fake::new();
            if read {
                b.fail_read = Some("chat".into());
            } else {
                b.fail_restore = Some("chat".into());
            }
            let mut e = engine(b);
            let report = e.verify_round_trip(&mut || false);
            assert!(!report.ok);
            assert!(report.summary.contains("recovery pending"));
            assert!(
                e.backend.current.contains("embed"),
                "later model must still recover"
            );
            let mut resumed =
                Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
            resumed.backend.fail_read = None;
            resumed.backend.fail_restore = None;
            resumed.restore(&mut || false).unwrap();
            assert!(resumed.state.is_none());
            assert_eq!(resumed.backend.current.len(), 2);
        }
    }
    #[test]
    fn verify_game_start_after_unload_retains_durable_recovery() {
        let mut e = engine(Fake::new());
        let mut calls = 0;
        assert!(
            !e.verify_round_trip(&mut || {
                calls += 1;
                calls >= 4
            })
            .ok
        );
        assert!(e.backend.current.is_empty());
        assert!(e.state.is_some());
        let mut resumed = Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
        resumed.restore(&mut || false).unwrap();
        assert_eq!(resumed.backend.current.len(), 2);
    }
    #[test]
    fn verify_cannot_unload_when_initial_journal_write_fails() {
        let mut e = engine(Fake::new());
        fs::create_dir(&e.path).unwrap();
        assert!(!e.verify_round_trip(&mut || false).ok);
        assert_eq!(e.backend.events, vec!["snapshot"]);
        assert_eq!(e.backend.current.len(), 2);
    }
    #[test]
    fn restart_recovers_each_destructive_stage() {
        for stage in ["unloading", "unloaded", "restoring", "restored"] {
            let mut e = engine(Fake::new());
            e.pause().unwrap();
            for model in &mut e.state.as_mut().unwrap().models {
                model.stage = stage.into();
            }
            e.save().unwrap();
            let mut resumed =
                Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
            resumed.restore(&mut || false).unwrap();
            assert!(resumed.state.is_none());
            assert_eq!(resumed.backend.current.len(), 2);
        }
    }
    #[test]
    fn failed_stage_and_final_writes_keep_recovery_in_memory_and_on_disk() {
        for boundary in 0..3 {
            let mut b = Fake::new();
            b.running = false;
            match boundary {
                0 => b.lock_after_unload = true,
                1 => b.lock_after_restore = true,
                _ => b.lock_after_stop = true,
            }
            let mut e = engine(b);
            assert!(!e.verify_round_trip(&mut || false).ok);
            assert!(e.state.is_some());
            e.backend.held_lock = None;
            e.backend.lock_after_unload = false;
            e.backend.lock_after_restore = false;
            e.backend.lock_after_stop = false;
            let mut resumed =
                Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
            assert!(resumed.state.is_some());
            resumed.restore(&mut || false).unwrap();
            assert!(resumed.state.is_none());
            assert_eq!(resumed.backend.current.len(), 2);
        }
    }
    #[test]
    fn interrupted_restore_of_initially_stopped_server_is_closed_on_repause() {
        let mut b = Fake::new();
        b.running = false;
        let mut e = engine(b);
        e.pause().unwrap();
        let mut calls = 0;
        e.restore(&mut || {
            calls += 1;
            calls >= 2
        })
        .unwrap();
        assert!(e.backend.running);
        e.pause().unwrap();
        assert!(!e.backend.running);
        assert!(e.backend.current.is_empty());
    }
    #[test]
    fn verify_nondefault_port_and_server_failures_remain_recoverable() {
        for failure in 0..3 {
            let mut b = Fake::new();
            b.running = false;
            b.port = 4321;
            b.fail_start = failure == 1;
            b.fail_stop = failure == 2;
            let mut e = engine(b);
            let report = e.verify_round_trip(&mut || false);
            assert_eq!(report.ok, failure == 0);
            assert_eq!(e.backend.port, 4321);
            if failure != 0 {
                assert!(e.state.is_some());
                e.backend.fail_start = false;
                e.backend.fail_stop = false;
                let mut resumed =
                    Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
                resumed.restore(&mut || false).unwrap();
                assert!(resumed.state.is_none());
            }
            assert_eq!(e.backend.current.len(), if failure == 1 { 0 } else { 2 });
        }
    }
    #[test]
    fn externally_changed_port_does_not_load_into_another_server() {
        let mut e = engine(Fake::new());
        e.pause().unwrap();
        e.backend.running = true;
        e.backend.port = 4322;
        assert!(e.restore(&mut || false).is_err());
        assert!(e.backend.current.is_empty());
        assert!(e.state.is_some());
    }
    #[test]
    fn legacy_empty_stopped_server_journal_remains_schema_two_compatible() {
        let mut e = engine(Fake::new());
        write_json(&e.path, &json!({"schema":2,"server":{"running":false},"server_stopped":false,"models":[],"pause_complete":true})).unwrap();
        e = Engine::new(e.config.clone(), e.backend.clone(), e.path.clone()).unwrap();
        assert_eq!(e.state.as_ref().unwrap().schema, 2);
        assert_eq!(e.state.as_ref().unwrap().server["port"], 1234);
        e.restore(&mut || false).unwrap();
        assert!(e.state.is_none());
    }
}
