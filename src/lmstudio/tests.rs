use super::{diagnostics::failing_field, websocket::receive_until, *};
use std::{
    io::Write,
    net::TcpStream,
    os::windows::process::CommandExt,
    process::{Command, Stdio},
    time::Instant,
};
use tungstenite::{Message, WebSocket};
#[test]
fn neutral_inventory_refuses_unknown_residency() {
    assert!(resident_keys(&[]).unwrap().is_empty());
    assert_eq!(
        resident_keys(&[json!({"identifier":"chat"}), json!({"identifier":"embed"})]).unwrap(),
        vec!["chat", "embed"]
    );
    for invalid in [
        json!({}),
        json!({"identifier":3}),
        json!({"identifier":" "}),
    ] {
        assert!(resident_keys(&[json!({"identifier":"chat"}), invalid]).is_err());
    }
    assert!(resident_keys(&[json!({"identifier":"chat"}), json!({"identifier":"chat"})]).is_err());
}
#[derive(Default)]
struct ServerOnly {
    events: Vec<&'static str>,
    fail_start: bool,
    fail_stop: bool,
}
#[test]
fn control_ownership_precedes_every_lm_mutation_and_survives_transport_replacement() {
    let claims = std::sync::Arc::new(std::sync::Mutex::new(crate::ownership::Claims::isolated()));
    claims
        .lock()
        .unwrap()
        .claim(
            "other",
            crate::provider::Kind::LMStudio,
            &["localhost:4321"],
        )
        .unwrap();
    let mut backend = LMStudio {
        configured_endpoint: "127.0.0.1:1234".into(),
        claims: claims.clone(),
        config: Config::default(),
        lms: PathBuf::new(),
        log_folder: None,
        cli_version: std::sync::OnceLock::new(),
        captured_bytes: 0,
    };
    let model = Model {
        identifier: "fixture".into(),
        model_key: "fixture/chat".into(),
        base_key: "fixture/chat".into(),
        namespace: "llm".into(),
        ttl_ms: None,
        load_config: json!({"fields":[]}),
        native_config: json!({}),
        stage: "unloaded".into(),
    };
    for result in [
        backend.snapshot().map(|_| ()),
        backend.start_server(1234),
        backend.ensure_server(1234),
        backend.stop_server(),
        backend.unload("fixture"),
        backend.restore(&model),
    ] {
        assert!(format!("{:#}", result.unwrap_err()).contains("already owned"));
    }
    // Only read-only calls could reach this invalid CLI; all mutations fail at ownership.
    assert!(backend.loaded().is_err());
    drop(backend);
    assert!(
        claims
            .lock()
            .unwrap()
            .claim(
                "replacement",
                crate::provider::Kind::LMStudio,
                &["localhost:5555"]
            )
            .is_err()
    );
    claims
        .lock()
        .unwrap()
        .claim(
            "other",
            crate::provider::Kind::LMStudio,
            &["localhost:4321"],
        )
        .unwrap();
}
#[test]
fn observation_refuses_control_without_claiming_the_provider() {
    let claims = std::sync::Arc::new(std::sync::Mutex::new(crate::ownership::Claims::isolated()));
    let backend = LMStudio {
        configured_endpoint: "127.0.0.1:1234".into(),
        claims: claims.clone(),
        config: Config {
            mode: "observe".into(),
            ..Default::default()
        },
        lms: PathBuf::new(),
        log_folder: None,
        cli_version: std::sync::OnceLock::new(),
        captured_bytes: 0,
    };
    assert!(backend.claim_control(None).is_err());
    claims
        .lock()
        .unwrap()
        .claim(
            "other",
            crate::provider::Kind::LMStudio,
            &["localhost:1234"],
        )
        .unwrap();
}
#[test]
fn captured_route_selection_retains_both_claims_without_starting_a_server() {
    let claims = std::sync::Arc::new(std::sync::Mutex::new(crate::ownership::Claims::isolated()));
    let mut backend = LMStudio {
        configured_endpoint: "127.0.0.1:1234".into(),
        claims: claims.clone(),
        config: Config::default(),
        lms: PathBuf::new(),
        log_folder: None,
        cli_version: std::sync::OnceLock::new(),
        captured_bytes: 0,
    };
    backend.select_control_port(4321).unwrap();
    assert_eq!(backend.config.lm_endpoint(), "127.0.0.1:4321");
    backend.claim_control(None).unwrap();
    for endpoint in ["localhost:1234", "localhost:4321"] {
        assert!(
            claims
                .lock()
                .unwrap()
                .claim("other", crate::provider::Kind::Ollama, &[endpoint])
                .is_err()
        );
    }
}
impl Backend for ServerOnly {
    fn snapshot(&mut self) -> Result<Snapshot> {
        unreachable!()
    }
    fn loaded(&mut self) -> Result<Vec<Value>> {
        unreachable!()
    }
    fn start_server(&mut self, _: u16) -> Result<()> {
        self.events.push("start");
        if self.fail_start {
            bail!("start failed")
        };
        Ok(())
    }
    fn stop_server(&mut self) -> Result<()> {
        self.events.push("stop");
        if self.fail_stop {
            bail!("stop failed")
        };
        Ok(())
    }
    fn unload(&mut self, _: &str) -> Result<()> {
        unreachable!()
    }
    fn restore(&mut self, _: &Model) -> Result<()> {
        unreachable!()
    }
    fn read_config(&mut self, _: &Model) -> Result<Value> {
        unreachable!()
    }
}
#[test]
fn temporary_capture_closes_server_on_success_and_failure() {
    let mut backend = ServerOnly::default();
    let value = capture_with_server(&mut backend, true, 1234, |b| {
        b.events.push("capture");
        Ok(42)
    })
    .unwrap();
    assert_eq!(value, 42);
    assert_eq!(backend.events, vec!["start", "capture", "stop"]);
    backend.events.clear();
    let result: Result<()> = capture_with_server(&mut backend, true, 1234, |b| {
        b.events.push("capture");
        bail!("capture failed")
    });
    assert!(result.is_err());
    assert_eq!(backend.events, vec!["start", "capture", "stop"]);
    backend.events.clear();
    capture_with_server(&mut backend, false, 1234, |b| {
        b.events.push("capture");
        Ok(())
    })
    .unwrap();
    assert_eq!(backend.events, vec!["capture"]);
}
#[test]
fn temporary_server_failures_do_not_report_successful_capture() {
    let mut backend = ServerOnly {
        fail_start: true,
        ..Default::default()
    };
    assert!(capture_with_server(&mut backend, true, 1234, |_| Ok(())).is_err());
    assert_eq!(backend.events, vec!["start", "stop"]);
    backend.fail_start = false;
    backend.fail_stop = true;
    assert!(capture_with_server(&mut backend, true, 1234, |_| Ok(())).is_err());
}
#[test]
fn identity_prefers_variant_then_file() {
    assert_eq!(
        resolved_key(&json!({"selectedVariant":"m@q4","path":"m"})).unwrap(),
        "m@q4"
    );
    assert_eq!(
        resolved_key(&json!({"path":"embed.gguf"})).unwrap(),
        "embed.gguf"
    );
}
#[test]
fn config_comparison_ignores_order_not_values() {
    let a = json!({"fields":[{"key":"gpu","value":1},{"key":"context","value":4096}]});
    let b = json!({"fields":[{"key":"context","value":4096},{"key":"gpu","value":1}]});
    assert!(compare_fields(&a, &b).is_ok());
    let b = json!({"fields":[{"key":"gpu","value":0}]});
    assert!(compare_fields(&a, &b).is_err());
}
#[test]
fn restored_variant_key_is_equivalent_but_wrong_quantization_is_not() {
    let model = Model {
        identifier: "chat".into(),
        model_key: "publisher/model@q4".into(),
        base_key: "publisher/model".into(),
        namespace: "llm".into(),
        ttl_ms: None,
        load_config: json!({"fields":[]}),
        native_config: json!({}),
        stage: "restoring".into(),
    };
    assert!(identity_matches(
        &model,
        &json!({"modelKey":"publisher/model", "selectedVariant":"publisher/model@q4"})
    ));
    assert!(identity_matches(
        &model,
        &json!({"modelKey":"publisher/model@q4", "path":"publisher/model"})
    ));
    assert!(!identity_matches(
        &model,
        &json!({"modelKey":"publisher/model", "selectedVariant":"publisher/model@q8"})
    ));
    assert_eq!(resolved_key(&json!({"modelKey":"publisher/model@q4", "indexedModelIdentifier":"publisher/model@provider/file.gguf"})).unwrap(), "publisher/model@q4");
}

