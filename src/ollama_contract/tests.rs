use super::*;
use serde_json::{Value, json};

fn model(name: &str, digit: char) -> Value {
    json!({"name": name, "model": name, "digest": digit.to_string().repeat(64),
            "context_length": 4096, "expires_at": "2026-10-05T12:00:00Z"})
}
fn inventory(models: Vec<Value>) -> ResidentInventory {
    parse_resident_inventory(json!({"models": models}).to_string().as_bytes()).unwrap()
}
fn catalog(models: Vec<Value>) -> Catalog {
    parse_catalog(json!({"models": models}).to_string().as_bytes()).unwrap()
}
fn show() -> Value {
    json!({"details":{"format":"gguf"}, "capabilities":["completion"],
            "model_info":{"general.architecture":"fixture"}})
}

#[test]
fn malformed_or_unknown_inventory_never_becomes_an_empty_success() {
    for body in [
        "",
        "{}",
        "null",
        "[]",
        "{\"models\":null}",
        "{\"models\":[] } trailing",
        "{\"models\":[],\"models\":[]}",
        "{\"models\":[],\"error\":\"fixture error\"}",
    ] {
        assert!(parse_resident_inventory(body.as_bytes()).is_err(), "{body}");
    }
    assert!(
        parse_resident_inventory(br#"{"models":[]}"#.as_slice())
            .unwrap()
            .0
            .is_empty()
    );
    let mut invalid = model("fixture:latest", 'a');
    invalid["context_length"] = json!(-1);
    assert!(parse_resident_inventory(json!({"models":[invalid]}).to_string().as_bytes()).is_err());
}

#[test]
fn read_and_model_limits_refuse_the_whole_response() {
    struct Endless(usize);
    impl Read for Endless {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            bytes.fill(b' ');
            self.0 += bytes.len();
            Ok(bytes.len())
        }
    }
    let mut reader = Endless(0);
    assert!(parse_resident_inventory(&mut reader).is_err());
    assert_eq!(reader.0, MAX_RESPONSE_BYTES + 1);
    let models = (0..=MAX_MODELS)
        .map(|i| model(&format!("fixture-{i}:latest"), 'a'))
        .collect::<Vec<_>>();
    let body = json!({"models":models}).to_string();
    assert!(parse_resident_inventory(body.as_bytes()).is_err());
    assert!(parse_catalog(body.as_bytes()).is_err());
    assert!(
        classify_show(
            std::io::repeat(b' '),
            &catalog(vec![model("fixture:latest", 'a')]).0["fixture:latest"]
        )
        .is_err()
    );
}

#[test]
fn ambiguous_names_digests_and_duplicate_routes_are_rejected() {
    for (field, value) in [
        ("name", ""),
        ("name", "fixture other"),
        ("model", "other:latest"),
        ("digest", "unknown"),
        ("digest", "sha256:zzz"),
    ] {
        let mut invalid = model("fixture:latest", 'a');
        invalid[field] = json!(value);
        let body = json!({"models":[invalid]}).to_string();
        assert!(parse_resident_inventory(body.as_bytes()).is_err());
        assert!(parse_catalog(body.as_bytes()).is_err());
    }
    let body =
        json!({"models":[model("fixture:latest", 'a'), model("fixture:latest", 'b')]}).to_string();
    assert!(parse_catalog(body.as_bytes()).is_err());
    assert!(parse_resident_inventory(body.as_bytes()).is_err());
}

#[test]
fn catalog_identity_never_substitutes_tags_content_or_remote_aliases() {
    let saved = inventory(vec![model("fixture:latest", 'a')]);
    let resident = &saved.0["fixture:latest"];
    assert!(
        verify_catalog_identity(resident, &catalog(vec![model("fixture:latest", 'a')])).is_ok()
    );
    for models in [
        vec![],
        vec![model("fixture:latest", 'b')],
        vec![model("alias:latest", 'a')],
    ] {
        assert!(verify_catalog_identity(resident, &catalog(models)).is_err());
    }
    let mut remote = model("fixture:latest", 'a');
    remote["remote_model"] = json!("remote-fixture");
    assert!(verify_catalog_identity(resident, &catalog(vec![remote])).is_err());
}

