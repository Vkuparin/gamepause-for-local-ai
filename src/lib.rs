pub mod app;
pub mod config;
pub mod dashboard;
pub mod discovery;
pub mod engine;
pub mod lmstudio;
pub mod processes;
pub mod tray;

pub fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
