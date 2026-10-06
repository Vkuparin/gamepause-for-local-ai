//! Provider-neutral operation contract. Associated payloads stay adapter-owned.
//! Persistence routing and independent scheduling are added by later tickets.
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct InferenceBusy;
impl std::fmt::Display for InferenceBusy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AI is busy; pause deferred until idle")
    }
}
impl std::error::Error for InferenceBusy {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    LMStudio,
    Ollama,
    /// Chosen executables stopped for gaming and relaunched afterwards.
    Process,
}
impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::LMStudio => "LM Studio",
            Self::Ollama => "Ollama",
            Self::Process => "Other AI apps",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Guarantee {
    /// Captured raw model fields, identity and original service state.
    CapturedConfiguration,
    /// Explicit limited fields; not original live settings or conversations.
    SupportedFields,
    /// Same executable, command line and working directory; nothing else.
    ProcessRelaunch,
    MonitorOnly,
}

/// No JSON inventory shape, service port, namespace, raw config, or model key
/// crosses this interface. An Ollama adapter can use its own typed payloads.
pub trait Backend {
    type Snapshot: Clone;
    type CapturedModel: Clone;

    fn kind(&self) -> Kind;
    fn guarantee(&self) -> Guarantee;
    fn capture(&mut self) -> Result<Self::Snapshot>;
    fn resident_keys(&mut self) -> Result<Vec<String>>;
    fn unload_captured(&mut self, model: &Self::CapturedModel) -> Result<()>;
    /// The adapter owns identity/config read-back and comparison. `verify_raw`
    /// requests additional diagnostics rather than weakening normal restore.
    fn restore_captured(&mut self, model: &Self::CapturedModel, verify_raw: bool) -> Result<()>;
}