#[test]
fn show_remote_aliases_and_unsupported_capabilities_never_authorize_control() {
    let catalog = catalog(vec![model("fixture:latest", 'a')]);
    let entry = &catalog.0["fixture:latest"];
    let classify = |value: Value| classify_show(value.to_string().as_bytes(), entry).unwrap();
    assert_eq!(classify(show()), ModelEvidence::LocalCompletionCandidate);
    for field in ["remote_host", "remote_model"] {
        let mut remote = show();
        remote[field] = json!("remote-fixture");
        let evidence = classify(remote);
        assert_eq!(evidence, ModelEvidence::Remote);
        assert_eq!(evidence.replay_block(), ReplayBlock::Remote);
    }
    let mut embed = show();
    embed["capabilities"] = json!(["embedding"]);
    let evidence = classify(embed);
    assert_eq!(evidence, ModelEvidence::Embedding);
    assert_eq!(
        evidence.replay_block(),
        ReplayBlock::EmbeddingContractUnresolved
    );
    for caps in [
        json!(["completion", "vision"]),
        json!(["tools", "thinking", "completion"]),
        json!(["completion", "future-capability"]),
    ] {
        let mut wider = show();
        wider["capabilities"] = caps;
        assert_eq!(classify(wider), ModelEvidence::LocalCompletionCandidate);
    }
    let mut both = show();
    both["capabilities"] = json!(["completion", "embedding"]);
    assert_eq!(classify(both), ModelEvidence::Embedding);
    for caps in [
        json!([]),
        json!(["unknown"]),
        json!(["vision"]),
        Value::Null,
    ] {
        let mut unknown = show();
        unknown["capabilities"] = caps;
        let evidence = classify(unknown);
        assert_eq!(evidence, ModelEvidence::Unknown);
        assert_eq!(evidence.replay_block(), ReplayBlock::LocalityUnknown);
    }
    assert_eq!(classify(json!({})), ModelEvidence::Unknown);
    assert_eq!(
        classify(show()).replay_block(),
        ReplayBlock::AdapterNotIntegrated
    );
}

#[test]
fn missing_options_context_and_expiry_remain_uninterpreted_evidence() {
    let mut wire = model("fixture:latest", 'a');
    wire.as_object_mut().unwrap().remove("context_length");
    wire.as_object_mut().unwrap().remove("expires_at");
    let saved = inventory(vec![wire]);
    assert_eq!(saved.0["fixture:latest"].context_length, None);
    assert_eq!(saved.0["fixture:latest"].expires_at, None);
    assert_eq!(
        compare_residency(&saved, &saved),
        [ResidencyIssue::ContextUnverified("fixture:latest".into())]
    );
    for expiry in [
        "0001-01-01T00:00:00Z",
        "9999-12-31T23:59:59Z",
        "not-a-timestamp",
        "2026-10-05T12:00:00+02:00",
    ] {
        let mut wire = model("fixture:latest", 'a');
        wire["expires_at"] = json!(expiry);
        let saved = inventory(vec![wire]);
        assert_eq!(
            saved.0["fixture:latest"].expires_at.as_deref(),
            Some(expiry)
        );
    }
    // A catalogue's parameters or defaults are never live runner options.
    assert_eq!(
        ModelEvidence::LocalCompletionCandidate.replay_block(),
        ReplayBlock::AdapterNotIntegrated
    );
}

