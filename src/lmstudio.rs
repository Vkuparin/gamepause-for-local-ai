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
        progress: &mut bool,
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
            *progress |= count != 0;
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
        // Set when the command itself has exited and its pipes were last busy.
        let mut exited_quiet: Option<Instant> = None;
        loop {
            if Instant::now() >= deadline {
                bail!("Local command timed out (including pipe completion)");
            }
            let mut progress = false;
            if !out_done {
                out_done = drain(&mut stdout, &mut output, &mut truncated, &mut progress)?;
            }
            if !err_done {
                err_done = drain(&mut stderr, &mut errors, &mut truncated, &mut progress)?;
            }
            let status = child.try_wait()?;
            // `lms` can start LM Studio, which inherits these pipes and never
            // closes them. Everything the command wrote is already buffered,
            // so once it has exited and the pipes stay quiet they count as done.
            if status.is_some() {
                if progress {
                    exited_quiet = None;
                } else if exited_quiet.get_or_insert_with(Instant::now).elapsed()
                    >= Duration::from_millis(250)
                {
                    out_done = true;
                    err_done = true;
                }
            }
            if let Some(status) = status
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
            // Keep draining while output flows; idle waits are coarse so a
            // slow command is not polled hundreds of times a second.
            if !progress {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(test)]
mod tests;
