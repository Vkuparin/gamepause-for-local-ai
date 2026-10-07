//! Which loaded instance is which: the key a resident model is recognised by,
//! and the field-by-field comparison of a restored model against its capture.

use super::Model;
use anyhow::{Context, Result, bail};
use serde_json::Value;
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
pub(super) fn identity_matches(model: &Model, info: &Value) -> bool {
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
pub(super) fn prevent_duplicate_restore(model: &Model, loaded: &[Value]) -> Result<()> {
    resident_keys(loaded)?;
    if let Some(existing) = loaded.iter().find(|info| {
        info["identifier"] != model.identifier
            && (identity_matches(model, info) || info["modelKey"] == model.base_key)
    }) {
        return Err(crate::provider::ManualRetryRequired(format!(
            "{} is already loaded as {}; saved instance {} is missing. A second copy was not loaded. Finish current work and unload the conflicting instance in LM Studio, then choose Resume AI. Recovery retained",
            model.base_key, existing["identifier"].as_str().unwrap_or("unknown"), model.identifier,
        )).into());
    }
    Ok(())
}
pub(super) fn verify_ttl(model: &Model, info: &Value) -> Result<()> {
    let actual = match info.get("ttlMs") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .context("Invalid LM Studio idle TTL; recovery retained")?,
        ),
    };
    if actual != model.ttl_ms {
        let display =
            |ttl: Option<u64>| ttl.map_or_else(|| "disabled".into(), |ms| format!("{ms} ms"));
        return Err(crate::provider::ManualRetryRequired(format!(
            "Idle TTL mismatch for {}: saved {}, observed {}. Automatic retry paused. Finish current work and unload this instance in LM Studio, then choose Resume AI to restore its saved settings. Recovery retained",
            model.identifier, display(model.ttl_ms), display(actual),
        )).into());
    }
    Ok(())
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
