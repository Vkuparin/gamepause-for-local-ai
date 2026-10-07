//! Model identity evidence for accepting a user-loaded loadout during recovery.
use super::{Model, Snapshot, identity};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

pub(crate) const MAX_RESIDENTS: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Resident {
    pub identifier: String,
    pub model_key: String,
    pub namespace: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LoadoutOffer {
    pub loaded: Vec<Resident>,
    pub saved: Vec<Resident>,
    // Binds approval to immutable recovery intent, including saved settings.
    #[serde(skip)]
    pub(crate) authority: String,
}

pub(crate) fn inventory(loaded: &[Value]) -> Result<Vec<Resident>> {
    identity::resident_keys(loaded)?;
    if loaded.len() > MAX_RESIDENTS {
        bail!("LM Studio inventory exceeds the recovery limit; recovery retained");
    }
    let mut result = loaded
        .iter()
        .map(|info| {
            let identifier = info["identifier"]
                .as_str()
                .context("Missing instance identifier")?;
            let model_key = identity::resolved_key(info)?;
            if model_key.trim().is_empty() {
                bail!("LM Studio model identity is unknown; recovery retained");
            }
            Ok(Resident {
                identifier: identifier.into(),
                model_key,
                namespace: if info["type"] == "embedding" {
                    "embedding"
                } else {
                    "llm"
                }
                .into(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    result.sort();
    Ok(result)
}

fn saved(model: &Model) -> Resident {
    Resident {
        identifier: model.identifier.clone(),
        model_key: model.model_key.clone(),
        namespace: model.namespace.clone(),
    }
}

pub(crate) fn matches(snapshot: &Snapshot, loaded: &[Resident]) -> bool {
    let wanted = snapshot
        .models
        .iter()
        .map(|m| (&m.model_key, &m.namespace))
        .collect::<BTreeSet<_>>();
    let actual = loaded
        .iter()
        .map(|m| (&m.model_key, &m.namespace))
        .collect::<BTreeSet<_>>();
    wanted == actual
}

pub(crate) fn offer(snapshot: &Snapshot, loaded: Vec<Resident>) -> Result<LoadoutOffer> {
    let mut original = snapshot.clone();
    original.server_stopped = false;
    original.pause_complete = false;
    for model in &mut original.models {
        model.stage.clear();
    }
    Ok(LoadoutOffer {
        loaded,
        saved: snapshot.models.iter().map(saved).collect(),
        authority: serde_json::to_string(&original)?,
    })
}
