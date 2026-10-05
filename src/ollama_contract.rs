//! Read-only P0-1 evidence for the pinned Ollama source target.
//! The experimental adapter consumes candidates; production enrollment and
//! journal routing remain disabled until mixed-provider engine integration.
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
    if capabilities.as_slice() == ["completion"] {
        ModelEvidence::LocalCompletionCandidate
    } else {
        // Vision, tools, thinking, unknown capabilities and other formats need
        // their own replay contract, even when completion is also advertised.
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
    pub fn validate(&self) -> Result<()> {
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
        self.deadline.validate(self.resident.expires_at.as_deref())
    }
    /// Capture only the limited identity/context/absolute-deadline contract.
    /// The caller must journal this original evidence before sending any change.
    pub fn capture(
        resident: &Resident,
        catalog: &Catalog,
        show: impl Read,
        now: std::time::SystemTime,
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
            crate::ollama_expiry::Deadline::capture(resident.expires_at.as_deref(), now)?;
        Ok(Self {
            resident: resident.clone(),
            deadline,
        })
    }

    pub fn unload_request(&self) -> Result<serde_json::Value> {
        Ok(
            serde_json::json!({"model":local_reference(&self.resident.identity.name)?,
            "prompt":"", "stream":false, "keep_alive":0}),
        )
    }

    /// Expired obligations have no load request; report their separate outcome.
    /// Recheck catalog identity and games immediately before dispatch, then
    /// verify the complete final resident set. This does not grant runtime authority.
    pub fn preload_request(
        &self,
        now: std::time::SystemTime,
        elapsed: Option<std::time::Duration>,
    ) -> Result<Option<serde_json::Value>> {
        match self.deadline.plan(now, elapsed)? {
            crate::ollama_expiry::ResidencyPlan::Expired => Ok(None),
            crate::ollama_expiry::ResidencyPlan::KeepFor(remaining) => {
                Ok(Some(serde_json::json!({
                    "model":local_reference(&self.resident.identity.name)?, "prompt":"", "stream":false,
                    "keep_alive":format!("{}ns", remaining.as_nanos()),
                    "options":{"num_ctx": self.resident.context_length}
                })))
            }
        }
    }
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
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn model(name: &str, digit: char) -> Value {
        json!({"name": name, "model": name, "digest": digit.to_string().repeat(64),
            "context_length": 4096, "expires_at": "2026-10-05T12:00:00Z"})
    }
    fn inventory(models: Vec<Value>) -> ResidentInventory {
        parse_resident_inventory(json!({"models": models}).to_string().as_bytes()).unwrap()
    }
    fn catalog(models: Vec<Value>) -> Catalog {
        parse_catalog(json!({"models": models}).to_string().as_bytes()).unwrap()
    }
    fn show() -> Value {
        json!({"details":{"format":"gguf"}, "capabilities":["completion"],
            "model_info":{"general.architecture":"fixture"}})
    }

    #[test]
    fn malformed_or_unknown_inventory_never_becomes_an_empty_success() {
        for body in [
            "",
            "{}",
            "null",
            "[]",
            "{\"models\":null}",
            "{\"models\":[] } trailing",
            "{\"models\":[],\"models\":[]}",
            "{\"models\":[],\"error\":\"fixture error\"}",
        ] {
            assert!(parse_resident_inventory(body.as_bytes()).is_err(), "{body}");
        }
        assert!(
            parse_resident_inventory(br#"{"models":[]}"#.as_slice())
                .unwrap()
                .0
                .is_empty()
        );
        let mut invalid = model("fixture:latest", 'a');
        invalid["context_length"] = json!(-1);
        assert!(
            parse_resident_inventory(json!({"models":[invalid]}).to_string().as_bytes()).is_err()
        );
    }

    #[test]
    fn read_and_model_limits_refuse_the_whole_response() {
        struct Endless(usize);
        impl Read for Endless {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                bytes.fill(b' ');
                self.0 += bytes.len();
                Ok(bytes.len())
            }
        }
        let mut reader = Endless(0);
        assert!(parse_resident_inventory(&mut reader).is_err());
        assert_eq!(reader.0, MAX_RESPONSE_BYTES + 1);
        let models = (0..=MAX_MODELS)
            .map(|i| model(&format!("fixture-{i}:latest"), 'a'))
            .collect::<Vec<_>>();
        let body = json!({"models":models}).to_string();
        assert!(parse_resident_inventory(body.as_bytes()).is_err());
        assert!(parse_catalog(body.as_bytes()).is_err());
        assert!(
            classify_show(
                std::io::repeat(b' '),
                &catalog(vec![model("fixture:latest", 'a')]).0["fixture:latest"]
            )
            .is_err()
        );
    }

    #[test]
    fn ambiguous_names_digests_and_duplicate_routes_are_rejected() {
        for (field, value) in [
            ("name", ""),
            ("name", "fixture other"),
            ("model", "other:latest"),
            ("digest", "unknown"),
            ("digest", "sha256:zzz"),
        ] {
            let mut invalid = model("fixture:latest", 'a');
            invalid[field] = json!(value);
            let body = json!({"models":[invalid]}).to_string();
            assert!(parse_resident_inventory(body.as_bytes()).is_err());
            assert!(parse_catalog(body.as_bytes()).is_err());
        }
        let body = json!({"models":[model("fixture:latest", 'a'), model("fixture:latest", 'b')]})
            .to_string();
        assert!(parse_catalog(body.as_bytes()).is_err());
        assert!(parse_resident_inventory(body.as_bytes()).is_err());
    }

    #[test]
    fn catalog_identity_never_substitutes_tags_content_or_remote_aliases() {
        let saved = inventory(vec![model("fixture:latest", 'a')]);
        let resident = &saved.0["fixture:latest"];
        assert!(
            verify_catalog_identity(resident, &catalog(vec![model("fixture:latest", 'a')])).is_ok()
        );
        for models in [
            vec![],
            vec![model("fixture:latest", 'b')],
            vec![model("alias:latest", 'a')],
        ] {
            assert!(verify_catalog_identity(resident, &catalog(models)).is_err());
        }
        let mut remote = model("fixture:latest", 'a');
        remote["remote_model"] = json!("remote-fixture");
        assert!(verify_catalog_identity(resident, &catalog(vec![remote])).is_err());
    }

    #[test]
    fn show_remote_aliases_and_unsupported_capabilities_never_authorize_control() {
        let catalog = catalog(vec![model("fixture:latest", 'a')]);
        let entry = &catalog.0["fixture:latest"];
        let classify = |value: Value| classify_show(value.to_string().as_bytes(), entry).unwrap();
        assert_eq!(classify(show()), ModelEvidence::LocalCompletionCandidate);
        for field in ["remote_host", "remote_model"] {
            let mut remote = show();
            remote[field] = json!("remote-fixture");
            let evidence = classify(remote);
            assert_eq!(evidence, ModelEvidence::Remote);
            assert_eq!(evidence.replay_block(), ReplayBlock::Remote);
        }
        let mut embed = show();
        embed["capabilities"] = json!(["embedding"]);
        let evidence = classify(embed);
        assert_eq!(evidence, ModelEvidence::Embedding);
        assert_eq!(
            evidence.replay_block(),
            ReplayBlock::EmbeddingContractUnresolved
        );
        for caps in [
            json!([]),
            json!(["completion", "vision"]),
            json!(["unknown"]),
            Value::Null,
        ] {
            let mut unknown = show();
            unknown["capabilities"] = caps;
            let evidence = classify(unknown);
            assert_eq!(evidence, ModelEvidence::Unknown);
            assert_eq!(evidence.replay_block(), ReplayBlock::LocalityUnknown);
        }
        assert_eq!(classify(json!({})), ModelEvidence::Unknown);
        assert_eq!(
            classify(show()).replay_block(),
            ReplayBlock::AdapterNotIntegrated
        );
    }

    #[test]
    fn missing_options_context_and_expiry_remain_uninterpreted_evidence() {
        let mut wire = model("fixture:latest", 'a');
        wire.as_object_mut().unwrap().remove("context_length");
        wire.as_object_mut().unwrap().remove("expires_at");
        let saved = inventory(vec![wire]);
        assert_eq!(saved.0["fixture:latest"].context_length, None);
        assert_eq!(saved.0["fixture:latest"].expires_at, None);
        assert_eq!(
            compare_residency(&saved, &saved),
            [ResidencyIssue::ContextUnverified("fixture:latest".into())]
        );
        for expiry in [
            "0001-01-01T00:00:00Z",
            "9999-12-31T23:59:59Z",
            "not-a-timestamp",
            "2026-10-05T12:00:00+02:00",
        ] {
            let mut wire = model("fixture:latest", 'a');
            wire["expires_at"] = json!(expiry);
            let saved = inventory(vec![wire]);
            assert_eq!(
                saved.0["fixture:latest"].expires_at.as_deref(),
                Some(expiry)
            );
        }
        // A catalogue's parameters or defaults are never live runner options.
        assert_eq!(
            ModelEvidence::LocalCompletionCandidate.replay_block(),
            ReplayBlock::AdapterNotIntegrated
        );
    }

    #[test]
    fn final_set_verification_catches_eviction_and_context_mismatch() {
        let saved = inventory(vec![
            model("first:latest", 'a'),
            model("second:latest", 'b'),
        ]);
        assert!(compare_residency(&saved, &saved).is_empty());
        let last_load = inventory(vec![model("second:latest", 'b')]);
        assert_eq!(
            compare_residency(&saved, &last_load),
            [ResidencyIssue::Missing("first:latest".into())]
        );
        let mut changed = model("second:latest", 'b');
        changed["context_length"] = json!(8192);
        let observed = inventory(vec![model("first:latest", 'c'), changed]);
        assert_eq!(
            compare_residency(&saved, &observed),
            [
                ResidencyIssue::IdentityChanged("first:latest".into()),
                ResidencyIssue::ContextUnverified("second:latest".into())
            ]
        );
        assert_eq!(saved.0["first:latest"].identity.digest, "a".repeat(64));
    }

    #[test]
    fn acknowledgement_or_alias_reload_cannot_establish_absence() {
        let saved = inventory(vec![model("fixture:latest", 'a')]);
        assert!(!captured_models_absent(&saved, &saved));
        assert!(!captured_models_absent(
            &saved,
            &inventory(vec![model("alias:latest", 'a')])
        ));
        assert!(captured_models_absent(&saved, &inventory(vec![])));
        assert!(!captured_models_absent(&saved, &saved)); // External reload after a prior absence.
        assert_eq!(saved.0.len(), 1); // Original evidence is never replaced or accumulated.
    }

    fn capture(wire: Value, details: Value) -> Result<ReplayCandidate> {
        let saved = inventory(vec![wire.clone()]);
        let catalog = catalog(vec![wire]);
        ReplayCandidate::capture(
            saved.0.values().next().unwrap(),
            &catalog,
            details.to_string().as_bytes(),
            clock(),
        )
    }
    fn clock() -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_791_201_300)
    }
    fn completion_details() -> Value {
        let mut details = show();
        details["model_info"]["fixture.context_length"] = json!(8192);
        details
    }
    fn ack(reason: &str) -> Value {
        json!({"model":"fixture:latest:local", "done":true, "done_reason":reason, "response":""})
    }

    #[test]
    fn limited_capture_uses_observed_per_slot_context_and_explicit_local_route() {
        let candidate = capture(model("fixture:latest", 'a'), completion_details()).unwrap();
        assert_eq!(
            candidate.unload_request().unwrap(),
            json!({"model":"fixture:latest:local",
            "prompt":"", "stream":false, "keep_alive":0})
        );
        let request = candidate
            .preload_request(clock(), Some(std::time::Duration::ZERO))
            .unwrap()
            .unwrap();
        assert_eq!(request["options"], json!({"num_ctx":4096}));
        assert_eq!(request["keep_alive"], "300000000000ns");
        assert_eq!(request["prompt"], "");
        assert_eq!(request["model"], "fixture:latest:local");
        assert!(request.get("messages").is_none());
        assert_eq!(
            candidate
                .preload_request(clock() + std::time::Duration::from_secs(300), None)
                .unwrap(),
            None
        );
        // Inventory reports per-slot context, not a context to divide or
        // multiply by a guessed parallel setting. No parallel option is sent.
        for observed in [4, 4096, 8192] {
            let mut wire = model("fixture:latest", 'a');
            wire["context_length"] = json!(observed);
            let request = capture(wire, completion_details())
                .unwrap()
                .preload_request(clock(), None)
                .unwrap()
                .unwrap();
            assert_eq!(request["options"]["num_ctx"], observed);
        }
    }

    #[test]
    fn unsupported_context_embedding_cloud_and_expiry_refuse_candidate_capture() {
        for observed in [0u64, 3, 8193, u64::MAX] {
            let mut wire = model("fixture:latest", 'a');
            wire["context_length"] = json!(observed);
            assert!(capture(wire, completion_details()).is_err());
        }
        for capabilities in [json!(["embedding"]), json!(["completion", "vision"])] {
            let mut details = completion_details();
            details["capabilities"] = capabilities;
            assert!(capture(model("fixture:latest", 'a'), details).is_err());
        }
        for name in [
            "fixture:cloud",
            "fixture:8b-cloud",
            "fixture:LOCAL",
            "fixture",
        ] {
            assert!(local_reference(name).is_err());
            assert!(capture(model(name, 'a'), completion_details()).is_err());
        }
        let mut details = completion_details();
        details["remote_host"] = json!("remote-fixture");
        assert!(capture(model("fixture:latest", 'a'), details).is_err());
        let mut details = completion_details();
        details["model_info"]
            .as_object_mut()
            .unwrap()
            .remove("fixture.context_length");
        assert!(capture(model("fixture:latest", 'a'), details).is_err());
        let mut wire = model("fixture:latest", 'a');
        wire["expires_at"] = Value::Null;
        assert!(capture(wire, completion_details()).is_err());
    }

    #[test]
    fn acknowledgement_refuses_inference_remote_errors_and_partial_responses() {
        assert!(
            verify_acknowledgement(
                ack("load").to_string().as_bytes(),
                "fixture:latest:local",
                false
            )
            .is_ok()
        );
        for (key, value) in [
            ("done", json!(false)),
            ("done_reason", json!("stop")),
            ("model", json!("another:latest:local")),
            ("response", json!("unexpected inference")),
            ("error", json!("fixture error")),
            ("remote_model", json!("remote-fixture")),
        ] {
            let mut body = ack("unload");
            body[key] = value;
            assert!(
                verify_acknowledgement(body.to_string().as_bytes(), "fixture:latest:local", true)
                    .is_err()
            );
        }
        assert!(verify_acknowledgement(b"{}".as_slice(), "fixture:latest:local", true).is_err());
    }

    #[test]
    fn isolated_http_acknowledgement_waits_for_inventory_and_detects_reload() {
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
            time::{Duration, Instant},
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let busy = json!({"models":[model("fixture:latest", 'a')]}).to_string();
        let absent = json!({"models":[]}).to_string();
        let reloaded = json!({"models":[model("alias:latest", 'a')]}).to_string();
        // A source-inspired mock keeps the runner resident after acknowledgement.
        // This tests our consumer's decisions, not the vendor scheduler itself.
        let responses = [
            ack("unload").to_string(),
            busy.clone(),
            busy,
            absent,
            reloaded,
        ];
        let server = std::thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(5);
            let mut requests = Vec::new();
            for body in responses {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < end =>
                        {
                            std::thread::sleep(Duration::from_millis(2))
                        }
                        Err(e) => panic!("fixture accept failed: {e}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let mut length = 0usize;
                let mut header_bytes = request.len();
                loop {
                    let mut line = String::new();
                    assert_ne!(
                        reader.read_line(&mut line).unwrap(),
                        0,
                        "fixture header ended early"
                    );
                    header_bytes += line.len();
                    assert!(header_bytes <= 8192);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                assert!(length <= 4096);
                let mut input = vec![0; length];
                reader.read_exact(&mut input).unwrap();
                requests.push((request, input));
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
            requests
        });
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout(Duration::from_secs(1))
            .build();
        let saved = inventory(vec![model("fixture:latest", 'a')]);
        let candidate = capture(model("fixture:latest", 'a'), completion_details()).unwrap();
        let response = agent
            .post(&format!("{url}/api/generate"))
            .send_json(candidate.unload_request().unwrap())
            .unwrap();
        verify_acknowledgement(response.into_reader(), "fixture:latest:local", true).unwrap();
        for expected_absent in [false, false, true, false] {
            let response = agent.get(&format!("{url}/api/ps")).call().unwrap();
            let observed = parse_resident_inventory(response.into_reader()).unwrap();
            assert_eq!(captured_models_absent(&saved, &observed), expected_absent);
        }
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 5);
        assert!(requests[0].0.starts_with("POST /api/generate "));
        assert_eq!(
            serde_json::from_slice::<Value>(&requests[0].1).unwrap(),
            candidate.unload_request().unwrap()
        );
        assert!(
            requests[1..]
                .iter()
                .all(|(line, body)| line.starts_with("GET /api/ps ") && body.is_empty())
        );
        assert_eq!(saved.0.len(), 1);
    }
}
