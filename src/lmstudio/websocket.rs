//! LM Studio's local WebSocket protocol: reading a loaded model's raw load
//! configuration and loading a model with it. Every operation runs under one
//! deadline and reads a bounded number of bounded messages.

use super::{LMStudio, Model};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    net::{TcpStream, ToSocketAddrs},
    time::{Duration, Instant, SystemTime},
};
use tungstenite::{Message, WebSocket};
pub(super) fn receive(ws: &mut WebSocket<TcpStream>) -> Result<Value> {
    receive_until(ws, Instant::now() + Duration::from_secs(15))
}
pub(super) fn receive_until(ws: &mut WebSocket<TcpStream>, deadline: Instant) -> Result<Value> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .context("LM Studio operation timed out; recovery retained")?;
        ws.get_mut()
            .set_read_timeout(Some(remaining.min(Duration::from_secs(180))))?;
        match ws.read()? {
            Message::Text(s) => return Ok(serde_json::from_str(&s)?),
            Message::Ping(bytes) => ws.send(Message::Pong(bytes))?,
            Message::Close(_) => bail!("LM Studio closed its control connection"),
            _ => {}
        }
    }
}
impl LMStudio {
    pub(super) fn socket(&self, namespace: &str) -> Result<WebSocket<TcpStream>> {
        let address = self
            .config
            .lm_endpoint()
            .to_socket_addrs()?
            .next()
            .context("No local server address")?;
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let (mut socket, _) = tungstenite::client(
            format!("ws://{}/{namespace}", self.config.lm_endpoint()),
            stream,
        )
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
    pub(super) fn raw_config(&self, namespace: &str, id: &str) -> Result<Value> {
        self.logged(&format!("getLoadConfig:{namespace}:{id}"), || {
            self.raw_config_inner(namespace, id)
        })
    }
    pub(super) fn raw_config_inner(&self, namespace: &str, id: &str) -> Result<Value> {
        let mut ws = self.socket(namespace)?;
        ws.send(Message::Text(json!({"type":"rpcCall","callId":1,"endpoint":"getLoadConfig","parameter":{"specifier":{"type":"query","query":{"identifier":id}}}}).to_string().into()))?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let v = receive_until(&mut ws, deadline)?;
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
    pub(super) fn load_model(&self, model: &Model) -> Result<()> {
        self.logged(
            &format!("loadModel:{}:{}", model.namespace, model.identifier),
            || self.load_model_inner(model),
        )
    }
    pub(super) fn load_model_inner(&self, model: &Model) -> Result<()> {
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
            let v = receive_until(&mut ws, started + Duration::from_secs(300))?;
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
}