#[test]
fn final_set_verification_catches_eviction_and_context_mismatch() {
    let saved = inventory(vec![
        model("first:latest", 'a'),
        model("second:latest", 'b'),
    ]);
    assert!(compare_residency(&saved, &saved).is_empty());
    let last_load = inventory(vec![model("second:latest", 'b')]);
    assert_eq!(
        compare_residency(&saved, &last_load),
        [ResidencyIssue::Missing("first:latest".into())]
    );
    let mut changed = model("second:latest", 'b');
    changed["context_length"] = json!(8192);
    let observed = inventory(vec![model("first:latest", 'c'), changed]);
    assert_eq!(
        compare_residency(&saved, &observed),
        [
            ResidencyIssue::IdentityChanged("first:latest".into()),
            ResidencyIssue::ContextUnverified("second:latest".into())
        ]
    );
    assert_eq!(saved.0["first:latest"].identity.digest, "a".repeat(64));
}

#[test]
fn acknowledgement_or_alias_reload_cannot_establish_absence() {
    let saved = inventory(vec![model("fixture:latest", 'a')]);
    assert!(!captured_models_absent(&saved, &saved));
    assert!(!captured_models_absent(
        &saved,
        &inventory(vec![model("alias:latest", 'a')])
    ));
    assert!(captured_models_absent(&saved, &inventory(vec![])));
    assert!(!captured_models_absent(&saved, &saved)); // External reload after a prior absence.
    assert_eq!(saved.0.len(), 1); // Original evidence is never replaced or accumulated.
}

fn capture_with(wire: Value, details: Value, policy: Policy) -> Result<ReplayCandidate> {
    let saved = inventory(vec![wire.clone()]);
    let catalog = catalog(vec![wire]);
    ReplayCandidate::capture(
        saved.0.values().next().unwrap(),
        &catalog,
        details.to_string().as_bytes(),
        clock(),
        policy,
    )
}
fn capture(wire: Value, details: Value) -> Result<ReplayCandidate> {
    capture_with(wire, details, ABSOLUTE)
}
const ABSOLUTE: Policy = Policy::AbsoluteDeadline;
const FROZEN: Policy = Policy::FrozenRemaining;
fn clock() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_791_201_300)
}
fn completion_details() -> Value {
    let mut details = show();
    details["model_info"]["fixture.context_length"] = json!(8192);
    details
}
fn ack(reason: &str) -> Value {
    json!({"model":"fixture:latest:local", "done":true, "done_reason":reason, "response":""})
}

#[test]
fn limited_capture_uses_observed_per_slot_context_and_explicit_local_route() {
    let candidate = capture(model("fixture:latest", 'a'), completion_details()).unwrap();
    assert_eq!(
        candidate.unload_request().unwrap(),
        json!({"model":"fixture:latest:local",
            "prompt":"", "stream":false, "keep_alive":0})
    );
    let request = candidate
        .preload_request(clock(), Some(std::time::Duration::ZERO), ABSOLUTE)
        .unwrap()
        .unwrap();
    assert_eq!(request["options"], json!({"num_ctx":4096}));
    assert_eq!(request["keep_alive"], "300000000000ns");
    assert_eq!(request["prompt"], "");
    assert_eq!(request["model"], "fixture:latest:local");
    assert!(request.get("messages").is_none());
    assert_eq!(
        candidate
            .preload_request(
                clock() + std::time::Duration::from_secs(300),
                None,
                ABSOLUTE
            )
            .unwrap(),
        None
    );
    // Inventory reports per-slot context, not a context to divide or
    // multiply by a guessed parallel setting. No parallel option is sent.
    for observed in [4, 4096, 8192] {
        let mut wire = model("fixture:latest", 'a');
        wire["context_length"] = json!(observed);
        let request = capture(wire, completion_details())
            .unwrap()
            .preload_request(clock(), None, ABSOLUTE)
            .unwrap()
            .unwrap();
        assert_eq!(request["options"]["num_ctx"], observed);
    }
}

