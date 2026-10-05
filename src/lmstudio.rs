use crate::config::Config;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::{
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime},
};
use tungstenite::{Message, WebSocket};

pub use crate::provider::InferenceBusy;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub identifier: String,
    pub model_key: String,
    pub base_key: String,
    pub namespace: String,
    pub ttl_ms: Option<u64>,
    pub load_config: Value,
    pub native_config: Value,
    pub stage: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema: u32,
    pub server: Value,
    pub server_stopped: bool,
    pub models: Vec<Model>,
    #[serde(default)]
    pub pause_complete: bool,
    #[serde(default)]
    pub games: Vec<crate::discovery::Game>,
}
pub trait Backend {
    fn snapshot(&mut self) -> Result<Snapshot>;
    fn loaded(&mut self) -> Result<Vec<Value>>;
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
}

/// Bridge existing LM transports and mocks to the neutral operation interface.
impl<B: Backend + ?Sized> Backend for &mut B {
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

pub(crate) fn resident_keys(models: &[Value]) -> Result<Vec<String>> {
    let mut seen = std::collections::HashSet::new();
    models
        .iter()
        .map(|model| {
            let key = model["identifier"]
                .as_str()
                .filter(|key| !key.trim().is_empty())
                .context("Loaded LM model has no identifier; residency is unknown")?;
            if !seen.insert(key) {
                bail!("Duplicate loaded LM model identifier; residency is unknown");
            }
            Ok(key.to_owned())
        })
        .collect()
}

impl Model {
    pub fn recovery_key(&self) -> &str {
        &self.identifier
    }
}

impl Snapshot {
    pub fn normalize_legacy(&mut self, config: &Config) {
        // Supported schema-2 empty snapshots once omitted a stopped-server port.
        if self.server["running"] == false
            && self.models.is_empty()
            && self.server.get("port").is_none()
        {
            self.server["port"] = json!(
                config
                    .lm_endpoint()
                    .rsplit_once(':')
                    .and_then(|(_, port)| port.parse::<u16>().ok())
                    .unwrap_or(1234)
            );
        }
    }
    pub fn validate_recovery(&self) -> Result<()> {
        if !self.server["running"].is_boolean()
            || self.server["port"]
                .as_u64()
                .is_none_or(|port| port == 0 || port > 65535)
        {
            bail!("Invalid recovery server settings; recovery retained");
        }
        if self.schema != 2 {
            bail!("Unsupported recovery format; preserve state.json and inspect manually");
        }
        if self
            .games
            .iter()
            .any(|g| g.name.is_empty() || !Path::new(&g.path).is_absolute())
        {
            bail!("Invalid recovery game location; recovery retained");
        }
        let mut identifiers = std::collections::BTreeSet::new();
        for model in &self.models {
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
        Ok(())
    }
    pub fn pause_service_required(&self, stop_during_gaming: bool) -> bool {
        stop_during_gaming && (self.server["running"] == true || self.server_stopped)
    }
    /// Mutates intent only. The engine must persist it before a native operation.
    pub fn prepare_restore_service(&mut self) -> Option<u16> {
        if self.server_stopped || !self.models.is_empty() {
            self.server_stopped = true;
            Some(self.server["port"].as_u64().unwrap_or(1234) as u16)
        } else {
            None
        }
    }
    pub fn close_restored_service(&self) -> bool {
        self.server["running"] != true && self.server_stopped
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
fn cli_version_tag(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let clean = regex::Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]")
        .expect("constant ANSI pattern")
        .replace_all(&text, "")
        .into_owned();
    let lines: Vec<_> = clean
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    lines
        .iter()
        .find(|s| {
            s.starts_with("CLI commit:")
                || s.to_lowercase().starts_with("lms version")
                || s.starts_with("Version:")
        })
        .copied()
        .or_else(|| {
            if lines.len() == 1 {
                lines.first().copied()
            } else {
                None
            }
        })
        .unwrap_or("unknown")
        .chars()
        .take(200)
        .collect()
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
            .context("Waiting for LM Studio: its CLI could not be found. Open LM Studio and install its CLI, or use Locate lms in GamePause")?;
        Ok(Self {
            configured_endpoint: config.lm_endpoint().into(),
            claims: Default::default(),
            config,
            lms,
            log_folder: None,
            cli_version: std::sync::OnceLock::new(),
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
    fn socket(&self, namespace: &str) -> Result<WebSocket<TcpStream>> {
        let address = self
            .config
            .lm_endpoint()
            .to_socket_addrs()?
            .next()
            .context("No local server address")?;
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let (mut socket, _) = tungstenite::client(
            format!("ws://{}/{namespace}", self.config.lm_endpoint()),
            stream,
        )
        .map_err(|e| anyhow::anyhow!("LM Studio WebSocket handshake failed: {e}"))?;
        let stamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos();
        socket.send(Message::Text(json!({"authVersion":1,"clientIdentifier":format!("gamepause-{}-{stamp}",std::process::id()),"clientPasskey":format!("gamepause-session-{stamp}")}).to_string().into()))?;
        let reply = receive(&mut socket)?;
        if reply["success"] != true {
            bail!("LM Studio WebSocket authentication refused");
        }
        Ok(socket)
    }
    fn raw_config(&self, namespace: &str, id: &str) -> Result<Value> {
        self.logged(&format!("getLoadConfig:{namespace}:{id}"), || {
            self.raw_config_inner(namespace, id)
        })
    }
    fn raw_config_inner(&self, namespace: &str, id: &str) -> Result<Value> {
        let mut ws = self.socket(namespace)?;
        ws.send(Message::Text(json!({"type":"rpcCall","callId":1,"endpoint":"getLoadConfig","parameter":{"specifier":{"type":"query","query":{"identifier":id}}}}).to_string().into()))?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let v = receive_until(&mut ws, deadline)?;
            if v["callId"] == 1 {
                if v["type"] != "rpcResult" {
                    bail!("LM Studio rejected configuration query");
                }
                let result = v["result"].clone();
                if !result["fields"].is_array() {
                    bail!("Unsupported LM Studio load-configuration protocol");
                }
                let _ = ws.close(None);
                return Ok(result);
            }
            if v["type"] == "communicationWarning" {
                bail!("LM Studio protocol warning; compatibility check required");
            }
        }
    }
    fn load_model(&self, model: &Model) -> Result<()> {
        self.logged(
            &format!("loadModel:{}:{}", model.namespace, model.identifier),
            || self.load_model_inner(model),
        )
    }
    fn load_model_inner(&self, model: &Model) -> Result<()> {
        // The native inventory associates instances with the catalog base key.
        // Loading by a variant/file key produces an instance omitted from that API.
        // Require the original variant to remain selected, then verify the exact
        // identity after loading through the base key.
        if model.model_key.contains('@') && model.namespace == "llm" {
            let inventory = self.native()?;
            let catalog = inventory["models"]
                .as_array()
                .context("Unexpected native inventory")?
                .iter()
                .find(|m| m["key"] == model.base_key)
                .context("Saved model missing from catalog")?;
            if catalog["selected_variant"] != model.model_key {
                bail!("Saved model variant is no longer selected; recovery retained");
            }
        }
        let mut ws = self.socket(&model.namespace)?;
        ws.get_mut()
            .set_read_timeout(Some(Duration::from_secs(180)))?;
        let mut params = json!({"modelKey":model.base_key,"identifier":model.identifier,"loadConfigStack":{"layers":[{"layerName":"apiOverride","config":model.load_config}]}});
        if let Some(ttl) = model.ttl_ms {
            params["ttlMs"] = json!(ttl);
        }
        ws.send(Message::Text(json!({"type":"channelCreate","channelId":1,"endpoint":"loadModel","creationParameter":params}).to_string().into()))?;
        let started = Instant::now();
        loop {
            if started.elapsed() > Duration::from_secs(300) {
                bail!("Model load timed out; recovery retained");
            }
            let v = receive_until(&mut ws, started + Duration::from_secs(300))?;
            if v["type"] == "channelError" || v["type"] == "communicationWarning" {
                bail!(
                    "LM Studio model load failed: {}",
                    v.get("error").unwrap_or(&Value::Null)
                );
            }
            if v["type"] == "channelSend"
                && ["success", "alreadyLoaded", "loadSuccess"]
                    .contains(&v["message"]["type"].as_str().unwrap_or(""))
            {
                let identifier = v["message"]["info"]["identifier"].as_str();
                if identifier != Some(&model.identifier) {
                    bail!("Restored identifier differs; recovery retained");
                }
                let _ = ws.close(None);
                return Ok(());
            }
            if v["type"] == "channelClose" {
                bail!("Model load closed before success");
            }
        }
    }
    fn verify(&self, model: &Model, info: &Value) -> Result<()> {
        if !identity_matches(model, info) {
            bail!("Model identity/quantization mismatch; recovery retained");
        }
        if info["ttlMs"].as_u64() != model.ttl_ms {
            bail!("Idle TTL mismatch; recovery retained");
        }
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
fn receive(ws: &mut WebSocket<TcpStream>) -> Result<Value> {
    receive_until(ws, Instant::now() + Duration::from_secs(15))
}
fn receive_until(ws: &mut WebSocket<TcpStream>, deadline: Instant) -> Result<Value> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .context("LM Studio operation timed out; recovery retained")?;
        ws.get_mut()
            .set_read_timeout(Some(remaining.min(Duration::from_secs(180))))?;
        match ws.read()? {
            Message::Text(s) => return Ok(serde_json::from_str(&s)?),
            Message::Ping(bytes) => ws.send(Message::Pong(bytes))?,
            Message::Close(_) => bail!("LM Studio closed its control connection"),
            _ => {}
        }
    }
}
pub fn resolved_key(info: &Value) -> Result<String> {
    if let Some(key) = info["selectedVariant"].as_str() {
        return Ok(key.to_owned());
    }
    if let Some(key) = info["modelKey"].as_str().filter(|key| key.contains('@')) {
        return Ok(key.to_owned());
    }
    ["selectedVariant", "indexedModelIdentifier", "path"]
        .iter()
        .find_map(|k| info[*k].as_str())
        .map(str::to_owned)
        .context("Model has no reloadable identity")
}
fn identity_matches(model: &Model, info: &Value) -> bool {
    let key = info["modelKey"].as_str().unwrap_or("");
    (key == model.base_key || key == model.model_key)
        && [
            "selectedVariant",
            "modelKey",
            "indexedModelIdentifier",
            "path",
        ]
        .iter()
        .any(|field| info[*field].as_str() == Some(model.model_key.as_str()))
}
pub fn compare_fields(expected: &Value, actual: &Value) -> Result<()> {
    let saved = expected["fields"]
        .as_array()
        .context("Invalid saved load fields")?;
    let current = actual["fields"]
        .as_array()
        .context("Invalid current load fields")?;
    for field in saved {
        let key = field["key"].as_str().context("Invalid load field key")?;
        if !current
            .iter()
            .any(|f| f["key"] == key && f["value"] == field["value"])
        {
            bail!("Restored load field {key} differs; recovery retained");
        }
    }
    Ok(())
}

// ── P2-2: diagnostics core (pure, mock-free) ─────────────────────────────────
// The Doctor panel / `--doctor` assembles these from the live backend. The core
// is pure and takes fixed inputs so it is unit-testable without a live LM
// Studio (the `diagnostics()` and `data_dir_writable()` acceptance test).
/// Whether a directory is actually writable. A missing directory is *not*
/// writable (the caller must create it first); a directory that cannot be
/// opened is treated as read-only rather than an error.
pub fn data_dir_writable(path: &Path) -> bool {
    let probe = path.join(format!(".gamepause-write-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&probe)
    {
        Ok(mut f) => {
            // Writing the actual byte is the real check: a directory can be
            // openable but read-only for the write (rare, but possible on
            // OneDrive-synced or networked paths).
            let ok = f.write_all(b"gamepause").is_ok();
            let _ = f.sync_all();
            let _ = std::fs::remove_file(&probe);
            ok
        }
        Err(_) => false,
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
/// Assemble the Doctor panel body from a fixed set of inputs. `lms_version` is
/// the `lms version` output (or the error text when the CLI is unavailable),
/// `server` the `server status --json` payload, `models` the loaded instances,
/// `data_dir` the resolved data directory, and `data_dir_ok` whether it is
/// writable. Keeping this pure is what lets the acceptance test assert the exact
/// shape without touching a live LM Studio.
#[must_use]
pub fn diagnostics(
    lms_version: &str,
    server: &Value,
    models: &[Value],
    data_dir: &Path,
    data_dir_ok: bool,
) -> Value {
    let (running, port) = server_state(server);
    json!({
        "lms_version": lms_version,
        "server": {
            "running": running,
            "port": port,
            "raw": server,
        },
        "loaded_models": models.iter().map(|m| json!({
            "identifier": m["identifier"],
            "model": m.get("modelKey").cloned().or_else(|| m.get("model").cloned()),
            "status": m["status"],
        })).collect::<Vec<_>>(),
        "model_count": models.len(),
        "data_dir": {
            "path": data_dir.to_string_lossy().to_string(),
            "writable": data_dir_ok,
        }
    })
}

// ── P2-4: per-capture/restore WS protocol logging (local-only) ──────────────
// On every WS operation (loadModel / channelCreate / getLoadConfig read-back)
// GamePause records the LM Studio version and a `success` or `failed:<field>`
// line, where <field> is the field that actually diverged. The line is pure
// (testable without a live server) and the sink reuses the same local
// gamepause.log format as the panic/doctor sink — never uploaded, never
// sent to a remote service.
/// The field that a step detail names as failing, if it names one. The field
/// comparison and native-config checks phrase failures as "Restored load field
/// <name> differs" / "Restored native <name> differs", so the field is the
/// token after those markers. Returns `None` when the failure names no field
/// (identity/TTL mismatch, a missing instance, …) or when the step succeeded.
fn failing_field(detail: &str) -> Option<String> {
    for marker in ["field ", "native "] {
        if let Some(idx) = detail.find(marker) {
            let token = detail[idx + marker.len()..]
                .split(|c: char| c.is_whitespace() || c == ';')
                .next()
                .unwrap_or("");
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}
/// Build one `gamepause.log` line for a WS protocol step. `ok` is the step's
/// authoritative outcome (from `VerifyStep`), so success/failure is never
/// guessed from free text; when a step failed and its detail names a field,
/// that field is included (`failed:<field>`). The LM Studio version is always
/// present so a line can be correlated with a specific protocol implementation.
#[must_use]
pub fn ws_log_line(version: &str, step: &str, ok: bool, detail: &str) -> String {
    let outcome = if ok {
        "success".to_string()
    } else {
        match failing_field(detail) {
            Some(field) => format!("failed:{field}"),
            None => "failed".to_string(),
        }
    };
    format!(
        "ws {} lm={} {outcome}",
        step.replace(['\r', '\n', '\t'], " "),
        version.replace(['\r', '\n', '\t'], " ")
    )
}
/// Append a WS protocol line to the local `gamepause.log` under `folder`.
/// Local-only: this never transmits data and is a no-op if the directory is
/// not writable (the app is still usable; the log is best-effort).
pub fn ws_log(folder: &Path, version: &str, step: &str, ok: bool, detail: &str) {
    let line = ws_log_line(version, step, ok, detail);
    crate::app::log(folder, &line);
}
impl Backend for LMStudio {
    fn loaded(&mut self) -> Result<Vec<Value>> {
        self.cli(&["ps", "--json"])?
            .as_array()
            .cloned()
            .context("Unexpected lms inventory")
    }
    fn snapshot(&mut self) -> Result<Snapshot> {
        self.claim_control(None)?;
        let loaded = self.loaded()?;
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
        if let Some(info) = self
            .loaded()?
            .iter()
            .find(|m| m["identifier"] == model.identifier)
        {
            return self.logged(&format!("restore-verify:{}", model.identifier), || {
                self.verify(model, info)
            });
        }
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
pub fn run_command(program: &str, args: &[&str], timeout: Duration) -> Result<Vec<u8>> {
    use std::{io::Read, os::windows::io::AsRawHandle};
    use windows_sys::Win32::{Foundation::ERROR_BROKEN_PIPE, System::Pipes::PeekNamedPipe};
    const CAP: usize = 4 * 1024 * 1024;
    // Poll pipe availability on this thread: no unbounded reader joins or detached readers.
    fn drain(
        pipe: &mut (impl Read + AsRawHandle),
        bytes: &mut Vec<u8>,
        truncated: &mut bool,
    ) -> Result<bool> {
        let mut available = 0;
        if unsafe {
            PeekNamedPipe(
                pipe.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        } == 0
        {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                return Ok(true);
            }
            return Err(error.into());
        }
        if available != 0 {
            let mut buffer = [0; 8192];
            let count = pipe.read(&mut buffer[..(available as usize).min(8192)])?;
            let retained = count.min(CAP.saturating_sub(bytes.len()));
            bytes.extend_from_slice(&buffer[..retained]);
            *truncated |= retained < count;
        }
        Ok(false)
    }
    let mut child = Command::new(program)
        .args(args)
        .creation_flags(0x08000000)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Unable to launch local command")?;
    let mut stdout = child.stdout.take().context("Command stdout unavailable")?;
    let mut stderr = child.stderr.take().context("Command stderr unavailable")?;
    let deadline = Instant::now() + timeout;
    let result = (|| {
        let (mut output, mut errors) = (Vec::new(), Vec::new());
        let (mut out_done, mut err_done, mut truncated) = (false, false, false);
        loop {
            if Instant::now() >= deadline {
                bail!("Local command timed out (including pipe completion)");
            }
            if !out_done {
                out_done = drain(&mut stdout, &mut output, &mut truncated)?;
            }
            if !err_done {
                err_done = drain(&mut stderr, &mut errors, &mut truncated)?;
            }
            if let Some(status) = child.try_wait()?
                && out_done
                && err_done
            {
                if truncated {
                    bail!("Local command output exceeded 4 MiB and was truncated");
                }
                if !status.success() {
                    bail!("Local command failed: {}", String::from_utf8_lossy(&errors));
                }
                return Ok(output);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn neutral_inventory_refuses_unknown_residency() {
        assert!(resident_keys(&[]).unwrap().is_empty());
        assert_eq!(
            resident_keys(&[json!({"identifier":"chat"}), json!({"identifier":"embed"})]).unwrap(),
            vec!["chat", "embed"]
        );
        for invalid in [
            json!({}),
            json!({"identifier":3}),
            json!({"identifier":" "}),
        ] {
            assert!(resident_keys(&[json!({"identifier":"chat"}), invalid]).is_err());
        }
        assert!(
            resident_keys(&[json!({"identifier":"chat"}), json!({"identifier":"chat"})]).is_err()
        );
    }
    #[derive(Default)]
    struct ServerOnly {
        events: Vec<&'static str>,
        fail_start: bool,
        fail_stop: bool,
    }
    #[test]
    fn control_ownership_precedes_every_lm_mutation_and_survives_transport_replacement() {
        let claims =
            std::sync::Arc::new(std::sync::Mutex::new(crate::ownership::Claims::isolated()));
        claims
            .lock()
            .unwrap()
            .claim(
                "other",
                crate::provider::Kind::LMStudio,
                &["localhost:4321"],
            )
            .unwrap();
        let mut backend = LMStudio {
            configured_endpoint: "127.0.0.1:1234".into(),
            claims: claims.clone(),
            config: Config::default(),
            lms: PathBuf::new(),
            log_folder: None,
            cli_version: std::sync::OnceLock::new(),
        };
        let model = Model {
            identifier: "fixture".into(),
            model_key: "fixture/chat".into(),
            base_key: "fixture/chat".into(),
            namespace: "llm".into(),
            ttl_ms: None,
            load_config: json!({"fields":[]}),
            native_config: json!({}),
            stage: "unloaded".into(),
        };
        for result in [
            backend.snapshot().map(|_| ()),
            backend.start_server(1234),
            backend.ensure_server(1234),
            backend.stop_server(),
            backend.unload("fixture"),
            backend.restore(&model),
        ] {
            assert!(format!("{:#}", result.unwrap_err()).contains("already owned"));
        }
        // Only read-only calls could reach this invalid CLI; all mutations fail at ownership.
        assert!(backend.loaded().is_err());
        drop(backend);
        assert!(
            claims
                .lock()
                .unwrap()
                .claim(
                    "replacement",
                    crate::provider::Kind::LMStudio,
                    &["localhost:5555"]
                )
                .is_err()
        );
        claims
            .lock()
            .unwrap()
            .claim(
                "other",
                crate::provider::Kind::LMStudio,
                &["localhost:4321"],
            )
            .unwrap();
    }
    #[test]
    fn observation_refuses_control_without_claiming_the_provider() {
        let claims =
            std::sync::Arc::new(std::sync::Mutex::new(crate::ownership::Claims::isolated()));
        let backend = LMStudio {
            configured_endpoint: "127.0.0.1:1234".into(),
            claims: claims.clone(),
            config: Config {
                mode: "observe".into(),
                ..Default::default()
            },
            lms: PathBuf::new(),
            log_folder: None,
            cli_version: std::sync::OnceLock::new(),
        };
        assert!(backend.claim_control(None).is_err());
        claims
            .lock()
            .unwrap()
            .claim(
                "other",
                crate::provider::Kind::LMStudio,
                &["localhost:1234"],
            )
            .unwrap();
    }
    #[test]
    fn captured_route_selection_retains_both_claims_without_starting_a_server() {
        let claims =
            std::sync::Arc::new(std::sync::Mutex::new(crate::ownership::Claims::isolated()));
        let mut backend = LMStudio {
            configured_endpoint: "127.0.0.1:1234".into(),
            claims: claims.clone(),
            config: Config::default(),
            lms: PathBuf::new(),
            log_folder: None,
            cli_version: std::sync::OnceLock::new(),
        };
        backend.select_control_port(4321).unwrap();
        assert_eq!(backend.config.lm_endpoint(), "127.0.0.1:4321");
        backend.claim_control(None).unwrap();
        for endpoint in ["localhost:1234", "localhost:4321"] {
            assert!(
                claims
                    .lock()
                    .unwrap()
                    .claim("other", crate::provider::Kind::Ollama, &[endpoint])
                    .is_err()
            );
        }
    }
    impl Backend for ServerOnly {
        fn snapshot(&mut self) -> Result<Snapshot> {
            unreachable!()
        }
        fn loaded(&mut self) -> Result<Vec<Value>> {
            unreachable!()
        }
        fn start_server(&mut self, _: u16) -> Result<()> {
            self.events.push("start");
            if self.fail_start {
                bail!("start failed")
            };
            Ok(())
        }
        fn stop_server(&mut self) -> Result<()> {
            self.events.push("stop");
            if self.fail_stop {
                bail!("stop failed")
            };
            Ok(())
        }
        fn unload(&mut self, _: &str) -> Result<()> {
            unreachable!()
        }
        fn restore(&mut self, _: &Model) -> Result<()> {
            unreachable!()
        }
        fn read_config(&mut self, _: &Model) -> Result<Value> {
            unreachable!()
        }
    }
    #[test]
    fn temporary_capture_closes_server_on_success_and_failure() {
        let mut backend = ServerOnly::default();
        let value = capture_with_server(&mut backend, true, 1234, |b| {
            b.events.push("capture");
            Ok(42)
        })
        .unwrap();
        assert_eq!(value, 42);
        assert_eq!(backend.events, vec!["start", "capture", "stop"]);
        backend.events.clear();
        let result: Result<()> = capture_with_server(&mut backend, true, 1234, |b| {
            b.events.push("capture");
            bail!("capture failed")
        });
        assert!(result.is_err());
        assert_eq!(backend.events, vec!["start", "capture", "stop"]);
        backend.events.clear();
        capture_with_server(&mut backend, false, 1234, |b| {
            b.events.push("capture");
            Ok(())
        })
        .unwrap();
        assert_eq!(backend.events, vec!["capture"]);
    }
    #[test]
    fn temporary_server_failures_do_not_report_successful_capture() {
        let mut backend = ServerOnly {
            fail_start: true,
            ..Default::default()
        };
        assert!(capture_with_server(&mut backend, true, 1234, |_| Ok(())).is_err());
        assert_eq!(backend.events, vec!["start", "stop"]);
        backend.fail_start = false;
        backend.fail_stop = true;
        assert!(capture_with_server(&mut backend, true, 1234, |_| Ok(())).is_err());
    }
    #[test]
    fn identity_prefers_variant_then_file() {
        assert_eq!(
            resolved_key(&json!({"selectedVariant":"m@q4","path":"m"})).unwrap(),
            "m@q4"
        );
        assert_eq!(
            resolved_key(&json!({"path":"embed.gguf"})).unwrap(),
            "embed.gguf"
        );
    }
    #[test]
    fn config_comparison_ignores_order_not_values() {
        let a = json!({"fields":[{"key":"gpu","value":1},{"key":"context","value":4096}]});
        let b = json!({"fields":[{"key":"context","value":4096},{"key":"gpu","value":1}]});
        assert!(compare_fields(&a, &b).is_ok());
        let b = json!({"fields":[{"key":"gpu","value":0}]});
        assert!(compare_fields(&a, &b).is_err());
    }
    #[test]
    fn restored_variant_key_is_equivalent_but_wrong_quantization_is_not() {
        let model = Model {
            identifier: "chat".into(),
            model_key: "publisher/model@q4".into(),
            base_key: "publisher/model".into(),
            namespace: "llm".into(),
            ttl_ms: None,
            load_config: json!({"fields":[]}),
            native_config: json!({}),
            stage: "restoring".into(),
        };
        assert!(identity_matches(
            &model,
            &json!({"modelKey":"publisher/model", "selectedVariant":"publisher/model@q4"})
        ));
        assert!(identity_matches(
            &model,
            &json!({"modelKey":"publisher/model@q4", "path":"publisher/model"})
        ));
        assert!(!identity_matches(
            &model,
            &json!({"modelKey":"publisher/model", "selectedVariant":"publisher/model@q8"})
        ));
        assert_eq!(resolved_key(&json!({"modelKey":"publisher/model@q4", "indexedModelIdentifier":"publisher/model@provider/file.gguf"})).unwrap(), "publisher/model@q4");
    }

    // ── P2-2 acceptance: diagnostics() assembles the expected fields ─────────
    #[test]
    fn diagnostics_assembles_expected_fields() {
        // A running server, one loaded model, a writable data dir.
        let server = json!({"running": true, "port": 1234});
        let models =
            vec![json!({"identifier":"pub/mo@q8","modelKey":"pub/mo@q8","status":"loaded"})];
        let dir = std::env::temp_dir().join("gp_diag_ok");
        let _ = std::fs::create_dir_all(&dir);
        let ok = crate::lmstudio::data_dir_writable(&dir);
        let report = crate::lmstudio::diagnostics("v1.5.1 (x86_64)", &server, &models, &dir, ok);

        assert_eq!(report["lms_version"], "v1.5.1 (x86_64)");
        assert_eq!(report["server"]["running"], true);
        assert_eq!(report["server"]["port"], 1234);
        assert_eq!(report["model_count"], 1);
        assert_eq!(report["loaded_models"][0]["identifier"], "pub/mo@q8");
        assert_eq!(report["loaded_models"][0]["status"], "loaded");
        assert_eq!(report["data_dir"]["writable"], true);
        let _ = std::fs::remove_dir(&dir);
    }

    // The acceptance test names this explicitly: a path that is not a writable
    // directory must report false. A *file* (not a dir) is the robust,
    // OS-portable way to get that: you cannot create a probe file inside a file,
    // so the open fails and writability is false. (A missing dir fails the same
    // way.) This holds on Windows, unlike setting a read-only bit on a dir.
    #[test]
    fn data_dir_writable_is_false_for_a_non_writable_path() {
        let as_file = std::env::temp_dir().join("gp_diag_afile");
        std::fs::write(&as_file, b"not a dir").unwrap();
        assert!(
            !crate::lmstudio::data_dir_writable(&as_file),
            "a file masquerading as a data dir must not be reported writable"
        );
        // A genuinely missing directory is also not writable.
        let missing = std::env::temp_dir().join("gp_diag_definitely_missing_xyz");
        assert!(!crate::lmstudio::data_dir_writable(&missing));
        let _ = std::fs::remove_file(&as_file);
    }

    // ── P2-4: WS protocol logging — a failing field produces a line naming it ──
    // The acceptance test. Drives the REAL field-compare path (`compare_fields`,
    // the exact source of a verify-fields failure detail), extracts the failing
    // field with the REAL `failing_field`, and asserts the REAL `ws_log_line`
    // names it. So the field the user sees in the verify report is the field
    // that lands in gamepause.log.
    #[test]
    fn failing_field_produces_a_log_line_naming_that_field() {
        // A restored config that drifts on `temperature` — the same mismatch the
        // P2-1 acceptance test exercises. compare_fields reports it.
        let expected = json!({"fields":[{"key":"temperature","value":0.7}]});
        let actual = json!({"fields":[{"key":"temperature","value":0.999}]});
        // The real failure detail, exactly as verify_round_trip would carry it
        // (with a model identifier prefix, as in `format!("{id}: {err}")`).
        let err = crate::lmstudio::compare_fields(&expected, &actual).unwrap_err();
        let detail = format!("chat: {err}");
        assert!(
            detail.contains("temperature"),
            "the real detail must name the field, got: {detail:?}"
        );

        // failing_field must pull out exactly that field from the real detail.
        let field = crate::lmstudio::failing_field(&detail)
            .expect("the failing field must be extractable from the detail");
        assert_eq!(field, "temperature");

        // The log line for that step names the field and carries the LM
        // Studio version, so it can be correlated with a protocol build.
        let line = crate::lmstudio::ws_log_line("v1.5.1 (x86_64)", "verify-fields", false, &detail);
        assert!(
            line.contains("failed:temperature"),
            "log line must name the failing field, got: {line:?}"
        );
        assert!(
            line.contains("lm=v1.5.1 (x86_64)"),
            "log line must carry the LM Studio version, got: {line:?}"
        );
        assert!(
            line.starts_with("ws verify-fields "),
            "log line must identify the step, got: {line:?}"
        );
    }
    // A passing step reports `success` — the outcome comes from the step's
    // authoritative `ok`, never guessed from its detail text.
    #[test]
    fn successful_step_reports_success_regardless_of_detail() {
        let line = crate::lmstudio::ws_log_line("v1.5.1", "capture", true, "2 model(s) captured");
        assert!(
            line.ends_with("success"),
            "a successful step must report success, got: {line:?}"
        );
        assert!(
            !line.contains("failed"),
            "a success line must not say failed, got: {line:?}"
        );
    }
    // A failure that names no field (identity/TTL mismatch, missing instance)
    // still records a failure, just without a `:field` suffix.
    #[test]
    fn failure_without_a_named_field_is_still_recorded() {
        let line = crate::lmstudio::ws_log_line(
            "v1.5.1",
            "restore",
            false,
            "Model identity/quantization mismatch; recovery retained",
        );
        assert!(
            line.contains("failed"),
            "a failure must be recorded, got: {line:?}"
        );
        assert!(
            !line.contains("failed:"),
            "no field was named, so the line must not carry a fake field, got: {line:?}"
        );
    }
    // The sink writes to the local gamepause.log (best-effort) and is a no-op
    // on an unwritable directory — never an error, never a remote call.
    #[test]
    fn ws_log_is_local_only_and_best_effort() {
        let dir = std::env::temp_dir().join(format!("gp_wslog_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        crate::lmstudio::ws_log(&dir, "v1.5.1", "capture", true, "1 model(s) captured");
        let content = std::fs::read_to_string(dir.join("gamepause.log")).unwrap_or_default();
        assert!(
            content.contains("ws capture lm=v1.5.1 success"),
            "the log file must contain the ws line, got: {content:?}"
        );
        // Unwritable target (a file masquerading as a dir) must not panic.
        let as_file = std::env::temp_dir().join(format!("gp_wslog_afile_{}", std::process::id()));
        std::fs::write(&as_file, b"x").unwrap();
        crate::lmstudio::ws_log(&as_file, "v1.5.1", "restore", false, "boom");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&as_file);
    }
    #[test]
    fn websocket_ping_traffic_cannot_extend_operation_deadline() {
        use std::net::TcpListener;
        use tungstenite::protocol::Role;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let worker = std::thread::spawn(move || {
            let (peer, _) = listener.accept().unwrap();
            let mut ws = WebSocket::from_raw_socket(peer, Role::Server, None);
            let end = Instant::now() + Duration::from_millis(250);
            while Instant::now() < end {
                if ws.send(Message::Ping(vec![1].into())).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let mut ws = WebSocket::from_raw_socket(stream, Role::Client, None);
        let start = Instant::now();
        assert!(receive_until(&mut ws, start + Duration::from_millis(80)).is_err());
        assert!(start.elapsed() < Duration::from_millis(240));
        drop(ws);
        worker.join().unwrap();
    }
    #[test]
    fn protocol_operation_logging_records_failure_without_leaking_payload() {
        let mut backend = LMStudio {
            configured_endpoint: Config::default().lm_endpoint().into(),
            claims: Default::default(),
            config: Config::default(),
            lms: PathBuf::new(),
            log_folder: None,
            cli_version: std::sync::OnceLock::new(),
        };
        let folder =
            std::env::temp_dir().join(format!("gamepause-operation-log-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        backend.set_log_folder(folder.clone());
        backend.cli_version.set("test-cli".into()).unwrap();
        let result: Result<()> = backend.logged("getLoadConfig", || {
            bail!("Restored load field contextLength differs; token=secret")
        });
        assert!(result.is_err());
        let log = std::fs::read_to_string(folder.join("gamepause.log")).unwrap();
        assert!(log.contains("getLoadConfig lm=cli:test-cli failed:contextLength"));
        assert!(!log.contains("secret"));
    }
    #[test]
    fn command_timeout_includes_inherited_pipe_handles() {
        let started = Instant::now();
        let result = run_command(
            &std::env::current_exe().unwrap().to_string_lossy(),
            &[
                "--ignored",
                "--exact",
                "lmstudio::tests::command_descendant_fixture",
                "--nocapture",
            ],
            Duration::from_millis(500),
        );
        assert!(result.unwrap_err().to_string().contains("pipe completion"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    #[test]
    #[ignore = "subprocess fixture: invoked only by command timeout regression"]
    #[allow(clippy::zombie_processes)] // Deliberately outlives its parent to hold inherited pipes.
    fn command_descendant_fixture() {
        // Intentionally inherit these pipes and outlive the direct child.
        let _child = Command::new("powershell.exe")
            .creation_flags(0x08000000)
            .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 2"])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
    }
    #[test]
    fn oversized_output_is_drained_and_reported_as_truncated() {
        let result = run_command(
            &std::env::current_exe().unwrap().to_string_lossy(),
            &[
                "--ignored",
                "--exact",
                "lmstudio::tests::command_output_fixture",
                "--nocapture",
            ],
            Duration::from_secs(5),
        );
        assert!(result.unwrap_err().to_string().contains("truncated"));
    }
    #[test]
    #[ignore = "subprocess fixture: invoked only by output cap regression"]
    fn command_output_fixture() {
        std::io::stdout()
            .write_all(&vec![b'x'; 4 * 1024 * 1024 + 65536])
            .unwrap();
    }
    #[test]
    fn actual_config_transport_logs_success_and_protocol_failure() {
        use std::net::TcpListener;
        for valid in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let peer = std::thread::spawn(move || {
                let mut ws = tungstenite::accept(listener.accept().unwrap().0).unwrap();
                ws.read().unwrap(); // auth
                ws.send(Message::Text(json!({"success":true}).to_string().into()))
                    .unwrap();
                ws.read().unwrap(); // getLoadConfig
                ws.send(Message::Text(json!({"type":"rpcResult","callId":1,"result":if valid { json!({"fields":[]}) } else { json!({"unexpected":"secret"}) }}).to_string().into())).unwrap();
                let _ = ws.read();
            });
            let folder = std::env::temp_dir().join(format!(
                "gamepause-transport-{}-{valid}",
                std::process::id()
            ));
            std::fs::create_dir_all(&folder).unwrap();
            let mut config = Config::default();
            config.lm_mut().unwrap().endpoint = address.to_string();
            let backend = LMStudio {
                configured_endpoint: config.lm_endpoint().into(),
                claims: Default::default(),
                config,
                lms: PathBuf::new(),
                log_folder: Some(folder.clone()),
                cli_version: std::sync::OnceLock::new(),
            };
            backend.cli_version.set("fixture".into()).unwrap();
            assert_eq!(backend.raw_config("llm", "instance").is_ok(), valid);
            peer.join().unwrap();
            let log = std::fs::read_to_string(folder.join("gamepause.log")).unwrap();
            assert!(log.contains(&format!(
                "getLoadConfig:llm:instance lm=cli:fixture {}",
                if valid { "success" } else { "failed" }
            )));
            assert!(!log.contains("secret"));
        }
    }
    #[test]
    fn stopped_server_without_port_and_cli_banner_are_normalized() {
        assert_eq!(
            control_port(&json!({"running":false}), "127.0.0.1:4321").unwrap(),
            4321
        );
        assert!(control_port(&json!({"running":true,"port":65536}), "127.0.0.1:4321").is_err());
        assert_eq!(
            cli_version_tag(b"\x1b[31mLOGO\x1b[0m\nCLI commit: 69d945a\nDocs: ignored"),
            "CLI commit: 69d945a"
        );
    }
}
