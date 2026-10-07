//! Bounded Ollama evidence, validated candidates and limited wire construction
//! for the pinned source target, live-checked against Ollama 0.35.1.
use crate::ollama_expiry::Policy;
use anyhow::{Result, bail};
use serde::{Deserialize, de::DeserializeOwned};
use std::{collections::BTreeMap, io::Read};

pub const SOURCE_REVISION: &str = "b0c1ca4f7549d7acdfa52a7dcffc934bc63a43ce";
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_MODELS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub name: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resident {
    pub identity: Identity,
    /// Observed runner context, not an original num_ctx/parallel request.
    pub context_length: Option<u64>,
    /// Raw evidence only. No timestamp/default/indefinite policy is inferred.
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResidentInventory(pub BTreeMap<String, Resident>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub identity: Identity,
    pub remote: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Catalog(pub BTreeMap<String, CatalogEntry>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelEvidence {
    Remote,
    Unknown,
    Embedding,
    LocalCompletionCandidate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayBlock {
    Remote,
    LocalityUnknown,
    EmbeddingContractUnresolved,
    AdapterNotIntegrated,
}

impl ModelEvidence {
    /// Even the local candidate cannot authorize an unload before replay is
    /// established. Keeping this negative contract explicit prevents accidental
    /// interpretation of parsed inventory as supported control.
    pub fn replay_block(self) -> ReplayBlock {
        match self {
            Self::Remote => ReplayBlock::Remote,
            Self::Unknown => ReplayBlock::LocalityUnknown,
            Self::Embedding => ReplayBlock::EmbeddingContractUnresolved,
            Self::LocalCompletionCandidate => ReplayBlock::AdapterNotIntegrated,
        }
    }
}

#[derive(Deserialize)]
struct Envelope<T> {
    models: Vec<T>,
    error: Option<String>,
}
#[derive(Deserialize)]
struct WireIdentity {
    name: String,
    model: String,
    digest: String,
}
#[derive(Deserialize)]
struct WireResident {
    #[serde(flatten)]
    identity: WireIdentity,
    context_length: Option<u64>,
    expires_at: Option<String>,
}
#[derive(Deserialize)]
struct WireCatalogEntry {
    #[serde(flatten)]
    identity: WireIdentity,
    remote_host: Option<String>,
    remote_model: Option<String>,
}
#[derive(Deserialize, Default)]
struct Details {
    format: Option<String>,
}
#[derive(Deserialize)]
struct WireShow {
    remote_host: Option<String>,
    remote_model: Option<String>,
    capabilities: Option<Vec<String>>,
    model_info: Option<BTreeMap<String, serde_json::Value>>,
    #[serde(default)]
    details: Details,
}

fn decode<T: DeserializeOwned>(reader: impl Read) -> Result<T> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("Could not read Ollama evidence"))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        bail!("Ollama evidence exceeds response limit");
    }
    // Do not include response bodies, remote hosts, or model prompts in errors.
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("Invalid Ollama evidence JSON"))
}

fn identity(wire: WireIdentity) -> Result<Identity> {
    if wire.name != wire.model
        || wire.name.is_empty()
        || wire.name.len() > 512
        || wire
            .name
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    {
        bail!("Invalid or ambiguous Ollama model name");
    }
    let digest = wire.digest.strip_prefix("sha256:").unwrap_or(&wire.digest);
    if digest.len() != 64 || !digest.bytes().all(|c| c.is_ascii_hexdigit()) {
        bail!("Invalid Ollama model digest");
    }
    Ok(Identity {
        name: wire.name,
        digest: digest.to_ascii_lowercase(),
    })
}

fn check_count(count: usize) -> Result<()> {
    if count > MAX_MODELS {
        bail!("Ollama evidence exceeds model limit");
    }
    Ok(())
}

