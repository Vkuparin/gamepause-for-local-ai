//! Read-only provider evidence. Successful probes do not certify recovery.
use crate::{
    config::{Config, Provider},
    lmstudio::LMStudio,
    ollama_contract,
    ollama_session::{Http, Transport},
};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use std::{path::Path, time::SystemTime};

fn probe<T: Serialize>(result: Result<T>) -> Value {
    match result {
        Ok(value) => json!({"ok":true,"value":value}),
        Err(error) => json!({"ok":false,"error":format!("{error:#}")}),
    }
}
fn observed_at() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn timestamp(seconds: u64) -> String {
    let Some(ticks) = seconds
        .checked_add(11_644_473_600)
        .and_then(|time| time.checked_mul(10_000_000))
    else {
        return "unknown time".into();
    };
    let filetime = windows_sys::Win32::Foundation::FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut time = windows_sys::Win32::Foundation::SYSTEMTIME::default();
    if unsafe { windows_sys::Win32::System::Time::FileTimeToSystemTime(&filetime, &mut time) } == 0
    {
        return "unknown time".into();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}
fn version(bytes: &[u8]) -> Result<String> {
    if bytes.len() > ollama_contract::MAX_RESPONSE_BYTES {
        bail!("Ollama version response exceeds limit");
    }
    let value: Value = serde_json::from_slice(bytes).context("Invalid Ollama version response")?;
    let version = value["version"]
        .as_str()
        .context("Missing Ollama version string")?;
    if version.is_empty() || version.len() > 128 || version.chars().any(char::is_control) {
        bail!("Invalid Ollama version string");
    }
    Ok(version.to_owned())
}
pub fn ollama(transport: &mut impl Transport) -> Value {
    // Probe independently: version failure must not hide inventory evidence.
    let version = probe(
        transport
            .request("/api/version", None)
            .and_then(|bytes| version(&bytes)),
    );
    let inventory = probe(transport.request("/api/ps", None).and_then(|bytes| {
        ollama_contract::parse_resident_inventory(bytes.as_slice())
            .map(|inventory| json!({"resident_count":inventory.0.len()}))
    }));
    json!({"version":version,"inventory":inventory,
        "compatibility":"unverified",
        "detail":"Tested with Ollama 0.35.1 and 0.40.0. Restoration requires local GGUF completion models with unchanged identity/context; other local models are unloaded without reload. Full load settings and conversations are not preserved."})
}
fn untested_lm(folder: &Path, error: Option<String>) -> Value {
    let version = match error {
        Some(error) => json!({"ok":false,"error":error}),
        None => json!({"ok":null,"detail":"Disabled; not probed"}),
    };
    json!({"cli_version":version,"server":{"ok":null},"loaded_models":{"ok":null},
        "ws_protocol":{"ok":null},"data_dir":{"path":folder.to_string_lossy(),
        "writable":crate::lmstudio::data_dir_writable(folder)},"read_only":true})
}
/// Preserve legacy LM Studio fields and add independent provider evidence.
/// This does not load, migrate or modify configuration or recovery journals.
pub fn doctor(config: &Config, folder: &Path) -> Value {
    let mut report = if config.lm_enabled() {
        match LMStudio::new(config.clone()) {
            Ok(mut backend) => {
                backend.set_log_folder(folder.to_owned());
                backend.doctor(folder)
            }
            Err(error) => untested_lm(folder, Some(format!("{error:#}"))),
        }
    } else {
        untested_lm(folder, None)
    };
    let lm_observed_at = observed_at();
    let mut providers = Vec::new();
    for provider in &config.providers {
        let mut evidence = if !provider.enabled() {
            json!({"detail":"Disabled; not probed"})
        } else {
            match provider {
                Provider::LMStudio { .. } => json!({
                    "cli_version":report["cli_version"],"server":report["server"],
                    "loaded_models":report["loaded_models"],"ws_protocol":report["ws_protocol"],
                    "guarantee":"Raw captured load configuration and original server lifecycle, verified on recovery"}),
                Provider::Ollama { endpoint, .. } => match Http::new(endpoint) {
                    Ok(mut transport) => ollama(&mut transport),
                    Err(error) => {
                        json!({"error":format!("{error:#}"),"compatibility":"unverified"})
                    }
                },
                // Names only: start commands can hold secrets and stay unread here.
                Provider::Process { apps, .. } => json!({
                    "apps": apps.iter().map(|app| json!({"name":app.name,
                        "executable":crate::process_session::file_name(&app.path),
                        "relaunch":app.relaunch})).collect::<Vec<_>>(),
                    "detail":"Untested with the real tools. Stops the chosen executables and relaunches them with the recorded command line and folder; environment variables and in-flight work are not preserved."}),
            }
        };
        evidence["id"] = json!(provider.id());
        evidence["kind"] = json!(provider.kind());
        evidence["enabled"] = json!(provider.enabled());
        evidence["endpoint"] = json!(provider.endpoint());
        if let Provider::LMStudio { connection, .. } = provider {
            evidence["connection"] = json!(connection);
        }
        evidence["evidence"] = json!(if provider.enabled() {
            "read_only_probe"
        } else {
            "not_probed"
        });
        evidence["observed_at_unix_seconds"] = if provider.enabled() {
            json!(if matches!(provider, Provider::LMStudio { .. }) {
                lm_observed_at
            } else {
                observed_at()
            })
        } else {
            Value::Null
        };
        providers.push(evidence);
    }
    report["providers"] = json!(providers);
    report["configuration"] = json!({"ok":true});
    report
}
pub fn configuration_error(folder: &Path, error: String) -> Value {
    let mut report = untested_lm(folder, Some(error.clone()));
    report["configuration"] = json!({"ok":false,"error":error});
    report["providers"] = json!([]);
    report
}

/// Advanced details always distinguish cached probes from recovery progress.
pub fn render(shared: &crate::app::Shared) -> String {
    let mut lines = vec![
        if shared.doctor_pending {
            "Read-only diagnostics running; previous evidence remains cached."
        } else {
            "Read-only diagnostics: cached observations; repeat after provider changes."
        }
        .to_string(),
    ];
    for provider in &shared.config.providers {
        let label = provider.kind().name();
        if let Some(status) = shared
            .provider_statuses
            .iter()
            .find(|status| status.kind == provider.kind())
        {
            lines.push(format!(
                "{label} recovery: {:?}. {}",
                status.state, status.error
            ));
        }
        if shared.provider_pending(provider.kind()) {
            lines.push(format!(
                "{label}: Restore saved recovery before disabling or changing its route."
            ));
        }
        if !provider.enabled() {
            lines.push(format!("{label}: disabled; not probed."));
            continue;
        }
        let evidence = shared
            .doctor_report
            .as_ref()
            .and_then(|report| report["providers"].as_array())
            .and_then(|providers| providers.iter().find(|entry| entry["id"] == provider.id()));
        let Some(evidence) = evidence else {
            lines.push(format!(
                "{label}: no diagnostic evidence. Run Read-only diagnostics."
            ));
            continue;
        };
        let connection_changed = matches!(provider, Provider::LMStudio { connection, .. }
            if evidence["connection"] != json!(connection));
        if connection_changed
            || evidence["endpoint"] != provider.endpoint()
            || evidence["enabled"] != provider.enabled()
        {
            lines.push(format!(
                "{label}: stale configuration; repeat diagnostics for the saved endpoint."
            ));
            continue;
        }
        lines.push(format!(
            "{label} cached probe: {}. Service state may have changed.",
            evidence["observed_at_unix_seconds"]
                .as_u64()
                .map(timestamp)
                .unwrap_or_else(|| "unknown time; not probed".into())
        ));
        let fields: &[&str] = match provider {
            Provider::LMStudio { .. } => &["cli_version", "server", "loaded_models", "ws_protocol"],
            Provider::Ollama { .. } => &["version", "inventory"],
            Provider::Process { .. } => &[],
        };
        for field in fields {
            let probe = &evidence[*field];
            let text = if probe["ok"] == true {
                match *field {
                    "cli_version" | "version" => {
                        probe["value"].as_str().unwrap_or("observed").to_string()
                    }
                    "inventory" => format!(
                        "{} resident models observed; recovery compatibility unverified",
                        probe["value"]["resident_count"]
                    ),
                    "loaded_models" => format!(
                        "{} models observed",
                        probe["value"].as_array().map_or(0, Vec::len)
                    ),
                    "server" => format!("running={}", probe["value"]["running"]),
                    _ => "compatible probe".into(),
                }
            } else {
                probe["error"]
                    .as_str()
                    .or(probe["detail"].as_str())
                    .unwrap_or("unknown; not tested")
                    .to_string()
            };
            lines.push(format!("{label} {field}: {text}"));
        }
    }
    lines.join("\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Mock(Vec<&'static str>);
    impl Transport for Mock {
        fn request(&mut self, path: &str, body: Option<Value>) -> Result<Vec<u8>> {
            assert!(body.is_none(), "Doctor must never send a mutation");
            self.0.push(if path == "/api/version" {
                "version"
            } else {
                assert_eq!(path, "/api/ps");
                "inventory"
            });
            if path == "/api/version" {
                bail!("Unavailable version")
            }
            Ok(br#"{"models":[]}"#.to_vec())
        }
    }
    #[test]
    fn ollama_probes_are_independent_and_read_only() {
        let mut mock = Mock(Vec::new());
        let report = ollama(&mut mock);
        assert_eq!(mock.0, ["version", "inventory"]);
        assert_eq!(report["version"]["ok"], false);
        assert_eq!(report["inventory"]["ok"], true);
        assert_eq!(report["inventory"]["value"]["resident_count"], 0);
        assert_eq!(report["compatibility"], "unverified");
    }
    #[test]
    fn version_requires_bounded_printable_evidence() {
        assert_eq!(version(br#"{"version":"0.35.1"}"#).unwrap(), "0.35.1");
        for body in [
            json!({}),
            json!({"version":42}),
            json!({"version":""}),
            json!({"version":"bad\nvalue"}),
            json!({"version":"x".repeat(129)}),
        ] {
            assert!(version(body.to_string().as_bytes()).is_err());
        }
        assert!(version(&vec![b' '; ollama_contract::MAX_RESPONSE_BYTES + 1]).is_err());
    }
    #[test]
    fn malformed_inventory_does_not_hide_observed_version() {
        let (endpoint, worker) = crate::ollama_session::tests::http_server(2, |path, body| {
            assert!(body.is_none());
            (
                200,
                match path {
                    "/api/version" => br#"{"version":"future-build"}"#.to_vec(),
                    "/api/ps" => br#"{"models":"unknown-shape"}"#.to_vec(),
                    _ => panic!("Unexpected diagnostic route"),
                },
            )
        });
        let report = ollama(&mut Http::new(&endpoint).unwrap());
        worker.join().unwrap();
        assert_eq!(report["version"]["ok"], true);
        assert_eq!(report["version"]["value"], "future-build");
        assert_eq!(report["inventory"]["ok"], false);
        assert_eq!(report["compatibility"], "unverified");
    }
    #[test]
    fn advanced_details_label_missing_cached_and_changed_configuration() {
        let mut shared = crate::app::Shared::default();
        shared
            .config
            .providers
            .retain(|provider| matches!(provider, Provider::Ollama { .. }));
        if let Provider::Ollama { enabled, .. } = &mut shared.config.providers[0] {
            *enabled = true;
        }
        assert!(render(&shared).contains("no diagnostic evidence"));
        shared.doctor_report = Some(
            json!({"providers":[{"id":"ollama-main", "endpoint":"127.0.0.1:11434", "enabled":true,
            "observed_at_unix_seconds":0,"version":{"ok":false,"error":"missing service"},"inventory":{"ok":null}}]}),
        );
        let rendered = render(&shared);
        assert!(rendered.contains("1970-01-01 00:00:00 UTC"));
        assert!(rendered.contains("missing service"));
        assert!(rendered.contains("unknown; not tested"));
        assert!(rendered.contains("Service state may have changed"));
        if let Provider::Ollama { endpoint, .. } = &mut shared.config.providers[0] {
            *endpoint = "127.0.0.1:11435".into();
        }
        assert!(render(&shared).contains("stale configuration"));
        shared.doctor_pending = true;
        assert!(render(&shared).contains("diagnostics running"));
        if let Provider::Ollama { enabled, .. } = &mut shared.config.providers[0] {
            *enabled = false;
        }
        assert!(render(&shared).contains("disabled; not probed"));
        assert!(!render(&shared).contains("missing service"));
    }
}
