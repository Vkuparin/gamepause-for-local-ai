use super::{Backend, FileIdentity, Model, Original, Process, default_home, native};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::Read,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

const CAP: u64 = 1024 * 1024;
const READ: Duration = Duration::from_secs(10);
const CONTROL: Duration = Duration::from_secs(180);
const LOAD: Duration = Duration::from_secs(600);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Host {
    role: String,
    pid: u32,
    start_time: u64,
    host: String,
    port: u16,
    protocol_version: u32,
    token_fingerprint: String,
    #[serde(default)]
    home: String,
    profiles: Vec<String>,
}
#[derive(Deserialize)]
struct Router {
    base_url: String,
    api_key: String,
    pid: u32,
    start_time: u64,
    executable: String,
    owner_pid: u32,
    owner_start_time: u64,
}
struct Control {
    endpoint: String,
    token: String,
    owner: Process,
}
pub struct Local {
    home: PathBuf,
    host_dir: PathBuf,
    agent: ureq::Agent,
    /// Open with no write/delete sharing until restoration is verified.
    pins: BTreeMap<String, (File, String)>,
    claims: crate::ownership::SharedClaims,
    id: String,
    excluded: Vec<String>,
    routes: BTreeSet<String>,
}
fn read(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(CAP + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > CAP {
        bail!("Hermes metadata exceeds bounds");
    }
    Ok(bytes)
}
fn string(path: &Path) -> Result<String> {
    String::from_utf8(read(path)?).context("Invalid Hermes metadata encoding")
}
fn endpoint(url: &str) -> Result<String> {
    let endpoint = url
        .strip_prefix("http://")
        .context("Hermes requires local HTTP")?
        .strip_suffix("/v1")
        .context("Invalid Hermes router route")?;
    crate::config::normalized_endpoint(endpoint)
}
fn host_directory() -> PathBuf {
    if let Some(path) = std::env::var_os("HERMES_GATEWAY_LOCK_DIR") {
        return path.into();
    }
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join(".local/state")
        });
    root.join("hermes/gateway-locks")
}
impl Local {
    pub fn new(home: &str, id: &str) -> Result<Self> {
        let home = if home.is_empty() {
            default_home()
        } else {
            home.into()
        };
        if !super::local_path(&home.to_string_lossy()) {
            bail!("Hermes data directory must be absolute");
        }
        Ok(Self {
            home,
            host_dir: host_directory(),
            id: id.into(),
            pins: BTreeMap::new(),
            excluded: vec![],
            routes: BTreeSet::new(),
            claims: Default::default(),
            agent: ureq::AgentBuilder::new()
                .try_proxy_from_env(false)
                .redirects(0)
                .timeout(READ)
                .build(),
        })
    }
    pub fn exclude_apps(&mut self, paths: Vec<String>) {
        self.excluded = paths;
    }
    pub fn check_process_overlap(&self) -> Result<()> {
        if self.excluded.is_empty() {
            return Ok(());
        }
        if let Some(router) = self.router()? {
            let owner_path = native::Held::recorded(router.owner_pid, router.owner_start_time)
                .and_then(|p| p.path())
                .ok();
            if self.excluded.iter().any(|p| {
                crate::discovery::same_path(p, &router.executable)
                    || owner_path
                        .as_ref()
                        .is_some_and(|owner| crate::discovery::same_path(p, owner))
            }) {
                bail!(
                    "Hermes runtime or backend is also chosen under Other AI apps; remove duplicate control"
                );
            }
        }
        Ok(())
    }
    pub fn diagnostics(&self) -> Result<Value> {
        let live = self.live_router()?;
        let control = self.control(
            live.as_ref()
                .map(|(r, _)| (r.owner_pid, r.owner_start_time)),
        )?;
        let enabled = control
            .as_ref()
            .map(|c| self.configuration(c).map(|v| v.0))
            .transpose()?;
        let residents = live
            .as_ref()
            .map(|(r, _)| {
                self.inventory(r).map(|rows| {
                    rows.iter()
                        .filter(|r| {
                            matches!(r["status"]["value"].as_str(), Some("loaded" | "ready"))
                        })
                        .count()
                })
            })
            .transpose()?;
        Ok(
            json!({"runtime":{"ok":true,"value":{"running":live.is_some(),"enabled":enabled,"resident_models":residents,"native_control":control.is_some()}},"compatibility":"Managed default-profile runtime; live restore compatibility depends on verified fields","detail":"Read-only; does not start Hermes or change model residency."}),
        )
    }
    pub(super) fn request(
        &self,
        address: &str,
        token: &str,
        path: &str,
        body: Option<Value>,
        timeout: Duration,
    ) -> Result<Value> {
        let address = crate::config::normalized_endpoint(address)?;
        let url = format!("http://{address}{path}");
        let method = if body.is_some() { "POST" } else { "GET" };
        let req = self
            .agent
            .request(method, &url)
            .timeout(timeout)
            .set("Authorization", &format!("Bearer {token}"))
            .set("X-Hermes-Session-Token", token);
        let response = match body {
            Some(body) => req.send_json(body),
            None => req.call(),
        }
        // Vendor errors can contain secrets or full start commands.
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => anyhow::anyhow!("Hermes HTTP {code}"),
            _ => anyhow::anyhow!("Hermes connection failed or timed out"),
        })?;
        if response.status() != 200 {
            bail!("Unexpected Hermes HTTP status");
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(CAP + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow::anyhow!("Hermes response read failed"))?;
        if bytes.len() as u64 > CAP {
            bail!("Hermes response exceeds bounds");
        }
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("Invalid Hermes response"))
    }
    fn router(&self) -> Result<Option<Router>> {
        let path = self.home.join("runtimes/llamacpp/server.json");
        if !path.exists() {
            return Ok(None);
        }
        let r: Router =
            serde_json::from_slice(&read(&path)?).context("Unsupported Hermes router record")?;
        endpoint(&r.base_url)?;
        if r.api_key.len() < 16 || r.api_key.len() > 1024 || !super::local_path(&r.executable) {
            bail!("Invalid Hermes router authority");
        }
        Ok(Some(r))
    }
    fn live_router(&self) -> Result<Option<(Router, native::Held)>> {
        let Some(router) = self.router()? else {
            return Ok(None);
        };
        // A live reused PID still must pass the recorded creation-time check.
        let Some(held) = native::Held::live_recorded(router.pid, router.start_time)? else {
            return Ok(None);
        };
        if !crate::discovery::same_path(&held.path()?, &router.executable)
            || !crate::discovery::inside(&router.executable, &self.home.to_string_lossy())
        {
            bail!("Hermes router executable identity changed");
        }
        let address = endpoint(&router.base_url)?;
        if native::listeners(&address)? != BTreeSet::from([router.pid]) {
            bail!("Hermes router endpoint is owned by another process");
        }
        Ok(Some((router, held)))
    }
    fn control(&self, required_owner: Option<(u32, u64)>) -> Result<Option<Control>> {
        let mut choices = Vec::new();
        for role in ["desktop-serve", "serve"] {
            let path = self.host_dir.join(format!("host-{role}.json"));
            if !path.exists() {
                continue;
            }
            let record: Host = serde_json::from_slice(&read(&path)?)
                .context("Unsupported Hermes control record")?;
            if let Some((pid, start)) = required_owner
                && (record.pid != pid || record.start_time != start)
            {
                continue;
            }
            let Some(held) = native::Held::live_recorded(record.pid, record.start_time)? else {
                continue;
            };
            if record.role != role || record.protocol_version != 1 || record.profiles != ["default"]
            {
                bail!("Hermes control requires a verified default-profile backend");
            }
            if !record.home.is_empty() {
                if !crate::discovery::same_path(&record.home, &self.home.to_string_lossy()) {
                    continue;
                }
            } else {
                let home = native::environment_home(&held.identity)?
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default())
                            .join("hermes")
                    });
                if !crate::discovery::same_path(
                    &home.to_string_lossy(),
                    &self.home.to_string_lossy(),
                ) {
                    continue;
                }
            }
            let address =
                crate::config::normalized_endpoint(&format!("{}:{}", record.host, record.port))?;
            if native::listeners(&address)? != BTreeSet::from([record.pid]) {
                bail!("Hermes control endpoint ownership changed");
            }
            let token = string(&self.host_dir.join(format!("host-{role}.token")))?
                .trim()
                .to_owned();
            if token.len() < 16
                || token.len() > 1024
                || native::hash_bytes(token.as_bytes())?[..16] != record.token_fingerprint
            {
                bail!("Hermes control token record is inconsistent");
            }
            let identity = self.request(&address, &token, "/api/host/identity", None, READ)?;
            if identity["pid"].as_u64() != Some(u64::from(record.pid))
                || identity["role"].as_str() != Some(role)
            {
                bail!("Hermes control identity does not match its owner");
            }
            choices.push(Control {
                endpoint: address,
                token,
                owner: held.identity,
            });
        }
        if choices.len() > 1 {
            bail!("More than one Hermes backend can control this runtime");
        }
        Ok(choices.pop())
    }
    fn configuration(&self, control: &Control) -> Result<(bool, String)> {
        let config = self.request(
            &control.endpoint,
            &control.token,
            "/api/config?include_defaults=false",
            None,
            READ,
        )?;
        let mut runtime = config.get("local_runtime").cloned().unwrap_or(json!({}));
        let enabled = runtime.get("enabled").map_or(Ok(false), |v| {
            v.as_bool().context("Invalid Hermes enabled setting")
        })?;
        runtime
            .as_object_mut()
            .context("Invalid Hermes runtime configuration")?
            .remove("enabled");
        let selected = json!({"model":config.get("model"), "local_runtime":runtime});
        Ok((
            enabled,
            native::hash_bytes(&serde_json::to_vec(&selected)?)?,
        ))
    }
    fn verify_preferences(&self, control: &Control, original: &Original) -> Result<bool> {
        let (enabled, preferences) = self.configuration(control)?;
        if preferences != original.preferences_sha256 {
            bail!("Hermes model or runtime preferences changed; recovery retained");
        }
        Ok(enabled)
    }
    fn identity(&mut self, path: &str) -> Result<FileIdentity> {
        if !super::local_path(path) {
            bail!("Hermes content must be a local absolute path");
        }
        let key = crate::discovery::canonical(path);
        if !self.pins.contains_key(&key) {
            let file = OpenOptions::new()
                .read(true)
                .share_mode(1)
                .open(path)
                .map_err(|_| anyhow::anyhow!("Hermes content cannot be locked for verification"))?;
            let sha256 = native::hash(&file)?;
            self.pins.insert(key.clone(), (file, sha256));
        }
        Ok(FileIdentity {
            path: path.into(),
            sha256: self.pins[&key].1.clone(),
        })
    }
    fn content(&mut self, original: &Original) -> Result<()> {
        for file in original
            .engine
            .iter()
            .chain(original.models.iter().flat_map(|m| &m.files))
        {
            if self.identity(&file.path)?.sha256 != file.sha256 {
                bail!("Hermes model or engine content changed; recovery retained");
            }
        }
        Ok(())
    }
    fn claim(
        &mut self,
        control: &Control,
        router: Option<&Router>,
        original: Option<&Original>,
    ) -> Result<()> {
        native::Held::matching(&control.owner)?.same_user()?;
        if native::listeners(&control.endpoint)? != BTreeSet::from([control.owner.pid]) {
            bail!("Hermes control owner changed before operation");
        }
        if let Some(original) = original {
            self.routes.insert(original.control_endpoint.clone());
            self.routes.insert(original.router_endpoint.clone());
        }
        self.routes.insert(control.endpoint.clone());
        let router_address = router.map(|r| endpoint(&r.base_url)).transpose()?;
        if let Some(address) = &router_address {
            self.routes.insert(address.clone());
        }
        let addresses = self.routes.iter().map(String::as_str).collect::<Vec<_>>();
        self.claims
            .lock()
            .map_err(|_| anyhow::anyhow!("Hermes ownership lock failed"))?
            .claim(&self.id, crate::provider::Kind::Hermes, &addresses)
    }
    fn inventory(&self, router: &Router) -> Result<Vec<Value>> {
        let value = self.request(
            &endpoint(&router.base_url)?,
            &router.api_key,
            "/models",
            None,
            READ,
        )?;
        let models = value["data"]
            .as_array()
            .context("Invalid Hermes model inventory")?;
        if models.len() > super::MAX_MODELS {
            bail!("Hermes model inventory exceeds bounds");
        }
        let mut names = BTreeSet::new();
        for model in models {
            let id = model["id"]
                .as_str()
                .context("Invalid Hermes model identifier")?;
            if id.is_empty() || id.len() > 512 || !names.insert(id) {
                bail!("Ambiguous Hermes model inventory");
            }
            if !matches!(
                model["status"]["value"].as_str(),
                Some("loaded" | "ready" | "unloaded")
            ) {
                bail!(crate::provider::InferenceBusy);
            }
        }
        Ok(models.clone())
    }
    fn wait_loaded(&self, router: &Router, id: &str, timeout: Duration) -> Result<()> {
        let began = std::time::Instant::now();
        loop {
            match self.inventory(router) {
                Ok(rows)
                    if rows.iter().any(|row| {
                        row["id"].as_str() == Some(id)
                            && matches!(row["status"]["value"].as_str(), Some("loaded" | "ready"))
                    }) =>
                {
                    return Ok(());
                }
                Ok(_) => {}
                Err(error) if error.is::<crate::provider::InferenceBusy>() => {}
                Err(error) => return Err(error),
            }
            if began.elapsed() >= timeout {
                bail!("Hermes model readiness timed out; recovery retained");
            }
            std::thread::sleep(
                Duration::from_millis(250).min(timeout - began.elapsed().min(timeout)),
            );
        }
    }
    fn idle(&self, router: &Router, id: &str) -> Result<()> {
        let query = encode(id);
        let slots = self.request(
            &endpoint(&router.base_url)?,
            &router.api_key,
            &format!("/slots?model={query}"),
            None,
            READ,
        )?;
        let slots = slots
            .as_array()
            .context("Hermes busy evidence is unavailable")?;
        if slots.is_empty()
            || slots.len() > 256
            || slots
                .iter()
                .any(|s| s["is_processing"].as_bool() != Some(false))
        {
            bail!(crate::provider::InferenceBusy);
        }
        // /slots alone does not account for queued work. Hermes's router metrics
        // use JSON only for slots, so obtain the bounded text separately.
        let response = self
            .agent
            .get(&format!(
                "http://{}/metrics?model={query}",
                endpoint(&router.base_url)?
            ))
            .set("Authorization", &format!("Bearer {}", router.api_key))
            .timeout(READ)
            .call()
            .map_err(|_| anyhow::anyhow!("Hermes queue evidence is unavailable"))?;
        if response.status() != 200 {
            bail!("Hermes queue evidence is unavailable");
        }
        let mut text = String::new();
        response
            .into_reader()
            .take(CAP + 1)
            .read_to_string(&mut text)
            .map_err(|_| anyhow::anyhow!("Invalid Hermes queue evidence"))?;
        if text.len() as u64 > CAP {
            bail!("Hermes metrics exceed bounds");
        }
        let mut processing = false;
        let mut deferred = false;
        for line in text.lines() {
            if line.starts_with("llamacpp:requests_processing ")
                || line.starts_with("llamacpp:requests_deferred ")
            {
                let n: f64 = line
                    .split_whitespace()
                    .nth(1)
                    .context("Invalid Hermes queue metric")?
                    .parse()?;
                if n != 0.0 {
                    bail!(crate::provider::InferenceBusy);
                }
                processing |= line.starts_with("llamacpp:requests_processing ");
                deferred |= line.starts_with("llamacpp:requests_deferred ");
            }
        }
        if !processing || !deferred {
            bail!("Hermes queue evidence is incomplete");
        }
        Ok(())
    }
    fn model(&mut self, router: &Router, row: &Value) -> Result<Model> {
        let id = row["id"]
            .as_str()
            .context("Invalid Hermes model identifier")?;
        let args = row["status"]["args"]
            .as_array()
            .context("Hermes live model settings are unavailable")?;
        if args.len() > 256 {
            bail!("Hermes live settings exceed bounds");
        }
        let mut settings = BTreeMap::new();
        let mut files = Vec::new();
        for pair in args.windows(2) {
            let Some(flag) = pair[0].as_str() else {
                bail!("Invalid Hermes live argument");
            };
            let Some(value) = pair[1].as_str() else {
                bail!("Invalid Hermes live argument");
            };
            if matches!(flag, "--model" | "--mmproj" | "--model-draft") {
                files.push(self.identity(value)?);
                if flag == "--model" {
                    self.split_files(value, &mut files)?;
                }
            }
            if matches!(
                flag,
                "--ctx-size"
                    | "--batch-size"
                    | "--ubatch-size"
                    | "--cache-type-k"
                    | "--cache-type-v"
                    | "--flash-attn"
                    | "--spec-type"
                    | "--spec-draft-n-max"
            ) && settings.insert(flag.into(), value.into()).is_some()
            {
                bail!("Ambiguous Hermes live settings");
            }
        }
        let props = self.request(
            &endpoint(&router.base_url)?,
            &router.api_key,
            &format!("/props?model={}", encode(id)),
            None,
            READ,
        )?;
        let context = props["default_generation_settings"]["n_ctx"]
            .as_u64()
            .filter(|n| *n > 0)
            .context("Hermes live context is unavailable")?;
        let primary = props["model_path"]
            .as_str()
            .context("Hermes primary model content is unavailable")?;
        if !files
            .iter()
            .any(|f| crate::discovery::same_path(&f.path, primary))
        {
            bail!("Hermes primary model content was not captured");
        }
        let model = Model {
            id: id.into(),
            context,
            files,
            settings,
        };
        if model.files.is_empty() {
            bail!("Hermes local model content is unavailable");
        }
        Ok(model)
    }
    fn split_files(&mut self, path: &str, files: &mut Vec<FileIdentity>) -> Result<()> {
        let p = Path::new(path);
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .context("Invalid Hermes model filename")?;
        if let Some((prefix, rest)) = name.rsplit_once("-00001-of-") {
            let count: usize = rest
                .strip_suffix(".gguf")
                .context("Invalid split model filename")?
                .parse()?;
            if !(1..=128).contains(&count) {
                bail!("Hermes split model exceeds bounds");
            }
            for part in 2..=count {
                let path = p.with_file_name(format!("{prefix}-{part:05}-of-{count:05}.gguf"));
                files.push(self.identity(&path.to_string_lossy())?);
            }
        }
        Ok(())
    }
    fn control_original(&self, original: &Original) -> Result<Control> {
        if !crate::discovery::same_path(&original.home, &self.home.to_string_lossy()) {
            bail!("Hermes installation changed; recovery retained");
        }
        self.control(None)?
            .context("Hermes is closed; reopen it to finish recovery")
    }
}
pub(super) fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
impl Backend for Local {
    fn capture(&mut self) -> Result<Original> {
        self.pins.clear();
        let live = self.live_router()?;
        let owner = live
            .as_ref()
            .map(|(r, _)| (r.owner_pid, r.owner_start_time));
        let control = self.control(owner)?;
        let mut original = Original {
            home: self.home.to_string_lossy().into_owned(),
            enabled: false,
            running: false,
            router: None,
            children: vec![],
            engine: None,
            control_endpoint: String::new(),
            router_endpoint: String::new(),
            preferences_sha256: String::new(),
            models: vec![],
        };
        let Some(control) = control else {
            if live.is_some() {
                bail!("Hermes runtime has no verified native control owner");
            }
            return Ok(original);
        };
        let (enabled, preferences) = self.configuration(&control)?;
        original.enabled = enabled;
        original.preferences_sha256 = preferences;
        original.control_endpoint = control.endpoint.clone();
        if let Some((router, held)) = live {
            if self
                .excluded
                .iter()
                .any(|p| crate::discovery::same_path(p, &router.executable))
            {
                bail!("Hermes engine is also chosen under Other AI apps; remove duplicate control");
            }
            if !enabled {
                bail!("Hermes running-but-disabled state cannot be restored faithfully");
            }
            self.claim(&control, Some(&router), None)?;
            original.router_endpoint = endpoint(&router.base_url)?;
            original.running = true;
            original.router = Some(held.identity.clone());
            original.children = native::tree(&held.identity, &router.executable)?;
            original.engine = Some(self.identity(&router.executable)?);
            let rows = self.inventory(&router)?;
            for row in rows
                .iter()
                .filter(|r| matches!(r["status"]["value"].as_str(), Some("loaded" | "ready")))
            {
                let id = row["id"].as_str().unwrap();
                self.idle(&router, id)?;
                original.models.push(self.model(&router, row)?);
            }
            let current = self.inventory(&router)?;
            let resident = |rows: &[Value]| {
                rows.iter()
                    .filter(|r| matches!(r["status"]["value"].as_str(), Some("loaded" | "ready")))
                    .map(|r| r["id"].as_str().unwrap().to_owned())
                    .collect::<BTreeSet<_>>()
            };
            if resident(&rows) != resident(&current) {
                bail!("Hermes residency changed during capture");
            }
        }
        original.validate()?;
        Ok(original)
    }
    fn stop(&mut self, original: &Original) -> Result<()> {
        let control = self.control_original(original)?;
        self.content(original)?;
        let enabled = self.verify_preferences(&control, original)?;
        let live = self.live_router()?;
        if let Some((router, _)) = live {
            if !enabled {
                bail!("Hermes runtime state conflicts with recovery");
            }
            self.claim(&control, Some(&router), Some(original))?;
            let rows = self.inventory(&router)?;
            for row in rows
                .iter()
                .filter(|r| matches!(r["status"]["value"].as_str(), Some("loaded" | "ready")))
            {
                let id = row["id"].as_str().unwrap();
                if !original.models.iter().any(|m| m.id == id) {
                    bail!("New Hermes models are resident; pause held");
                }
                self.idle(&router, id)?;
                let model = self.model(&router, row)?;
                if !original.models.contains(&model) {
                    bail!("Hermes resident model settings changed; pause held");
                }
            }
            self.request(
                &control.endpoint,
                &control.token,
                "/api/local-models/server",
                Some(json!({"action":"stop"})),
                CONTROL,
            )?;
        } else if enabled {
            // A lost stop can leave configuration enabled after the tree exited.
            self.claim(&control, None, Some(original))?;
            self.request(
                &control.endpoint,
                &control.token,
                "/api/local-models/server",
                Some(json!({"action":"stop"})),
                CONTROL,
            )?;
        }
        Ok(())
    }
    fn verify_paused(&mut self, original: &Original) -> Result<()> {
        let control = self.control_original(original)?;
        if self.verify_preferences(&control, original)? || self.live_router()?.is_some() {
            bail!("Hermes runtime is still enabled or running");
        }
        for process in original.router.iter().chain(&original.children) {
            if !native::gone(process)? {
                bail!("Hermes managed model process is still running");
            }
        }
        if let Some(engine) = &original.engine
            && !native::no_engine(&engine.path)?
        {
            bail!("Hermes model process absence is unverified");
        }
        Ok(())
    }
    fn start(&mut self, original: &Original) -> Result<()> {
        let control = self.control_original(original)?;
        self.content(original)?;
        let enabled = self.verify_preferences(&control, original)?;
        let live = self.live_router()?;
        self.claim(&control, live.as_ref().map(|(r, _)| r), Some(original))?;
        if live.is_none() {
            if let Some(engine) = &original.engine
                && !native::no_engine(&engine.path)?
            {
                bail!("Another server uses the saved Hermes engine; start refused");
            }
            self.request(
                &control.endpoint,
                &control.token,
                "/api/local-models/server",
                Some(json!({"action":"start"})),
                CONTROL,
            )?;
        } else if !enabled {
            bail!("Hermes enabled state conflicts with recovery");
        }
        let (router, _) = self
            .live_router()?
            .context("Hermes runtime did not start")?;
        if original.engine.as_ref() != Some(&self.identity(&router.executable)?) {
            bail!("Hermes restarted with a different engine; recovery retained");
        }
        Ok(())
    }
    fn load(&mut self, original: &Original, model: &Model) -> Result<()> {
        let control = self.control_original(original)?;
        if !self.verify_preferences(&control, original)? {
            bail!("Hermes runtime was turned off; recovery retained");
        }
        self.content(original)?;
        let (router, _) = self
            .live_router()?
            .context("Hermes runtime is unavailable")?;
        self.claim(&control, Some(&router), Some(original))?;
        let rows = self.inventory(&router)?;
        if rows.iter().any(|r| {
            matches!(r["status"]["value"].as_str(), Some("loaded" | "ready"))
                && !original
                    .models
                    .iter()
                    .any(|m| Some(m.id.as_str()) == r["id"].as_str())
        }) {
            bail!("A different Hermes loadout is resident; recovery retained");
        }
        if let Some(row) = rows.iter().find(|r| {
            r["id"].as_str() == Some(&model.id)
                && matches!(r["status"]["value"].as_str(), Some("loaded" | "ready"))
        }) {
            if self.model(&router, row)? != *model {
                bail!("Hermes live model fields differ from capture");
            }
            return Ok(());
        }
        self.request(
            &endpoint(&router.base_url)?,
            &router.api_key,
            "/models/load",
            Some(json!({"model":model.id})),
            LOAD,
        )?;
        self.wait_loaded(&router, &model.id, LOAD)
    }
    fn verify_restored(&mut self, original: &Original) -> Result<()> {
        let control = self.control_original(original)?;
        if self.verify_preferences(&control, original)? != original.enabled {
            bail!("Hermes original enabled state is not restored");
        }
        self.content(original)?;
        let (router, _) = self
            .live_router()?
            .context("Hermes restored runtime is unavailable")?;
        if original.engine.as_ref() != Some(&self.identity(&router.executable)?) {
            bail!("Hermes original engine is not restored");
        }
        let rows = self.inventory(&router)?;
        let mut current = Vec::new();
        for row in rows
            .iter()
            .filter(|r| matches!(r["status"]["value"].as_str(), Some("loaded" | "ready")))
        {
            current.push(self.model(&router, row)?);
        }
        if current.len() != original.models.len()
            || original.models.iter().any(|m| !current.contains(m))
        {
            bail!("Hermes original resident models and supported fields are not restored");
        }
        self.pins.clear();
        Ok(())
    }
}

#[cfg(test)]
mod readiness_tests {
    use super::*;
    #[test]
    fn load_acknowledgement_waits_for_inventory_readiness() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let (address, worker) = crate::ollama_session::tests::http_server(2, move |path, body| {
            assert_eq!(path, "/models");
            assert!(body.is_none());
            let status = if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                "loading"
            } else {
                "loaded"
            };
            (
                200,
                serde_json::to_vec(&json!({"data":[{"id":"fixture","status":{"value":status}}]}))
                    .unwrap(),
            )
        });
        let router = Router {
            base_url: format!("http://{address}/v1"),
            api_key: "fixture-token".into(),
            pid: 1,
            start_time: 1,
            executable: r"D:\Fixture\engine.exe".into(),
            owner_pid: 2,
            owner_start_time: 2,
        };
        let local = Local::new(r"D:\Fixture\Hermes", "hermes-fixture").unwrap();
        local
            .wait_loaded(&router, "fixture", Duration::from_secs(2))
            .unwrap();
        worker.join().unwrap();
    }
}
