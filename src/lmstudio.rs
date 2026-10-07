//! LM Studio integration: the mockable `Backend` contract and the `LMStudio`
//! client that implements it over the CLI, REST and the local WebSocket API.
//! `lm_session` owns the coordinator units built on this.

mod command;
mod diagnostics;
mod identity;
mod loadout;
mod snapshot;
mod websocket;

pub use command::run_command;
pub use diagnostics::{data_dir_writable, diagnostics, ws_log, ws_log_line};
pub(crate) use identity::resident_keys;
pub use identity::{compare_fields, resolved_key};
pub use loadout::{LoadoutOffer, Resident};
pub(crate) use loadout::{MAX_RESIDENTS, matches as loadout_matches, offer as loadout_offer};
pub use snapshot::{Model, Snapshot};

use crate::config::Config;
use anyhow::{Context, Result, bail};
use diagnostics::cli_version_tag;
use identity::identity_matches;

use serde::Serialize;
use serde_json::{Value, json};

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub use crate::provider::InferenceBusy;

pub trait Backend {
    fn snapshot(&mut self) -> Result<Snapshot>;
    fn loaded(&mut self) -> Result<Vec<Value>>;
    /// Read-only identity evidence. Unsupported backends retain strict recovery.
    fn recovery_models(&mut self) -> Result<Option<Vec<Resident>>> {
        Ok(None)
    }
    fn stop_server(&mut self) -> Result<()>;
    fn start_server(&mut self, port: u16) -> Result<()>;
    fn ensure_server(&mut self, port: u16) -> Result<()> {
        self.start_server(port)
    }
    fn unload(&mut self, id: &str) -> Result<()>;
    fn restore(&mut self, model: &Model) -> Result<()>;
    /// Re-read the live load config for a model after it has been restored,
    /// so the round-trip verify (P2-1) can field-compare it against the
    /// captured snapshot. Backends that cannot read config back report a
    /// "read-back unavailable" error for that model instead of panicking.
    fn read_config(&mut self, model: &Model) -> Result<Value>;
    fn server_state(&mut self) -> Result<Value> {
        bail!("LM Studio server verification is unavailable; recovery retained")
    }
    /// Read-back only. This must never load a missing model.
    fn verify_restored(&mut self, _model: &Model) -> Result<()> {
        bail!("LM Studio model verification is unavailable; recovery retained")
    }
    /// Select/claim the captured route without starting the service.
    fn select_control_port(&mut self, _port: u16) -> Result<()> {
        Ok(())
    }
    /// False when LM Studio's CLI cannot be found: nothing to pause, not a
    /// failure. Checked without contacting the service.
    fn installed(&mut self) -> bool {
        true
    }
    /// Model sizes reported at the latest capture, for "memory freed" text.
    /// LM Studio reports each model's file size, an approximation of its use.
    fn captured_bytes(&mut self) -> u64 {
        0
    }
}