// ── P2-2 acceptance: diagnostics() assembles the expected fields ─────────
#[test]
fn diagnostics_assembles_expected_fields() {
    // A running server, one loaded model, a writable data dir.
    let server = json!({"running": true, "port": 1234});
    let models = vec![json!({"identifier":"pub/mo@q8","modelKey":"pub/mo@q8","status":"loaded"})];
    let dir = std::env::temp_dir().join("gp_diag_ok");
    let _ = std::fs::create_dir_all(&dir);
    let ok = crate::lmstudio::data_dir_writable(&dir);
    let report = crate::lmstudio::diagnostics("v1.5.1 (x86_64)", &server, &models, &dir, ok);

    assert_eq!(report["lms_version"], "v1.5.1 (x86_64)");
    assert_eq!(report["server"]["running"], true);
    assert_eq!(report["server"]["port"], 1234);
    assert_eq!(report["model_count"], 1);
    assert_eq!(report["loaded_models"][0]["identifier"], "pub/mo@q8");
    assert_eq!(report["loaded_models"][0]["status"], "loaded");
    assert_eq!(report["data_dir"]["writable"], true);
    let _ = std::fs::remove_dir(&dir);
}

// The acceptance test names this explicitly: a path that is not a writable
// directory must report false. A *file* (not a dir) is the robust,
// OS-portable way to get that: you cannot create a probe file inside a file,
// so the open fails and writability is false. (A missing dir fails the same
// way.) This holds on Windows, unlike setting a read-only bit on a dir.
#[test]
fn data_dir_writable_is_false_for_a_non_writable_path() {
    let as_file = std::env::temp_dir().join("gp_diag_afile");
    std::fs::write(&as_file, b"not a dir").unwrap();
    assert!(
        !crate::lmstudio::data_dir_writable(&as_file),
        "a file masquerading as a data dir must not be reported writable"
    );
    // A genuinely missing directory is also not writable.
    let missing = std::env::temp_dir().join("gp_diag_definitely_missing_xyz");
    assert!(!crate::lmstudio::data_dir_writable(&missing));
    let _ = std::fs::remove_file(&as_file);
}

