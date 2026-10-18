//! Hermes's managed local runtime. Local evidence and HTTP stay on the worker.
mod gateway;
mod native;
mod transport;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
pub use transport::Local;

pub const ROUTE: &str = "hermes-local-runtime";
pub const MAX_MODELS: usize = 256;
pub(crate) fn local_path(path: &str) -> bool {
    use std::path::{Component, Prefix};
    std::path::Path::new(path).is_absolute()
        && matches!(std::path::Path::new(path).components().next(),Some(Component::Prefix(p)) if matches!(p.kind(),Prefix::Disk(_)|Prefix::VerbatimDisk(_)))
}

pub fn default_home() -> PathBuf {
    if let Some(home) = std::env::var_os("HERMES_HOME").filter(|v| !v.is_empty()) {
        let home = PathBuf::from(home);
        if home
            .parent()
            .is_some_and(|p| p.file_name().is_some_and(|n| n == "profiles"))
        {
            return home.parent().unwrap().parent().unwrap().to_owned();
        }
        return home;
    }
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("hermes")
}
pub fn installed(home: &str) -> bool {
    let home = if home.is_empty() {
        default_home()
    } else {
        PathBuf::from(home)
    };
    home.join("runtimes/llamacpp/server.json").is_file()
        || home.join("runtimes/llamacpp/presets.ini").is_file()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Process {
    pub pid: u32,
    pub start_time: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    pub path: String,
    pub sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub id: String,
    pub context: u64,
    pub files: Vec<FileIdentity>,
    /// Only the supported non-secret launch fields, never an entire command.
    pub settings: std::collections::BTreeMap<String, String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Original {
    pub home: String,
    pub enabled: bool,
    pub running: bool,
    pub router: Option<Process>,
    pub children: Vec<Process>,
    pub engine: Option<FileIdentity>,
    pub control_endpoint: String,
    pub router_endpoint: String,
    /// Digest of the saved model/runtime preferences, excluding enabled state.
    pub preferences_sha256: String,
    pub models: Vec<Model>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<Box<Gateway>>,
}
/// The gateway's command, working directory and environment are sealed together.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gateway {
    pub process: Process,
    pub image: FileIdentity,
    pub sealed: String,
}
impl Original {
    pub fn validate(&self) -> Result<()> {
        if let Some(gateway) = &self.gateway {
            validate_file(&gateway.image)?;
            if !self.running
                || gateway.process.pid == 0
                || gateway.process.start_time == 0
                || gateway.sealed.is_empty()
                || gateway.sealed.len() > 1024 * 1024
                || !gateway.sealed.len().is_multiple_of(2)
                || !gateway.sealed.bytes().all(|b| b.is_ascii_hexdigit())
            {
                bail!("Invalid Hermes gateway recovery; source retained");
            }
        }
        if self.home.is_empty()
            || !local_path(&self.home)
            || self.models.len() > MAX_MODELS
            || self.children.len() > MAX_MODELS
            || self.running != self.router.is_some()
            || (self.running && (!self.enabled || self.engine.is_none()))
            || (self.running && !digest(&self.preferences_sha256))
            || (!self.running && (!self.models.is_empty() || !self.children.is_empty()))
            || (!self.preferences_sha256.is_empty() && !digest(&self.preferences_sha256))
        {
            bail!("Invalid Hermes capture; recovery retained");
        }
        if self.running {
            crate::config::normalized_endpoint(&self.control_endpoint)?;
            crate::config::normalized_endpoint(&self.router_endpoint)?;
            if self.control_endpoint == self.router_endpoint {
                bail!("Hermes control and router routes must differ");
            }
        } else if !self.router_endpoint.is_empty() {
            bail!("Hermes absent runtime carries a router route");
        }
        let mut ids = std::collections::BTreeSet::new();
        for model in &self.models {
            if model.id.is_empty()
                || model.id.len() > 512
                || !ids.insert(&model.id)
                || model.context == 0
                || model.files.is_empty()
                || model.files.len() > 128
                || model.settings.len() > 32
                || model.settings.iter().any(|(k, v)| {
                    !matches!(
                        k.as_str(),
                        "--ctx-size"
                            | "--batch-size"
                            | "--ubatch-size"
                            | "--cache-type-k"
                            | "--cache-type-v"
                            | "--flash-attn"
                            | "--spec-type"
                            | "--spec-draft-n-max"
                    ) || v.len() > 1024
                })
            {
                bail!("Invalid Hermes model evidence; recovery retained");
            }
            for file in &model.files {
                validate_file(file)?;
            }
        }
        if let Some(engine) = &self.engine {
            validate_file(engine)?;
        }
        if self
            .router
            .iter()
            .chain(&self.children)
            .any(|p| p.pid == 0 || p.start_time == 0)
        {
            bail!("Invalid Hermes process evidence; recovery retained");
        }
        Ok(())
    }
}
fn digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn validate_file(file: &FileIdentity) -> Result<()> {
    if !local_path(&file.path) || !digest(&file.sha256) {
        bail!("Invalid Hermes content identity; recovery retained");
    }
    Ok(())
}
pub trait Backend {
    fn capture(&mut self) -> Result<Original>;
    fn stop(&mut self, original: &Original) -> Result<()>;
    fn verify_paused(&mut self, original: &Original) -> Result<()>;
    fn start(&mut self, original: &Original) -> Result<()>;
    fn load(&mut self, original: &Original, model: &Model) -> Result<()>;
    fn verify_restored(&mut self, original: &Original) -> Result<()>;
    fn stop_gateway(&mut self, _: &Original) -> Result<()> {
        Ok(())
    }
    fn start_gateway(&mut self, _: &Original) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
