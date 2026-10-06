use gamepause_lmstudio::config::{self, Config, write_json};
use serde_json::json;
use std::{
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
static SERIAL: AtomicU64 = AtomicU64::new(0);
#[test]
fn enabled_ollama_is_left_alone_in_observation() {
    let fixture = Fixture::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let path = fixture.0.join("config.json");
    let mut config = Config::load(&path).unwrap();
    for provider in &mut config.providers {
        match provider {
            config::Provider::LMStudio { enabled, .. } => *enabled = false,
            config::Provider::Ollama {
                enabled,
                endpoint: route,
                ..
            } => {
                *enabled = true;
                *route = endpoint.clone();
            }
        }
    }
    config.validate().unwrap();
    write_json(&path, &config).unwrap();
    let before = std::fs::read(&path).unwrap();
    let output = fixture.run(&["--headless", "--observe", "--duration", "0.5"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(!fixture.0.join("state.json").exists());
}
#[test]
fn observation_watcher_uses_background_detection_without_touching_pending_recovery() {
    use gamepause_lmstudio::{
        lmstudio::Snapshot,
        recovery::{Binding, Intent, Journal},
    };
    let fixture = Fixture::new();
    let config = Config::load(&fixture.0.join("config.json")).unwrap();
    let snapshot = Snapshot {
        schema: 2,
        server: json!({"running":false,"port":1234}),
        server_stopped: false,
        pause_complete: false,
        games: vec![],
        models: vec![],
    };
    let journal = Journal::lm(
        Binding::capture(&config, &snapshot).unwrap(),
        snapshot,
        Intent::Reconcile,
    );
    let path = fixture.0.join("state.json");
    write_json(&path, &journal).unwrap();
    let before = std::fs::read(&path).unwrap();
    let output = fixture.run(&["--headless", "--observe", "--duration", "0.8"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
    let status: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.0.join("status.json")).unwrap()).unwrap();
    assert_eq!(status["mode"], "observe");
    assert_eq!(status["recovery_pending"], true);
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let folder = std::env::temp_dir().join(format!(
            "gamepause-cli-integration-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&folder).unwrap();
        let mut config = Config::default();
        config.lm_mut().unwrap().lms_path = folder.join("absent-lms.exe").to_string_lossy().into();
        // Tests that exercise Ollama enable it against a private endpoint;
        // the rest must not probe a developer's real service.
        for provider in &mut config.providers {
            if let config::Provider::Ollama { enabled, .. } = provider {
                *enabled = false;
            }
        }
        write_json(&folder.join("config.json"), &config).unwrap();
        Self(folder)
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_GamePauseCLI"))
            .creation_flags(0x08000000)
            .args(args)
            .arg("--data-dir")
            .arg(&self.0)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn verify_with_existing_lock_is_an_error_not_silent_success() {
    let f = Fixture::new();
    let _lock = config::lock(&f.0).unwrap();
    let output = f.run(&["--verify"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("already running"));
}
#[test]
fn status_distinguishes_live_lock_from_stale_cached_file() {
    let f = Fixture::new();
    write_json(
        &f.0.join("status.json"),
        &json!({"message":"AI available","active_games":[],"recovery_pending":false,
            "provider_outcomes":[{"kind":"ollama","id":"ollama-main","state":"failed",
                "guarantee":"supported_fields","pending":true,"error":"fixture\nerror","retry_seconds":3}]}),
    )
    .unwrap();
    let lock = config::lock(&f.0).unwrap();
    let output = f.run(&["--status"]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("active_games=[]"));
    assert!(text.contains("provider_evidence=cached_status"));
    assert!(text.contains("provider.ollama.pending=yes"));
    assert!(text.contains("provider.ollama.error=fixture\\nerror"));
    drop(lock);
    assert_eq!(
        String::from_utf8_lossy(&f.run(&["--status"]).stdout).trim(),
        "status=absent"
    );
}
#[test]
fn doctor_reports_missing_cli_and_unwritable_data_directory() {
    let f = Fixture::new();
    let output = f.run(&["--doctor"]);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["cli_version"]["ok"], false);
    assert_eq!(report["data_dir"]["writable"], true);
    let blocked = f.0.join("file-as-data-directory");
    std::fs::write(&blocked, "blocked").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_GamePauseCLI"))
        .creation_flags(0x08000000)
        .args(["--doctor", "--data-dir"])
        .arg(blocked)
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["data_dir"]["writable"], false);
    assert!(report["report_write_error"].is_string());
}
#[test]
fn doctor_reads_ollama_independently_and_preserves_configuration_and_recovery() {
    use std::io::{Read, Write};
    let fixture = Fixture::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let server = std::thread::spawn(move || {
        for expected in ["/api/version", "/api/ps"] {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "Missing diagnostic request"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() < 8192);
            }
            assert!(
                String::from_utf8(request)
                    .unwrap()
                    .starts_with(&format!("GET {expected} HTTP/1.1\r\n"))
            );
            let body = if expected == "/api/version" {
                r#"{"version":"0.35.1"}"#
            } else {
                r#"{"models":[]}"#
            };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    });
    let config_path = fixture.0.join("config.json");
    let mut config = Config::load(&config_path).unwrap();
    for provider in &mut config.providers {
        if let config::Provider::Ollama {
            enabled,
            endpoint: route,
            ..
        } = provider
        {
            *enabled = true;
            *route = endpoint.clone();
        }
    }
    write_json(&config_path, &config).unwrap();
    let before = std::fs::read(&config_path).unwrap();
    // Doctor must not even parse an unrelated pending journal.
    let journal = fixture.0.join("state.json");
    std::fs::write(&journal, "private pending recovery fixture").unwrap();
    let output = fixture.run(&["--doctor"]);
    server.join().unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["cli_version"]["ok"], false);
    let providers = report["providers"].as_array().unwrap();
    let ollama = providers
        .iter()
        .find(|provider| provider["kind"] == "ollama")
        .unwrap();
    assert_eq!(ollama["version"]["value"], "0.35.1");
    assert_eq!(ollama["inventory"]["value"]["resident_count"], 0);
    assert_eq!(ollama["compatibility"], "unverified");
    assert_eq!(ollama["evidence"], "read_only_probe");
    assert!(ollama["observed_at_unix_seconds"].as_u64().is_some());
    assert_eq!(std::fs::read(&config_path).unwrap(), before);
    assert_eq!(
        std::fs::read_to_string(journal).unwrap(),
        "private pending recovery fixture"
    );
}
#[test]
fn doctor_skips_disabled_provider_endpoints() {
    let fixture = Fixture::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let lm_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    lm_listener.set_nonblocking(true).unwrap();
    let mut config = Config::load(&fixture.0.join("config.json")).unwrap();
    for provider in &mut config.providers {
        match provider {
            config::Provider::LMStudio {
                enabled,
                connection,
                ..
            } => {
                *enabled = false;
                connection.endpoint = lm_listener.local_addr().unwrap().to_string();
            }
            config::Provider::Ollama {
                enabled, endpoint, ..
            } => {
                *enabled = false;
                *endpoint = listener.local_addr().unwrap().to_string();
            }
        }
    }
    write_json(&fixture.0.join("config.json"), &config).unwrap();
    let output = fixture.run(&["--doctor"]);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["configuration"]["ok"], true);
    assert_eq!(report["cli_version"]["ok"], serde_json::Value::Null);
    for provider in report["providers"].as_array().unwrap() {
        assert_eq!(provider["evidence"], "not_probed");
        assert_eq!(
            provider["observed_at_unix_seconds"],
            serde_json::Value::Null
        );
    }
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(
        lm_listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
#[test]
fn corrupt_recovery_and_observation_refuse_verification() {
    let f = Fixture::new();
    std::fs::write(f.0.join("state.json"), "{bad json").unwrap();
    assert!(!f.run(&["--verify"]).status.success());
    assert_eq!(
        std::fs::read_to_string(f.0.join("state.json")).unwrap(),
        "{bad json"
    );
    std::fs::remove_file(f.0.join("state.json")).unwrap();
    let output = f.run(&["--verify", "--observe"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Observe mode"));
}
#[test]
fn conflicting_cli_modes_are_rejected() {
    assert!(
        !Fixture::new()
            .run(&["--status", "--verify"])
            .status
            .success()
    );
}
#[test]
fn legacy_doctor_does_not_migrate_and_disabled_provider_retains_recovery() {
    let f = Fixture::new();
    write_json(&f.0.join("config.json"), &json!({"settings_version":2,"lms_path":f.0.join("absent-lms.exe").to_string_lossy(),"automation_enabled":false})).unwrap();
    let original = std::fs::read(f.0.join("config.json")).unwrap();
    let output = f.run(&["--doctor"]);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["configuration"]["ok"], true);
    assert_eq!(std::fs::read(f.0.join("config.json")).unwrap(), original);
    assert!(!f.0.join("config.v2.backup.json").exists());
    let mut config = Config::load(&f.0.join("config.json")).unwrap();
    if let config::Provider::LMStudio { enabled, .. } = &mut config.providers[0] {
        *enabled = false;
    }
    write_json(&f.0.join("config.json"), &config).unwrap();
    write_json(&f.0.join("state.json"), &json!({"schema":2,"server":{"running":false,"port":1234},"server_stopped":false,"models":[],"pause_complete":true})).unwrap();
    let journal = std::fs::read(f.0.join("state.json")).unwrap();
    for command in ["--restore", "--verify"] {
        let output = f.run(&[command]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("recovery is pending"));
        assert_eq!(std::fs::read(f.0.join("state.json")).unwrap(), journal);
    }
}
