//! The persisted LM Studio snapshot: its model records, validation before a
//! recovery is trusted, and normalization of journals written by old versions.

use crate::config::Config;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
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
