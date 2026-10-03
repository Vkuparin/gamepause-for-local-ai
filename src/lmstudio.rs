use crate::config::Config;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::os::windows::process::CommandExt;
use std::{
    net::{TcpStream, ToSocketAddrs},
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime},
};
use tungstenite::{Message, WebSocket};

#[derive(Clone, Debug, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    fn unload(&mut self, id: &str) -> Result<()>;
    fn restore(&mut self, model: &Model) -> Result<()>;
    /// Re-read the live load config for a model after it has been restored,
    /// so the round-trip verify (P2-1) can field-compare it against the
    /// captured snapshot. Backends that cannot read config back report a
    /// "read-back unavailable" error for that model instead of panicking.
    fn read_config(&mut self, model: &Model) -> Result<Value>;
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
pub struct LMStudio {
    pub config: Config,
    pub lms: PathBuf,
}
impl LMStudio {
    pub fn new(config: Config) -> Result<Self> {
        let mut candidates = vec![];
        if !config.lms_path.is_empty() {
            candidates.push(PathBuf::from(&config.lms_path));
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
        Ok(Self { config, lms })
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
            bail!("AI is busy; pause deferred until idle");
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
        let url = format!("http://{}/api/v1/models", self.config.api_host);
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
            .api_host
            .to_socket_addrs()?
            .next()
            .context("No local server address")?;
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let (mut socket, _) =
            tungstenite::client(format!("ws://{}/{namespace}", self.config.api_host), stream)
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
        let mut ws = self.socket(namespace)?;
        ws.send(Message::Text(json!({"type":"rpcCall","callId":1,"endpoint":"getLoadConfig","parameter":{"specifier":{"type":"query","query":{"identifier":id}}}}).to_string().into()))?;
        loop {
            let v = receive(&mut ws)?;
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
            let v = receive(&mut ws)?;
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
    loop {
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
impl Backend for LMStudio {
    fn loaded(&mut self) -> Result<Vec<Value>> {
        self.cli(&["ps", "--json"])?
            .as_array()
            .cloned()
            .context("Unexpected lms inventory")
    }
    fn snapshot(&mut self) -> Result<Snapshot> {
        let loaded = self.loaded()?;
        let mut server = self.cli(&["server", "status", "--json"])?;
        if loaded.is_empty() {
            return self.capture_snapshot(loaded, server);
        }
        if loaded.iter().any(|m| {
            m["status"].as_str().is_some_and(|s| s != "idle")
                || m["queued"].as_u64().unwrap_or(0) > 0
        }) {
            bail!("AI is busy; pause deferred until idle");
        }
        let temporary = server["running"] != true;
        let port = if temporary {
            self.config
                .api_host
                .rsplit_once(':')
                .and_then(|(_, p)| p.parse::<u16>().ok())
                .unwrap_or(1234)
        } else {
            server["port"]
                .as_u64()
                .context("LM Studio server port missing")? as u16
        };
        self.config.api_host = format!("127.0.0.1:{port}");
        server["port"] = json!(port);
        capture_with_server(self, temporary, port, |backend| {
            backend.capture_snapshot(loaded, server)
        })
    }
    fn stop_server(&mut self) -> Result<()> {
        self.cli(&["server", "stop"])?;
        Ok(())
    }
    fn start_server(&mut self, port: u16) -> Result<()> {
        self.config.api_host = format!("127.0.0.1:{port}");
        self.cli(&["server", "start", "--port", &port.to_string()])?;
        Ok(())
    }
    fn unload(&mut self, id: &str) -> Result<()> {
        self.cli(&["unload", id])?;
        if self.loaded()?.iter().any(|m| m["identifier"] == id) {
            bail!("Model still loaded after unload");
        }
        Ok(())
    }
    fn restore(&mut self, model: &Model) -> Result<()> {
        if let Some(info) = self
            .loaded()?
            .iter()
            .find(|m| m["identifier"] == model.identifier)
        {
            return self.verify(model, info);
        }
        self.load_model(model)?;
        let loaded = self.loaded()?;
        let info = loaded
            .iter()
            .find(|m| m["identifier"] == model.identifier)
            .context("Restored model missing")?;
        self.verify(model, info)
    }
    fn read_config(&mut self, model: &Model) -> Result<Value> {
        self.raw_config(&model.namespace, &model.identifier)
    }
}
pub fn run_command(program: &str, args: &[&str], timeout: Duration) -> Result<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .creation_flags(0x08000000)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Unable to launch local command")?;
    use std::io::Read;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    // Drain both pipes so verbose output cannot deadlock the timeout loop.
    let read = |stream: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            stream.take(4 * 1024 * 1024).read_to_end(&mut b).map(|_| b)
        })
    };
    let out = read(Box::new(stdout));
    let err = read(Box::new(stderr));
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Local command timed out");
        }
        std::thread::sleep(Duration::from_millis(40));
    };
    let output = out
        .join()
        .map_err(|_| anyhow::anyhow!("Command output reader failed"))??;
    let errors = err
        .join()
        .map_err(|_| anyhow::anyhow!("Command error reader failed"))??;
    if !status.success() {
        bail!("Local command failed: {}", String::from_utf8_lossy(&errors));
    }
    Ok(output)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct ServerOnly {
        events: Vec<&'static str>,
        fail_start: bool,
        fail_stop: bool,
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
}