/// Bridge existing LM transports and mocks to the neutral operation interface.
impl<B: Backend + ?Sized> Backend for &mut B {
    fn recovery_models(&mut self) -> Result<Option<Vec<Resident>>> {
        (**self).recovery_models()
    }
    fn snapshot(&mut self) -> Result<Snapshot> {
        (**self).snapshot()
    }
    fn loaded(&mut self) -> Result<Vec<Value>> {
        (**self).loaded()
    }
    fn stop_server(&mut self) -> Result<()> {
        (**self).stop_server()
    }
    fn start_server(&mut self, port: u16) -> Result<()> {
        (**self).start_server(port)
    }
    fn ensure_server(&mut self, port: u16) -> Result<()> {
        (**self).ensure_server(port)
    }
    fn unload(&mut self, id: &str) -> Result<()> {
        (**self).unload(id)
    }
    fn restore(&mut self, model: &Model) -> Result<()> {
        (**self).restore(model)
    }
    fn read_config(&mut self, model: &Model) -> Result<Value> {
        (**self).read_config(model)
    }
    fn server_state(&mut self) -> Result<Value> {
        (**self).server_state()
    }
    fn verify_restored(&mut self, model: &Model) -> Result<()> {
        (**self).verify_restored(model)
    }
    fn select_control_port(&mut self, port: u16) -> Result<()> {
        (**self).select_control_port(port)
    }
    fn installed(&mut self) -> bool {
        (**self).installed()
    }
    fn captured_bytes(&mut self) -> u64 {
        (**self).captured_bytes()
    }
}
/// Bridge existing LM transports and mocks to the neutral operation interface.
/// Legacy server/diagnostic methods remain LM-owned, never imposed on Ollama.
impl<T: Backend> crate::provider::Backend for T {
    type Snapshot = Snapshot;
    type CapturedModel = Model;
    fn kind(&self) -> crate::provider::Kind {
        crate::provider::Kind::LMStudio
    }
    fn guarantee(&self) -> crate::provider::Guarantee {
        crate::provider::Guarantee::CapturedConfiguration
    }
    fn capture(&mut self) -> Result<Snapshot> {
        self.snapshot()
    }
    fn resident_keys(&mut self) -> Result<Vec<String>> {
        resident_keys(&self.loaded()?)
    }
    fn unload_captured(&mut self, model: &Model) -> Result<()> {
        self.unload(&model.identifier)
    }
    fn restore_captured(&mut self, model: &Model, verify_raw: bool) -> Result<()> {
        self.restore(model)?;
        if verify_raw {
            compare_fields(&model.load_config, &self.read_config(model)?)?;
        }
        Ok(())
    }
}