#[test]
fn unsupported_context_embedding_cloud_and_expiry_refuse_candidate_capture() {
    for observed in [0u64, 3, 8193, u64::MAX] {
        let mut wire = model("fixture:latest", 'a');
        wire["context_length"] = json!(observed);
        assert!(capture(wire, completion_details()).is_err());
    }
    for capabilities in [json!(["embedding"]), json!(["vision"]), json!([])] {
        let mut details = completion_details();
        details["capabilities"] = capabilities;
        assert!(capture(model("fixture:latest", 'a'), details).is_err());
    }
    for name in [
        "fixture:cloud",
        "fixture:8b-cloud",
        "fixture:LOCAL",
        "fixture",
    ] {
        assert!(local_reference(name).is_err());
        assert!(capture(model(name, 'a'), completion_details()).is_err());
    }
    let mut details = completion_details();
    details["remote_host"] = json!("remote-fixture");
    assert!(capture(model("fixture:latest", 'a'), details).is_err());
    let mut details = completion_details();
    details["model_info"]
        .as_object_mut()
        .unwrap()
        .remove("fixture.context_length");
    assert!(capture(model("fixture:latest", 'a'), details).is_err());
    let mut wire = model("fixture:latest", 'a');
    wire["expires_at"] = Value::Null;
    assert!(capture(wire, completion_details()).is_err());
}

#[test]
fn acknowledgement_refuses_inference_remote_errors_and_partial_responses() {
    assert!(
        verify_acknowledgement(
            ack("load").to_string().as_bytes(),
            "fixture:latest:local",
            false
        )
        .is_ok()
    );
    for (key, value) in [
        ("done", json!(false)),
        ("done_reason", json!("stop")),
        ("model", json!("another:latest:local")),
        ("response", json!("unexpected inference")),
        ("error", json!("fixture error")),
        ("remote_model", json!("remote-fixture")),
    ] {
        let mut body = ack("unload");
        body[key] = value;
        assert!(
            verify_acknowledgement(body.to_string().as_bytes(), "fixture:latest:local", true)
                .is_err()
        );
    }
    assert!(verify_acknowledgement(b"{}".as_slice(), "fixture:latest:local", true).is_err());
}

#[test]
fn isolated_http_acknowledgement_waits_for_inventory_and_detects_reload() {
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        time::{Duration, Instant},
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let busy = json!({"models":[model("fixture:latest", 'a')]}).to_string();
    let absent = json!({"models":[]}).to_string();
    let reloaded = json!({"models":[model("alias:latest", 'a')]}).to_string();
    // A source-inspired mock keeps the runner resident after acknowledgement.
    // This tests our consumer's decisions, not the vendor scheduler itself.
    let responses = [
        ack("unload").to_string(),
        busy.clone(),
        busy,
        absent,
        reloaded,
    ];
    let server = std::thread::spawn(move || {
        let end = Instant::now() + Duration::from_secs(5);
        let mut requests = Vec::new();
        for body in responses {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end =>
                    {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("fixture accept failed: {e}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut length = 0usize;
            let mut header_bytes = request.len();
            loop {
                let mut line = String::new();
                assert_ne!(
                    reader.read_line(&mut line).unwrap(),
                    0,
                    "fixture header ended early"
                );
                header_bytes += line.len();
                assert!(header_bytes <= 8192);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
            }
            assert!(length <= 4096);
            let mut input = vec![0; length];
            reader.read_exact(&mut input).unwrap();
            requests.push((request, input));
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        }
        requests
    });
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(1))
        .build();
    let saved = inventory(vec![model("fixture:latest", 'a')]);
    let candidate = capture(model("fixture:latest", 'a'), completion_details()).unwrap();
    let response = agent
        .post(&format!("{url}/api/generate"))
        .send_json(candidate.unload_request().unwrap())
        .unwrap();
    verify_acknowledgement(response.into_reader(), "fixture:latest:local", true).unwrap();
    for expected_absent in [false, false, true, false] {
        let response = agent.get(&format!("{url}/api/ps")).call().unwrap();
        let observed = parse_resident_inventory(response.into_reader()).unwrap();
        assert_eq!(captured_models_absent(&saved, &observed), expected_absent);
    }
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 5);
    assert!(requests[0].0.starts_with("POST /api/generate "));
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[0].1).unwrap(),
        candidate.unload_request().unwrap()
    );
    assert!(
        requests[1..]
            .iter()
            .all(|(line, body)| line.starts_with("GET /api/ps ") && body.is_empty())
    );
    assert_eq!(saved.0.len(), 1);
}

