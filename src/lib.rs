pub mod app;
pub mod commands;
pub mod config;
pub mod control;
pub mod coordinator;
pub mod dashboard;
mod detection_worker;
pub mod diagnostics;
pub mod discovery;
pub mod engine;
pub mod gameplay;
pub mod lm_session;
pub mod lmstudio;
mod native_menu;
pub mod notifications;
pub mod ollama_contract;
pub mod ollama_expiry;
pub mod ollama_session;
pub mod ownership;
pub mod power;
pub mod presentation;
pub mod processes;
pub mod provider;
pub mod provider_runtime;
pub mod recovery;
pub mod restore_dialog;
mod rich_text;
mod theme;
pub mod tray;
pub mod ui_commands;

pub fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
