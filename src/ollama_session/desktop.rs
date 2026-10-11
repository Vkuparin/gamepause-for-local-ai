//! Adapter-owned durable Desktop lifecycle, independent of model replay.
mod native;
#[cfg(test)]
mod tests;
use anyhow::{Result, bail};
pub(super) use native::Native;
use serde::{Deserialize, Serialize};

pub const SUPPORTED_VERSION: &str = "0.40.3";
pub const SOURCE_REVISION: &str = "eab97e9f92b9a25c2d52d2cc6c1b1c99bd9fae21";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Process {
    pub pid: u32,
    pub created_at: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    pub app: Process,
    pub server: Option<Process>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub version: String,
    pub source_revision: String,
    pub executable: String,
    pub executable_sha256: String,
    pub server_sha256: String,
    /// Per-user encrypted command, directory, environment and Desktop settings.
    pub sealed_startup: String,
    pub instance: Instance,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Captured,
    Stopping,
    Stopped,
    Starting,
    Ready,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub original: Capture,
    pub current: Instance,
    pub phase: Phase,
    /// Externally restarted instances can satisfy recovery, never new stop authority.
    pub external_restart: bool,
}
impl Snapshot {
    pub fn validate(&self) -> Result<()> {
        let c = &self.original;
        let digest =
            |value: &str| value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit());
        let process = |p: &Process| p.pid != 0 && p.created_at != 0;
        if c.version != SUPPORTED_VERSION
            || !std::path::Path::new(&c.executable).is_absolute()
            || c.executable.contains('\0')
            || c.executable.len() > 32768
            || c.source_revision != SOURCE_REVISION
            || !crate::process_session::file_name(&c.executable)
                .eq_ignore_ascii_case("ollama app.exe")
            || !digest(&c.executable_sha256)
            || !digest(&c.server_sha256)
            || c.sealed_startup.is_empty()
            || c.sealed_startup.len() > 2 * 1024 * 1024
            || !c.sealed_startup.len().is_multiple_of(2)
            || !c.sealed_startup.bytes().all(|c| c.is_ascii_hexdigit())
            || !process(&c.instance.app)
            || c.instance.server.as_ref().is_none_or(|s| !process(s))
            || !process(&self.current.app)
            || self.current.server.as_ref().is_some_and(|s| !process(s))
            || (matches!(
                self.phase,
                Phase::Captured | Phase::Stopping | Phase::Stopped | Phase::Ready
            ) && self.current.server.is_none())
        {
            bail!("Unsupported or inconsistent Ollama Desktop recovery; source retained");
        }
        Ok(())
    }
    pub fn transition(&self, new: &Self) -> Result<()> {
        if self.original != new.original {
            bail!("Original Ollama Desktop startup evidence changed; recovery retained");
        }
        new.validate()
    }
    pub fn restored(&self) -> bool {
        self.phase == Phase::Ready
    }
    pub fn note(&self) -> &'static str {
        if self.external_restart {
            "Ollama Desktop restarted externally; its current instance was kept."
        } else if self.phase == Phase::Stopped {
            "Ollama Desktop and its managed server stopped; client reloads blocked unless restarted externally."
        } else if self.phase == Phase::Ready {
            "Ollama Desktop is responding at its original endpoint."
        } else {
            "Ollama Desktop shutdown/restart protection is not yet verified."
        }
    }
}

pub(super) enum Eligibility {
    Supported(Capture),
    Fallback(String),
}
pub(super) struct Started {
    pub instance: Instance,
    pub external: bool,
}
/// Native work runs only in checkpointed provider units. No idle polling.
pub(super) trait Control: Send {
    fn capture(&mut self, endpoint: &str, version: &str) -> Result<Eligibility>;
    fn request_stop(&mut self, snapshot: &Snapshot, endpoint: &str) -> Result<()>;
    fn verify_stopped(&mut self, snapshot: &Snapshot, endpoint: &str) -> Result<()>;
    fn start(&mut self, snapshot: &Snapshot, endpoint: &str) -> Result<Started>;
    fn verify_ready(&mut self, snapshot: &Snapshot, endpoint: &str) -> Result<Started>;
}