// ── P2-4: WS protocol logging — a failing field produces a line naming it ──
// The acceptance test. Drives the REAL field-compare path (`compare_fields`,
// the exact source of a verify-fields failure detail), extracts the failing
// field with the REAL `failing_field`, and asserts the REAL `ws_log_line`
// names it. So the field the user sees in the verify report is the field
// that lands in gamepause.log.
#[test]
fn failing_field_produces_a_log_line_naming_that_field() {
    // A restored config that drifts on `temperature` — the same mismatch the
    // P2-1 acceptance test exercises. compare_fields reports it.
    let expected = json!({"fields":[{"key":"temperature","value":0.7}]});
    let actual = json!({"fields":[{"key":"temperature","value":0.999}]});
    // The real failure detail, exactly as verify_round_trip would carry it
    // (with a model identifier prefix, as in `format!("{id}: {err}")`).
    let err = crate::lmstudio::compare_fields(&expected, &actual).unwrap_err();
    let detail = format!("chat: {err}");
    assert!(
        detail.contains("temperature"),
        "the real detail must name the field, got: {detail:?}"
    );

    // failing_field must pull out exactly that field from the real detail.
    let field =
        failing_field(&detail).expect("the failing field must be extractable from the detail");
    assert_eq!(field, "temperature");

    // The log line for that step names the field and carries the LM
    // Studio version, so it can be correlated with a protocol build.
    let line = crate::lmstudio::ws_log_line("v1.5.1 (x86_64)", "verify-fields", false, &detail);
    assert!(
        line.contains("failed:temperature"),
        "log line must name the failing field, got: {line:?}"
    );
    assert!(
        line.contains("lm=v1.5.1 (x86_64)"),
        "log line must carry the LM Studio version, got: {line:?}"
    );
    assert!(
        line.starts_with("ws verify-fields "),
        "log line must identify the step, got: {line:?}"
    );
}
// A passing step reports `success` — the outcome comes from the step's
// authoritative `ok`, never guessed from its detail text.
#[test]
fn successful_step_reports_success_regardless_of_detail() {
    let line = crate::lmstudio::ws_log_line("v1.5.1", "capture", true, "2 model(s) captured");
    assert!(
        line.ends_with("success"),
        "a successful step must report success, got: {line:?}"
    );
    assert!(
        !line.contains("failed"),
        "a success line must not say failed, got: {line:?}"
    );
}
// A failure that names no field (identity/TTL mismatch, missing instance)
// still records a failure, just without a `:field` suffix.
#[test]
fn failure_without_a_named_field_is_still_recorded() {
    let line = crate::lmstudio::ws_log_line(
        "v1.5.1",
        "restore",
        false,
        "Model identity/quantization mismatch; recovery retained",
    );
    assert!(
        line.contains("failed"),
        "a failure must be recorded, got: {line:?}"
    );
    assert!(
        !line.contains("failed:"),
        "no field was named, so the line must not carry a fake field, got: {line:?}"
    );
}
// The sink writes to the local gamepause.log (best-effort) and is a no-op
// on an unwritable directory — never an error, never a remote call.
#[test]
fn ws_log_is_local_only_and_best_effort() {
    let dir = std::env::temp_dir().join(format!("gp_wslog_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    crate::lmstudio::ws_log(&dir, "v1.5.1", "capture", true, "1 model(s) captured");
    let content = std::fs::read_to_string(dir.join("gamepause.log")).unwrap_or_default();
    assert!(
        content.contains("ws capture lm=v1.5.1 success"),
        "the log file must contain the ws line, got: {content:?}"
    );
    // Unwritable target (a file masquerading as a dir) must not panic.
    let as_file = std::env::temp_dir().join(format!("gp_wslog_afile_{}", std::process::id()));
    std::fs::write(&as_file, b"x").unwrap();
    crate::lmstudio::ws_log(&as_file, "v1.5.1", "restore", false, "boom");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&as_file);
}
#[test]
fn websocket_ping_traffic_cannot_extend_operation_deadline() {
    use std::net::TcpListener;
    use tungstenite::protocol::Role;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let worker = std::thread::spawn(move || {
        let (peer, _) = listener.accept().unwrap();
        let mut ws = WebSocket::from_raw_socket(peer, Role::Server, None);
        let end = Instant::now() + Duration::from_millis(250);
        while Instant::now() < end {
            if ws.send(Message::Ping(vec![1].into())).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let mut ws = WebSocket::from_raw_socket(stream, Role::Client, None);
    let start = Instant::now();
    assert!(receive_until(&mut ws, start + Duration::from_millis(80)).is_err());
    assert!(start.elapsed() < Duration::from_millis(240));
    drop(ws);
    worker.join().unwrap();
}
#[test]
fn protocol_operation_logging_records_failure_without_leaking_payload() {
    let mut backend = LMStudio {
        configured_endpoint: Config::default().lm_endpoint().into(),
        claims: Default::default(),
        config: Config::default(),
        lms: PathBuf::new(),
        log_folder: None,
        cli_version: std::sync::OnceLock::new(),
        captured_bytes: 0,
    };
    let folder =
        std::env::temp_dir().join(format!("gamepause-operation-log-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    backend.set_log_folder(folder.clone());
    backend.cli_version.set("test-cli".into()).unwrap();
    let result: Result<()> = backend.logged("getLoadConfig", || {
        bail!("Restored load field contextLength differs; token=secret")
    });
    assert!(result.is_err());
    let log = std::fs::read_to_string(folder.join("gamepause.log")).unwrap();
    assert!(log.contains("getLoadConfig lm=cli:test-cli failed:contextLength"));
    assert!(!log.contains("secret"));
}
#[test]
fn a_finished_command_is_not_held_open_by_a_process_it_started() {
    // The fixture exits at once and leaves a two-second descendant holding
    // the pipes, as `lms` does when it starts LM Studio.
    let started = Instant::now();
    let result = run_command(
        &std::env::current_exe().unwrap().to_string_lossy(),
        &[
            "--ignored",
            "--exact",
            "lmstudio::tests::command_descendant_fixture",
            "--nocapture",
        ],
        Duration::from_secs(10),
    );
    let output = String::from_utf8_lossy(&result.unwrap()).into_owned();
    assert!(output.contains("command_descendant_fixture"));
    assert!(started.elapsed() < Duration::from_millis(1800));
}
#[test]
#[ignore = "subprocess fixture: invoked only by command timeout regression"]
#[allow(clippy::zombie_processes)] // Deliberately outlives its parent to hold inherited pipes.
fn command_descendant_fixture() {
    // Intentionally inherit these pipes and outlive the direct child.
    let _child = Command::new("powershell.exe")
        .creation_flags(0x08000000)
        .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 2"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
}
#[test]
fn oversized_output_is_drained_and_reported_as_truncated() {
    let result = run_command(
        &std::env::current_exe().unwrap().to_string_lossy(),
        &[
            "--ignored",
            "--exact",
            "lmstudio::tests::command_output_fixture",
            "--nocapture",
        ],
        Duration::from_secs(5),
    );
    assert!(result.unwrap_err().to_string().contains("truncated"));
}
#[test]
#[ignore = "subprocess fixture: invoked only by output cap regression"]
fn command_output_fixture() {
    std::io::stdout()
        .write_all(&vec![b'x'; 4 * 1024 * 1024 + 65536])
        .unwrap();
}
#[test]
fn actual_config_transport_logs_success_and_protocol_failure() {
    use std::net::TcpListener;
    for valid in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let peer = std::thread::spawn(move || {
            let mut ws = tungstenite::accept(listener.accept().unwrap().0).unwrap();
            ws.read().unwrap(); // auth
            ws.send(Message::Text(json!({"success":true}).to_string().into()))
                .unwrap();
            ws.read().unwrap(); // getLoadConfig
            ws.send(Message::Text(json!({"type":"rpcResult","callId":1,"result":if valid { json!({"fields":[]}) } else { json!({"unexpected":"secret"}) }}).to_string().into())).unwrap();
            let _ = ws.read();
        });
        let folder = std::env::temp_dir().join(format!(
            "gamepause-transport-{}-{valid}",
            std::process::id()
        ));
        std::fs::create_dir_all(&folder).unwrap();
        let mut config = Config::default();
        config.lm_mut().unwrap().endpoint = address.to_string();
        let backend = LMStudio {
            configured_endpoint: config.lm_endpoint().into(),
            claims: Default::default(),
            config,
            lms: PathBuf::new(),
            log_folder: Some(folder.clone()),
            cli_version: std::sync::OnceLock::new(),
            captured_bytes: 0,
        };
        backend.cli_version.set("fixture".into()).unwrap();
        assert_eq!(backend.raw_config("llm", "instance").is_ok(), valid);
        peer.join().unwrap();
        let log = std::fs::read_to_string(folder.join("gamepause.log")).unwrap();
        assert!(log.contains(&format!(
            "getLoadConfig:llm:instance lm=cli:fixture {}",
            if valid { "success" } else { "failed" }
        )));
        assert!(!log.contains("secret"));
    }
}
#[test]
fn stopped_server_without_port_and_cli_banner_are_normalized() {
    assert_eq!(
        control_port(&json!({"running":false}), "127.0.0.1:4321").unwrap(),
        4321
    );
    assert!(control_port(&json!({"running":true,"port":65536}), "127.0.0.1:4321").is_err());
    assert_eq!(
        cli_version_tag(b"\x1b[31mLOGO\x1b[0m\nCLI commit: 69d945a\nDocs: ignored"),
        "CLI commit: 69d945a"
    );
}