#[test]
fn frozen_capture_replays_remaining_time_and_indefinite_residency() {
    let candidate =
        capture_with(model("fixture:latest", 'a'), completion_details(), FROZEN).unwrap();
    candidate.validate(FROZEN).unwrap();
    let later = clock() + std::time::Duration::from_secs(7200);
    let request = candidate
        .preload_request(later, Some(std::time::Duration::from_secs(7200)), FROZEN)
        .unwrap()
        .unwrap();
    assert_eq!(request["keep_alive"], "300000000000ns");
    assert_eq!(request["options"], json!({"num_ctx":4096}));
    assert_eq!(
        candidate.frozen_residency(FROZEN),
        Some(std::time::Duration::from_secs(300))
    );
    assert_eq!(candidate.frozen_residency(ABSOLUTE), None);
    let mut wire = model("fixture:latest", 'a');
    wire["expires_at"] = json!("2319-01-16T01:01:52.618468107+02:00");
    assert!(capture(wire.clone(), completion_details()).is_err());
    let forever = capture_with(wire, completion_details(), FROZEN).unwrap();
    let request = forever
        .preload_request(later, None, FROZEN)
        .unwrap()
        .unwrap();
    assert_eq!(request["keep_alive"], -1);
    assert_eq!(request["model"], "fixture:latest:local");
    assert_eq!(forever.frozen_residency(FROZEN), None);
    // The live 0.35.1 capability set of a tool/thinking completion model.
    let mut details = completion_details();
    details["capabilities"] = json!(["tools", "thinking", "completion"]);
    assert!(capture_with(model("fixture:latest", 'a'), details, FROZEN).is_ok());
}

#[test]
fn unload_only_accepts_local_unreplayable_models_and_refuses_remote_or_changed() {
    let wire = model("embed:latest", 'e');
    let saved = inventory(vec![wire.clone()]);
    let resident = &saved.0["embed:latest"];
    let local = catalog(vec![wire.clone()]);
    let mut details = show();
    details["capabilities"] = json!(["embedding"]);
    let identity = unload_only(resident, &local, details.to_string().as_bytes()).unwrap();
    assert_eq!(
        unload_request(&identity).unwrap(),
        json!({"model":"embed:latest:local", "prompt":"", "stream":false, "keep_alive":0})
    );
    // Missing metadata is unknown, not remote: unloading stays possible.
    assert!(unload_only(resident, &local, b"{}".as_slice()).is_ok());
    let mut remote = details.clone();
    remote["remote_host"] = json!("remote-fixture");
    assert!(unload_only(resident, &local, remote.to_string().as_bytes()).is_err());
    assert!(
        unload_only(
            resident,
            &catalog(vec![model("embed:latest", 'f')]),
            details.to_string().as_bytes()
        )
        .is_err()
    );
    assert!(unload_only(resident, &catalog(vec![]), details.to_string().as_bytes()).is_err());
    let cloud = inventory(vec![model("fixture:cloud", 'a')]);
    assert!(
        unload_only(
            &cloud.0["fixture:cloud"],
            &catalog(vec![model("fixture:cloud", 'a')]),
            details.to_string().as_bytes()
        )
        .is_err()
    );
}

#[test]
fn resident_bytes_prefer_reported_memory_and_tolerate_missing_or_bad_bodies() {
    let body = json!({"models":[
        {"name":"a:latest", "size":10, "size_vram":7},
        {"name":"b:latest", "size":5, "size_vram":0},
        {"name":"c:latest"}
    ]});
    assert_eq!(resident_bytes(body.to_string().as_bytes()), 12);
    assert_eq!(resident_bytes(b"not json"), 0);
    assert_eq!(resident_bytes(br#"{"models":[]}"#), 0);
}