pub fn parse_resident_inventory(reader: impl Read) -> Result<ResidentInventory> {
    let wire: Envelope<WireResident> = decode(reader)?;
    if wire.error.is_some() {
        bail!("Ollama inventory reported an error");
    }
    check_count(wire.models.len())?;
    let mut inventory = BTreeMap::new();
    for model in wire.models {
        let identity = identity(model.identity)?;
        if model
            .expires_at
            .as_ref()
            .is_some_and(|s| s.len() > 128 || s.chars().any(char::is_control))
        {
            bail!("Invalid Ollama expiry evidence");
        }
        let resident = Resident {
            identity,
            context_length: model.context_length.filter(|n| *n > 0),
            expires_at: model.expires_at.filter(|s| !s.is_empty()),
        };
        if inventory
            .insert(resident.identity.name.clone(), resident)
            .is_some()
        {
            bail!("Duplicate Ollama resident name");
        }
    }
    Ok(ResidentInventory(inventory))
}

/// Memory the resident models occupy, as the service reports it. Presentation
/// only: an unreadable body is simply an unknown amount.
pub fn resident_bytes(body: &[u8]) -> u64 {
    #[derive(Deserialize)]
    struct Sized {
        size_vram: Option<u64>,
        size: Option<u64>,
    }
    serde_json::from_slice::<Envelope<Sized>>(body).map_or(0, |wire| {
        wire.models
            .iter()
            .filter_map(|model| model.size_vram.filter(|n| *n > 0).or(model.size))
            .fold(0, u64::saturating_add)
    })
}

pub fn parse_catalog(reader: impl Read) -> Result<Catalog> {
    let wire: Envelope<WireCatalogEntry> = decode(reader)?;
    if wire.error.is_some() {
        bail!("Ollama catalog reported an error");
    }
    check_count(wire.models.len())?;
    let mut catalog = BTreeMap::new();
    for model in wire.models {
        let entry = CatalogEntry {
            identity: identity(model.identity)?,
            remote: model.remote_host.is_some_and(|s| !s.is_empty())
                || model.remote_model.is_some_and(|s| !s.is_empty()),
        };
        if catalog.insert(entry.identity.name.clone(), entry).is_some() {
            bail!("Duplicate Ollama catalog name");
        }
    }
    Ok(Catalog(catalog))
}

/// Match routing name AND content identity. Never substitute another tag or
/// treat a digest as an accepted routing name.
pub fn verify_catalog_identity(resident: &Resident, catalog: &Catalog) -> Result<()> {
    match catalog.0.get(&resident.identity.name) {
        Some(entry) if entry.identity == resident.identity && !entry.remote => Ok(()),
        _ => bail!("Ollama catalog identity is missing, changed, or remote"),
    }
}

pub fn classify_show(reader: impl Read, entry: &CatalogEntry) -> Result<ModelEvidence> {
    let show: WireShow = decode(reader)?;
    Ok(classify_details(&show, entry))
}

fn classify_details(show: &WireShow, entry: &CatalogEntry) -> ModelEvidence {
    let source_tag = entry
        .identity
        .name
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if entry.remote
        || source_tag == "cloud"
        || source_tag.ends_with("-cloud")
        || show.remote_host.as_ref().is_some_and(|s| !s.is_empty())
        || show.remote_model.as_ref().is_some_and(|s| !s.is_empty())
    {
        return ModelEvidence::Remote;
    }
    if local_reference(&entry.identity.name).is_err() {
        return ModelEvidence::Unknown;
    }
    let Some(capabilities) = &show.capabilities else {
        return ModelEvidence::Unknown;
    };
    let architecture = show
        .model_info
        .as_ref()
        .and_then(|info| info.get("general.architecture"))
        .and_then(serde_json::Value::as_str);
    if show.details.format.as_deref() != Some("gguf") || architecture.is_none_or(str::is_empty) {
        return ModelEvidence::Unknown;
    }
    if capabilities.iter().any(|s| s == "embedding") {
        return ModelEvidence::Embedding;
    }
    if capabilities.iter().any(|s| s == "completion") {
        // Tools, thinking, vision and later capability names describe what a
        // completion model accepts, not load state an empty preload must replay.
        ModelEvidence::LocalCompletionCandidate
    } else {
        ModelEvidence::Unknown
    }
}