fn capture_with_server<B: Backend, T>(
    backend: &mut B,
    temporary: bool,
    port: u16,
    capture: impl FnOnce(&mut B) -> Result<T>,
) -> Result<T> {
    if temporary && let Err(start_error) = backend.start_server(port) {
        backend
            .stop_server()
            .context("Could not clean up temporary server after a failed start")?;
        return Err(start_error);
    }
    let captured = capture(backend);
    if temporary {
        backend
            .stop_server()
            .context("Could not close temporary LM Studio server; models remain loaded")?;
    }
    captured
}
fn control_port(server: &Value, api_host: &str) -> Result<u16> {
    let running = server["running"]
        .as_bool()
        .context("Unsupported LM Studio server status")?;
    if running {
        let port = server["port"]
            .as_u64()
            .context("LM Studio server port missing")?;
        let port = u16::try_from(port).context("Invalid LM Studio server port")?;
        if port == 0 {
            bail!("Invalid LM Studio server port");
        }
        Ok(port)
    } else {
        api_host
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u16>().ok())
            .filter(|port| *port != 0)
            .context("Invalid configured control port")
    }
}
pub struct LMStudio {
    pub config: Config,
    pub lms: PathBuf,
    configured_endpoint: String,
    claims: crate::ownership::SharedClaims,
    log_folder: Option<PathBuf>,
    cli_version: std::sync::OnceLock<String>,
    captured_bytes: u64,
}
impl LMStudio {
    pub fn new(config: Config) -> Result<Self> {
        if !config.lm_enabled() {
            bail!("LM Studio provider is disabled or not configured");
        }
        let mut candidates = vec![];
        if !config.lms_path().is_empty() {
            candidates.push(PathBuf::from(config.lms_path()));
        } else {
            if let Some(paths) = std::env::var_os("PATH") {
                candidates.extend(std::env::split_paths(&paths).map(|p| p.join("lms.exe")));
            }
            for (variable, suffix) in [
                ("USERPROFILE", r".lmstudio\bin\lms.exe"),
                ("ProgramFiles", r"LM Studio\resources\app\.webpack\lms.exe"),
                (
                    "LOCALAPPDATA",
                    r"Programs\LM Studio\resources\app\.webpack\lms.exe",
                ),
            ] {
                if let Some(root) = std::env::var_os(variable) {
                    candidates.push(PathBuf::from(root).join(suffix));
                }
            }
        }
        let lms = candidates
            .into_iter()
            .find(|p| p.is_file())
            .context("Waiting for LM Studio: its CLI could not be found. Open LM Studio and install its CLI, or choose the lms executable under Advanced > LM Studio")?;
        Ok(Self {
            configured_endpoint: config.lm_endpoint().into(),
            claims: Default::default(),
            config,
            lms,
            log_folder: None,
            cli_version: std::sync::OnceLock::new(),
            captured_bytes: 0,
        })
    }
    pub fn set_log_folder(&mut self, folder: PathBuf) {
        self.log_folder = Some(folder);
    }
    pub(crate) fn use_claims(&mut self, claims: crate::ownership::SharedClaims) {
        self.claims = claims;
    }
    fn claim_control(&self, port: Option<u16>) -> Result<()> {
        if self.config.mode != "active" || !self.config.lm_enabled() {
            bail!("LM Studio control is disabled; no ownership or mutation was started");
        }
        let owner = self
            .config
            .providers
            .iter()
            .find(|provider| provider.kind() == crate::provider::Kind::LMStudio)
            .context("LM Studio provider identity is missing")?
            .id();
        let actual = port
            .map(|port| format!("127.0.0.1:{port}"))
            .unwrap_or_else(|| self.config.lm_endpoint().into());
        self.claims
            .lock()
            .map_err(|_| anyhow::anyhow!("Provider ownership state is unavailable"))?
            .claim(
                owner,
                crate::provider::Kind::LMStudio,
                &[&self.configured_endpoint, &actual],
            )
    }
    fn logged<T>(&self, step: &str, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        let result = operation();
        if let Some(folder) = &self.log_folder {
            let version = self.cli_version.get_or_init(|| {
                run_command(
                    &self.lms.to_string_lossy(),
                    &["version"],
                    Duration::from_secs(5),
                )
                .map(|b| cli_version_tag(&b))
                .unwrap_or_else(|_| "unknown".into())
            });
            // Errors are reduced to the differing field; never log transport payloads or credentials.
            let detail = result
                .as_ref()
                .err()
                .map(|e| format!("{e:#}"))
                .unwrap_or_default();
            ws_log(
                folder,
                &format!("cli:{version}"),
                step,
                result.is_ok(),
                &detail,
            );
        }
        result
    }
    /// Independent read-only probes; a stopped server is reported, never started.
    pub fn doctor(&mut self, folder: &Path) -> Value {
        fn probe<T: Serialize>(result: Result<T>) -> Value {
            match result {
                Ok(value) => json!({"ok":true,"value":value}),
                Err(e) => json!({"ok":false,"error":format!("{e:#}")}),
            }
        }
        let version = probe(
            run_command(
                &self.lms.to_string_lossy(),
                &["version"],
                Duration::from_secs(5),
            )
            .map(|b| cli_version_tag(&b)),
        );
        let server = probe(self.cli(&["server", "status", "--json"]));
        let loaded = self.loaded();
        let ws = match loaded.as_ref().ok().and_then(|m| m.first()) {
            Some(model) if server["value"]["running"] == true => probe((|| {
                let id = model["identifier"].as_str().context("Missing identifier")?;
                self.raw_config(
                    if model["type"] == "embedding" {
                        "embedding"
                    } else {
                        "llm"
                    },
                    id,
                )
                .map(|_| "compatible")
            })()),
            _ => {
                json!({"ok":null,"detail":"Not tested: requires a running server and loaded model"})
            }
        };
        json!({"cli_version":version,"server":server,"loaded_models":probe(loaded),"ws_protocol":ws,
            "data_dir":{"path":folder.to_string_lossy(),"writable":data_dir_writable(folder)},"read_only":true})
    }
    fn capture_snapshot(&mut self, loaded: Vec<Value>, server: Value) -> Result<Snapshot> {
        let mut snapshot = Snapshot {
            games: vec![],
            schema: 2,
            server: server.clone(),
            server_stopped: false,
            models: vec![],
            pause_complete: false,
        };
        if loaded.is_empty() {
            return Ok(snapshot);
        }
        if loaded.iter().any(|m| {
            m["status"].as_str().is_some_and(|s| s != "idle")
                || m["queued"].as_u64().unwrap_or(0) > 0
        }) {
            return Err(InferenceBusy.into());
        }
        let native = self.native()?;
        for info in &loaded {
            let id = info["identifier"]
                .as_str()
                .context("Missing instance identifier")?;
            let namespace = if info["type"] == "embedding" {
                "embedding"
            } else {
                "llm"
            };
            let config = self.raw_config(namespace, id)?;
            let instance = native["models"]
                .as_array()
                .context("Unexpected native inventory")?
                .iter()
                .flat_map(|m| m["loaded_instances"].as_array().into_iter().flatten())
                .find(|m| m["id"] == id)
                .context("Inventory changed during snapshot")?;
            if !instance["config"].is_object() {
                bail!("Unsupported native configuration; capture refused");
            }
            snapshot.models.push(Model {
                identifier: id.into(),
                model_key: resolved_key(info)?,
                base_key: info["modelKey"]
                    .as_str()
                    .context("Missing model key")?
                    .split('@')
                    .next()
                    .unwrap()
                    .into(),
                namespace: namespace.into(),
                ttl_ms: info["ttlMs"].as_u64(),
                load_config: config,
                native_config: instance["config"].clone(),
                stage: "planned".into(),
            });
        }
        let after = self.loaded()?;
        if after.len() != loaded.len()
            || after.iter().any(|m| {
                !loaded
                    .iter()
                    .any(|a| a["identifier"] == m["identifier"] && a["modelKey"] == m["modelKey"])
            })
        {
            bail!("Models changed during snapshot; retry later");
        }
        Ok(snapshot)
    }

