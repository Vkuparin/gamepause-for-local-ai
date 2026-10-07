//! Read-only diagnostics for LM Studio and the sanitized WebSocket step log.

use super::server_state;
use serde_json::{Value, json};
use std::{io::Write, path::Path};
pub(super) fn cli_version_tag(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let clean = regex::Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]")
        .expect("constant ANSI pattern")
        .replace_all(&text, "")
        .into_owned();
    let lines: Vec<_> = clean
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    lines
        .iter()
        .find(|s| {
            s.starts_with("CLI commit:")
                || s.to_lowercase().starts_with("lms version")
                || s.starts_with("Version:")
        })
        .copied()
        .or_else(|| {
            if lines.len() == 1 {
                lines.first().copied()
            } else {
                None
            }
        })
        .unwrap_or("unknown")
        .chars()
        .take(200)
        .collect()
}
// ── P2-2: diagnostics core (pure, mock-free) ─────────────────────────────────
// The Doctor panel / `--doctor` assembles these from the live backend. The core
// is pure and takes fixed inputs so it is unit-testable without a live LM
// Studio (the `diagnostics()` and `data_dir_writable()` acceptance test).
/// Whether a directory is actually writable. A missing directory is *not*
/// writable (the caller must create it first); a directory that cannot be
/// opened is treated as read-only rather than an error.
pub fn data_dir_writable(path: &Path) -> bool {
    let probe = path.join(format!(".gamepause-write-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&probe)
    {
        Ok(mut f) => {
            // Writing the actual byte is the real check: a directory can be
            // openable but read-only for the write (rare, but possible on
            // OneDrive-synced or networked paths).
            let ok = f.write_all(b"gamepause").is_ok();
            let _ = f.sync_all();
            let _ = std::fs::remove_file(&probe);
            ok
        }
        Err(_) => false,
    }
}
/// Assemble the Doctor panel body from a fixed set of inputs. `lms_version` is
/// the `lms version` output (or the error text when the CLI is unavailable),
/// `server` the `server status --json` payload, `models` the loaded instances,
/// `data_dir` the resolved data directory, and `data_dir_ok` whether it is
/// writable. Keeping this pure is what lets the acceptance test assert the exact
/// shape without touching a live LM Studio.
#[must_use]
pub fn diagnostics(
    lms_version: &str,
    server: &Value,
    models: &[Value],
    data_dir: &Path,
    data_dir_ok: bool,
) -> Value {
    let (running, port) = server_state(server);
    json!({
        "lms_version": lms_version,
        "server": {
            "running": running,
            "port": port,
            "raw": server,
        },
        "loaded_models": models.iter().map(|m| json!({
            "identifier": m["identifier"],
            "model": m.get("modelKey").cloned().or_else(|| m.get("model").cloned()),
            "status": m["status"],
        })).collect::<Vec<_>>(),
        "model_count": models.len(),
        "data_dir": {
            "path": data_dir.to_string_lossy().to_string(),
            "writable": data_dir_ok,
        }
    })
}
// ── P2-4: per-capture/restore WS protocol logging (local-only) ──────────────
// On every WS operation (loadModel / channelCreate / getLoadConfig read-back)
// GamePause records the LM Studio version and a `success` or `failed:<field>`
// line, where <field> is the field that actually diverged. The line is pure
// (testable without a live server) and the sink reuses the same local
// gamepause.log format as the panic/doctor sink — never uploaded, never
// sent to a remote service.
/// The field that a step detail names as failing, if it names one. The field
/// comparison and native-config checks phrase failures as "Restored load field
/// <name> differs" / "Restored native <name> differs", so the field is the
/// token after those markers. Returns `None` when the failure names no field
/// (identity/TTL mismatch, a missing instance, …) or when the step succeeded.
pub(super) fn failing_field(detail: &str) -> Option<String> {
    for marker in ["field ", "native "] {
        if let Some(idx) = detail.find(marker) {
            let token = detail[idx + marker.len()..]
                .split(|c: char| c.is_whitespace() || c == ';')
                .next()
                .unwrap_or("");
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}
/// Build one `gamepause.log` line for a WS protocol step. `ok` is the step's
/// authoritative outcome (from `VerifyStep`), so success/failure is never
/// guessed from free text; when a step failed and its detail names a field,
/// that field is included (`failed:<field>`). The LM Studio version is always
/// present so a line can be correlated with a specific protocol implementation.
#[must_use]
pub fn ws_log_line(version: &str, step: &str, ok: bool, detail: &str) -> String {
    let outcome = if ok {
        "success".to_string()
    } else {
        match failing_field(detail) {
            Some(field) => format!("failed:{field}"),
            None => "failed".to_string(),
        }
    };
    format!(
        "ws {} lm={} {outcome}",
        step.replace(['\r', '\n', '\t'], " "),
        version.replace(['\r', '\n', '\t'], " ")
    )
}
/// Append a WS protocol line to the local `gamepause.log` under `folder`.
/// Local-only: this never transmits data and is a no-op if the directory is
/// not writable (the app is still usable; the log is best-effort).
pub fn ws_log(folder: &Path, version: &str, step: &str, ok: bool, detail: &str) {
    let line = ws_log_line(version, step, ok, detail);
    crate::app::log(folder, &line);
}