/// Explicit local suffix prevents the pinned handler from proxying a remote
/// alias, including an alias changed between catalog validation and dispatch.
pub fn local_reference(name: &str) -> Result<String> {
    let tag = name
        .rsplit_once(':')
        .filter(|(_, tag)| !tag.contains('/'))
        .map(|(_, tag)| tag.to_ascii_lowercase());
    if name.is_empty()
        || name.len() > 512
        || name.chars().any(|c| c.is_whitespace() || c.is_control())
        || tag.as_deref().is_none_or(|tag| {
            tag.is_empty() || tag == "local" || tag == "cloud" || tag.ends_with("-cloud")
        })
    {
        bail!("Ollama route is not an unambiguous local tagged model");
    }
    Ok(format!("{name}:local"))
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayCandidate {
    resident: Resident,
    deadline: crate::ollama_expiry::Deadline,
}

impl ReplayCandidate {
    pub fn resident(&self) -> &Resident {
        &self.resident
    }
    pub fn captured_at(&self) -> std::time::Duration {
        self.deadline.captured_at()
    }
    pub fn validate(&self, policy: Policy) -> Result<()> {
        let normalized = identity(WireIdentity {
            name: self.resident.identity.name.clone(),
            model: self.resident.identity.name.clone(),
            digest: self.resident.identity.digest.clone(),
        })?;
        if normalized != self.resident.identity
            || self
                .resident
                .context_length
                .is_none_or(|n| !(4..=i32::MAX as u64).contains(&n))
        {
            bail!("Ollama persisted candidate identity/context is invalid");
        }
        local_reference(&self.resident.identity.name)?;
        self.deadline
            .validate(self.resident.expires_at.as_deref(), policy)
    }
    /// Capture only the limited identity/context/deadline contract.
    /// The caller must journal this original evidence before sending any change.
    pub fn capture(
        resident: &Resident,
        catalog: &Catalog,
        show: impl Read,
        now: std::time::SystemTime,
        policy: Policy,
    ) -> Result<Self> {
        verify_catalog_identity(resident, catalog)?;
        let normalized = identity(WireIdentity {
            name: resident.identity.name.clone(),
            model: resident.identity.name.clone(),
            digest: resident.identity.digest.clone(),
        })?;
        if normalized != resident.identity {
            bail!("Ollama captured identity is not canonical");
        }
        let details: WireShow = decode(show)?;
        let entry = &catalog.0[&resident.identity.name];
        if classify_details(&details, entry) != ModelEvidence::LocalCompletionCandidate {
            bail!("Ollama model type or locality is unsupported for replay");
        }
        let context = resident
            .context_length
            .filter(|n| (4..=i32::MAX as u64).contains(n))
            .ok_or_else(|| anyhow::anyhow!("Ollama observed context is unsupported"))?;
        let info = details
            .model_info
            .as_ref()
            .expect("candidate requires model info");
        let architecture = info["general.architecture"]
            .as_str()
            .expect("candidate requires architecture");
        let training_context = info
            .get(&format!("{architecture}.context_length"))
            .and_then(serde_json::Value::as_u64);
        if training_context.is_none_or(|limit| limit < context) {
            bail!("Ollama observed context exceeds or lacks model context evidence");
        }
        let deadline =
            crate::ollama_expiry::Deadline::capture(resident.expires_at.as_deref(), now, policy)?;
        Ok(Self {
            resident: resident.clone(),
            deadline,
        })
    }

    pub fn unload_request(&self) -> Result<serde_json::Value> {
        unload_request(&self.resident.identity)
    }

    /// Expired obligations have no load request; report their separate outcome.
    /// Recheck catalog identity and games immediately before dispatch, then
    /// verify the complete final resident set. This does not grant runtime authority.
    pub fn preload_request(
        &self,
        now: std::time::SystemTime,
        elapsed: Option<std::time::Duration>,
        policy: Policy,
    ) -> Result<Option<serde_json::Value>> {
        let keep_alive = match self.deadline.plan(now, elapsed, policy)? {
            crate::ollama_expiry::ResidencyPlan::Expired => return Ok(None),
            crate::ollama_expiry::ResidencyPlan::KeepFor(remaining) => {
                serde_json::json!(format!("{}ns", remaining.as_nanos()))
            }
            crate::ollama_expiry::ResidencyPlan::Indefinite => serde_json::json!(-1),
        };
        Ok(Some(serde_json::json!({
            "model":local_reference(&self.resident.identity.name)?, "prompt":"", "stream":false,
            "keep_alive":keep_alive,
            "options":{"num_ctx": self.resident.context_length}
        })))
    }
    /// Residency the frozen policy asks the load to keep; `None` when the
    /// obligation is expired, indefinite or governed by the absolute policy.
    pub fn frozen_residency(&self, policy: Policy) -> Option<std::time::Duration> {
        match self
            .deadline
            .plan(std::time::UNIX_EPOCH, None, policy)
            .ok()?
        {
            crate::ollama_expiry::ResidencyPlan::KeepFor(remaining)
                if policy == Policy::FrozenRemaining =>
            {
                Some(remaining)
            }
            _ => None,
        }
    }
}

pub fn unload_request(identity: &Identity) -> Result<serde_json::Value> {
    Ok(serde_json::json!({"model":local_reference(&identity.name)?,
        "prompt":"", "stream":false, "keep_alive":0}))
}

/// A resident local model that cannot be replayed (embedding, unknown format
/// or capabilities, unsupported context or expiry) may still be unloaded and
/// reported as not restored. Its catalog identity and locality must hold:
/// remote, changed or ambiguously routed content refuses the whole capture.
pub fn unload_only(resident: &Resident, catalog: &Catalog, show: impl Read) -> Result<Identity> {
    verify_catalog_identity(resident, catalog)?;
    let normalized = identity(WireIdentity {
        name: resident.identity.name.clone(),
        model: resident.identity.name.clone(),
        digest: resident.identity.digest.clone(),
    })?;
    local_reference(&resident.identity.name)?;
    if normalized != resident.identity
        || classify_show(show, &catalog.0[&resident.identity.name])? == ModelEvidence::Remote
    {
        bail!("Ollama resident model is remote or not canonical; no control started");
    }
    Ok(normalized)
}

/// Acknowledgement is not residency evidence. Reject inference/error/remote
/// responses instead of treating any HTTP success as an accepted empty request.
pub fn verify_acknowledgement(reader: impl Read, route: &str, unloading: bool) -> Result<()> {
    #[derive(Deserialize)]
    struct Ack {
        model: String,
        done: bool,
        done_reason: String,
        response: Option<String>,
        error: Option<String>,
        remote_host: Option<String>,
        remote_model: Option<String>,
    }
    let ack: Ack = decode(reader)?;
    if ack.model != route
        || !ack.done
        || ack.done_reason != if unloading { "unload" } else { "load" }
        || ack.response.is_some_and(|s| !s.is_empty())
        || ack.error.is_some()
        || ack.remote_host.is_some_and(|s| !s.is_empty())
        || ack.remote_model.is_some_and(|s| !s.is_empty())
    {
        bail!("Ollama empty-request acknowledgement is invalid");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResidencyIssue {
    Missing(String),
    IdentityChanged(String),
    ContextUnverified(String),
}

/// One final inventory must verify the whole captured set. A successful load
/// response for each model cannot establish simultaneous residency.
pub fn compare_residency(
    captured: &ResidentInventory,
    observed: &ResidentInventory,
) -> Vec<ResidencyIssue> {
    let mut issues = Vec::new();
    for (name, saved) in &captured.0 {
        let Some(current) = observed.0.get(name) else {
            issues.push(ResidencyIssue::Missing(name.clone()));
            continue;
        };
        if saved.identity != current.identity {
            issues.push(ResidencyIssue::IdentityChanged(name.clone()));
        } else if saved.context_length.is_none() || saved.context_length != current.context_length {
            issues.push(ResidencyIssue::ContextUnverified(name.clone()));
        }
    }
    issues
}

/// Absence is content-based so reloading the same digest under another alias
/// cannot be mistaken for freeing the captured model's resources.
pub fn captured_models_absent(captured: &ResidentInventory, observed: &ResidentInventory) -> bool {
    !captured.0.values().any(|saved| {
        observed
            .0
            .values()
            .any(|current| saved.identity.digest == current.identity.digest)
    })
}

#[cfg(test)]
mod tests;