    pub fn cli(&self, args: &[&str]) -> Result<Value> {
        let output = run_command(&self.lms.to_string_lossy(), args, Duration::from_secs(25))?;
        if args.contains(&"--json") {
            Ok(serde_json::from_slice(&output)?)
        } else {
            Ok(Value::Null)
        }
    }
    fn native(&self) -> Result<Value> {
        let url = format!("http://{}/api/v1/models", self.config.lm_endpoint());
        let mut request = ureq::get(&url).timeout(Duration::from_secs(10));
        if let Ok(token) = std::env::var("GAMEPAUSE_LM_API_TOKEN") {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        request
            .call()
            .context("LM Studio native API unavailable; check version/server/token")?
            .into_json()
            .map_err(Into::into)
    }
    fn verify(&self, model: &Model, info: &Value) -> Result<()> {
        if !identity_matches(model, info) {
            bail!("Model identity/quantization mismatch; recovery retained");
        }
        identity::verify_ttl(model, info)?;
        let actual = self.raw_config(&model.namespace, &model.identifier)?;
        compare_fields(&model.load_config, &actual)?;
        let native = self.native()?;
        let actual_native = native["models"]
            .as_array()
            .context("Unexpected native inventory")?
            .iter()
            .flat_map(|m| m["loaded_instances"].as_array().into_iter().flatten())
            .find(|m| m["id"] == model.identifier)
            .context("Restored instance missing")?;
        for (key, value) in model
            .native_config
            .as_object()
            .context("Invalid saved config")?
        {
            if actual_native["config"][key] != *value {
                bail!("Restored native {key} differs; recovery retained");
            }
        }
        Ok(())
    }
}

/// Parse the `lms server status --json` payload into the two diagnostics a
/// user actually cares about: is the server running, and on which port.
pub fn server_state(server: &Value) -> (bool, Option<u16>) {
    let running = server
        .get("running")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let port = server.get("port").and_then(Value::as_u64).map(|p| p as u16);
    (running, port)
}

impl Backend for LMStudio {
    fn recovery_models(&mut self) -> Result<Option<Vec<Resident>>> {
        loadout::inventory(&self.loaded()?).map(Some)
    }
    fn loaded(&mut self) -> Result<Vec<Value>> {
        self.cli(&["ps", "--json"])?
            .as_array()
            .cloned()
            .context("Unexpected lms inventory")
    }
    fn snapshot(&mut self) -> Result<Snapshot> {
        self.claim_control(None)?;
        let loaded = self.loaded()?;
        self.captured_bytes = loaded
            .iter()
            .filter_map(|model| model["sizeBytes"].as_u64())
            .fold(0, u64::saturating_add);
        let mut server = self.cli(&["server", "status", "--json"])?;
        if loaded.iter().any(|m| {
            m["status"].as_str().is_some_and(|s| s != "idle")
                || m["queued"].as_u64().unwrap_or(0) > 0
        }) {
            return Err(InferenceBusy.into());
        }
        let temporary = server["running"] != true;
        let port = control_port(&server, self.config.lm_endpoint())?;
        self.claim_control(Some(port))?;
        self.config.lm_mut()?.endpoint = format!("127.0.0.1:{port}");
        server["port"] = json!(port);
        if loaded.is_empty() {
            return self.capture_snapshot(loaded, server);
        }
        capture_with_server(self, temporary, port, |backend| {
            backend.capture_snapshot(loaded, server)
        })
    }
    fn captured_bytes(&mut self) -> u64 {
        self.captured_bytes
    }
    fn stop_server(&mut self) -> Result<()> {
        self.claim_control(None)?;
        self.cli(&["server", "stop"])?;
        Ok(())
    }
    fn start_server(&mut self, port: u16) -> Result<()> {
        self.claim_control(Some(port))?;
        self.config.lm_mut()?.endpoint = format!("127.0.0.1:{port}");
        self.cli(&["server", "start", "--port", &port.to_string()])?;
        Ok(())
    }
    fn ensure_server(&mut self, port: u16) -> Result<()> {
        self.claim_control(Some(port))?;
        let status = self.cli(&["server", "status", "--json"])?;
        if !status["running"]
            .as_bool()
            .context("Unsupported LM Studio server status")?
        {
            return self.start_server(port);
        }
        let actual = status["port"]
            .as_u64()
            .context("LM Studio server port missing")?;
        if actual != u64::from(port) {
            bail!("Server port changed; recovery retained");
        }
        self.config.lm_mut()?.endpoint = format!("127.0.0.1:{port}");
        Ok(())
    }
    fn unload(&mut self, id: &str) -> Result<()> {
        self.claim_control(None)?;
        self.cli(&["unload", id])?;
        if self.loaded()?.iter().any(|m| m["identifier"] == id) {
            bail!("Model still loaded after unload");
        }
        Ok(())
    }
    fn restore(&mut self, model: &Model) -> Result<()> {
        self.claim_control(None)?;
        let loaded = self.loaded()?;
        resident_keys(&loaded)?;
        if let Some(info) = loaded.iter().find(|m| m["identifier"] == model.identifier) {
            return self.logged(&format!("restore-verify:{}", model.identifier), || {
                self.verify(model, info)
            });
        }
        identity::prevent_duplicate_restore(model, &loaded)?;
        self.load_model(model)?;
        let loaded = self.loaded()?;
        let info = loaded
            .iter()
            .find(|m| m["identifier"] == model.identifier)
            .context("Restored model missing")?;
        self.logged(&format!("restore-verify:{}", model.identifier), || {
            self.verify(model, info)
        })
    }
    fn read_config(&mut self, model: &Model) -> Result<Value> {
        self.raw_config(&model.namespace, &model.identifier)
    }
    fn server_state(&mut self) -> Result<Value> {
        self.cli(&["server", "status", "--json"])
    }
    fn select_control_port(&mut self, port: u16) -> Result<()> {
        self.claim_control(Some(port))?;
        self.config.lm_mut()?.endpoint = format!("127.0.0.1:{port}");
        Ok(())
    }
    fn verify_restored(&mut self, model: &Model) -> Result<()> {
        let loaded = self.loaded()?;
        resident_keys(&loaded)?;
        let info = loaded
            .iter()
            .find(|info| info["identifier"] == model.identifier)
            .context("Captured model disappeared before final verification; recovery retained")?;
        self.logged(&format!("final-verify:{}", model.identifier), || {
            self.verify(model, info)
        })
    }
}

#[cfg(test)]
mod tests;
