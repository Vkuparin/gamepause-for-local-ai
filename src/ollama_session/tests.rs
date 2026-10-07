use super::*;
use crate::{
    coordinator::{Coordinator, State, Step, Store},
    recovery::Journal,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Write,
    net::TcpListener,
    rc::Rc,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
struct TestClock(Rc<Cell<(u64, u64)>>);
impl Clock for TestClock {
    fn wall(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(self.0.get().0)
    }
    fn monotonic(&self) -> Duration {
        Duration::from_secs(self.0.get().1)
    }
}
#[derive(Clone, Default)]
struct Disk(Arc<Mutex<Option<Vec<u8>>>>);
impl Disk {
    fn journal(&self) -> Option<Journal<Snapshot>> {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .map(|bytes| serde_json::from_slice(bytes).unwrap())
    }
}
impl Store<Snapshot> for Disk {
    fn save(&mut self, journal: Option<&Journal<Snapshot>>) -> Result<()> {
        *self.0.lock().unwrap() = journal.map(serde_json::to_vec).transpose()?;
        Ok(())
    }
}
#[derive(Clone)]
struct Fake {
    clock: TestClock,
    load_delay: u64,
    disk: Disk,
    resident: BTreeMap<String, Value>,
    catalog: BTreeMap<String, Value>,
    posts: Vec<Value>,
    fail: Option<(bool, bool)>, // unloading, failure after effect
    hold_unload: bool,
    down: bool,
    evict: bool,
    embedding: bool,
    changed_capture: bool,
    inventories: usize,
}
fn resident(name: &str, digit: char) -> Value {
    json!({"name":name, "model":name, "digest":digit.to_string().repeat(64),
            "context_length":4096, "expires_at":"1970-01-01T00:05:00Z"})
}
impl Transport for Fake {
    fn request(&mut self, path: &str, body: Option<Value>) -> Result<Vec<u8>> {
        if self.down {
            return Err(Unreachable.into());
        }
        let value = match path {
            "/api/ps" => {
                self.inventories += 1;
                if self.changed_capture && self.inventories == 2 {
                    self.resident.clear();
                }
                json!({"models":self.resident.values().collect::<Vec<_>>()})
            }
            "/api/tags" => json!({"models":self.catalog.values().collect::<Vec<_>>()}),
            "/api/show" => {
                assert!(
                    body.as_ref().unwrap()["model"]
                        .as_str()
                        .unwrap()
                        .ends_with(":local")
                );
                let name = body.unwrap()["model"].as_str().unwrap().to_string();
                let embedding = self.embedding || name.starts_with("fixture-embed");
                json!({"details":{"format":"gguf"},
                        "capabilities":[if embedding {"embedding"} else {"completion"}],
                        "model_info":{"general.architecture":"fixture", "fixture.context_length":8192}})
            }
            "/api/generate" => {
                let body = body.unwrap();
                assert_eq!(body["prompt"], "");
                assert_eq!(body["stream"], false);
                let route = body["model"].as_str().unwrap().to_string();
                let name = route.strip_suffix(":local").unwrap().to_string();
                let unloading = body["keep_alive"] == 0;
                let journal = self
                    .disk
                    .journal()
                    .expect("intent must be persisted before control");
                assert_eq!(
                    journal.session.intent,
                    if unloading {
                        Intent::Pause
                    } else {
                        Intent::Restore
                    }
                );
                let payload = &journal.providers[0].payload;
                assert!(
                    payload.models.iter().any(|model| {
                        model.original.resident().identity.name == name
                            && model.stage
                                == if unloading {
                                    Stage::Unloading
                                } else {
                                    Stage::Loading
                                }
                    }) || (unloading
                        && payload.unload_only.iter().any(|model| {
                            model.identity.name == name && model.stage == Stage::Unloading
                        }))
                );
                self.posts.push(body.clone());
                if self.fail == Some((unloading, false)) {
                    bail!("fixture pre-effect failure");
                }
                if unloading {
                    if !self.hold_unload {
                        self.resident.remove(&name);
                    }
                } else {
                    assert_eq!(body["options"]["num_ctx"], 4096);
                    assert!(
                        body["keep_alive"] == -1
                            || body["keep_alive"].as_str().unwrap().ends_with("ns")
                    );
                    if self.evict {
                        self.resident.clear();
                    }
                    self.resident
                        .insert(name.clone(), self.catalog[&name].clone());
                    let (wall, mono) = self.clock.0.get();
                    self.clock
                        .0
                        .set((wall + self.load_delay, mono + self.load_delay));
                }
                if self.fail == Some((unloading, true)) {
                    bail!("fixture response lost after effect");
                }
                json!({"model":route, "done":true, "done_reason":if unloading {"unload"} else {"load"}, "response":""})
            }
            _ => panic!("unexpected fixture path"),
        };
        Ok(serde_json::to_vec(&value).unwrap())
    }
}
fn binding() -> Binding {
    Binding {
        id: "ollama-main".into(),
        kind: Kind::Ollama,
        endpoint: "127.0.0.1:11434".into(),
        configured_endpoint: "127.0.0.1:11434".into(),
        payload_version: 1,
        guarantee: Guarantee::SupportedFields,
    }
}
type TestCoordinator = Coordinator<Adapter<Fake, TestClock>, Disk>;
/// Journals written before the frozen policy keep absolute-deadline replay.
fn legacy_fixture() -> TestCoordinator {
    let (mut adapter, disk, _) = fixture().into_parts();
    adapter.capture_policy = Policy::AbsoluteDeadline;
    Coordinator::new(
        adapter,
        disk,
        vec![binding()],
        None,
        Duration::from_secs(10),
    )
    .unwrap()
}
fn fixture() -> TestCoordinator {
    let disk = Disk::default();
    let models = [
        ("fixture-a:latest".into(), resident("fixture-a:latest", 'a')),
        ("fixture-b:latest".into(), resident("fixture-b:latest", 'b')),
    ]
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let clock = TestClock(Rc::new(Cell::new((100, 0))));
    let transport = Fake {
        clock: clock.clone(),
        load_delay: 0,
        disk: disk.clone(),
        resident: models.clone(),
        catalog: models,
        posts: vec![],
        fail: None,
        hold_unload: false,
        down: false,
        evict: false,
        embedding: false,
        changed_capture: false,
        inventories: 0,
    };
    Coordinator::new(
        Adapter::new(transport, clock, binding()).unwrap(),
        disk,
        vec![binding()],
        None,
        Duration::from_secs(10),
    )
    .unwrap()
}
fn drive(c: &mut TestCoordinator, intent: Intent, now: u64) {
    for _ in 0..8 {
        c.advance(intent, &[], Duration::from_secs(now), &mut || false)
            .unwrap();
    }
}
#[test]
fn a_new_capture_starts_its_budget_after_previous_session_and_idle_time() {
    let mut c = legacy_fixture();
    drive(&mut c, Intent::Pause, 0);
    drive(&mut c, Intent::Restore, 1);
    assert!(c.journal().is_none());
    let (mut adapter, disk, _) = c.into_parts();
    adapter.clock.0.set((700, 600));
    for model in adapter.transport.resident.values_mut() {
        model["expires_at"] = json!("1970-01-01T00:15:00Z");
    }
    adapter.transport.posts.clear();
    let mut c = Coordinator::new(
        adapter,
        disk,
        vec![binding()],
        None,
        Duration::from_secs(10),
    )
    .unwrap();
    drive(&mut c, Intent::Pause, 600);
    assert!(c.pause_complete());
    drive(&mut c, Intent::Restore, 601);
    assert!(c.journal().is_none());
    assert_eq!(c.runtime.transport.posts.len(), 4);
    for body in c.runtime.transport.posts.iter().skip(2) {
        assert_eq!(body["keep_alive"], "200000000000ns");
    }
}
#[test]
fn delayed_unload_needs_absence_and_restore_uses_remaining_budget() {
    let mut c = legacy_fixture();
    c.runtime.transport.hold_unload = true;
    drive(&mut c, Intent::Pause, 0);
    assert_eq!(c.statuses()["ollama-main"].state, State::Deferred);
    assert!(!c.pause_complete());
    assert_eq!(c.runtime.transport.posts.len(), 2);
    let originals = c.journal().unwrap().providers[0].payload.models.clone();
    let reads = c.runtime.transport.inventories;
    assert_eq!(
        c.advance(Intent::Pause, &[], Duration::from_secs(9), &mut || false)
            .unwrap(),
        Step::Idle
    );
    assert_eq!(c.runtime.transport.inventories, reads);
    c.runtime.transport.resident.clear(); // fixture inference finishes
    drive(&mut c, Intent::Pause, 10);
    assert!(c.pause_complete());
    assert_eq!(
        c.runtime.transport.posts.len(),
        2,
        "no repeated unload after acknowledgement"
    );
    c.runtime.clock.0.set((160, 60));
    drive(&mut c, Intent::Restore, 11);
    assert!(c.journal().is_none());
    assert!(c.store.journal().is_none());
    assert_eq!(c.runtime.transport.resident.len(), 2);
    for body in c.runtime.transport.posts.iter().skip(2) {
        assert_eq!(body["keep_alive"], "140000000000ns");
    }
    assert_eq!(
        originals[0].original.resident().expires_at.as_deref(),
        Some("1970-01-01T00:05:00Z")
    );
}
#[test]
fn control_failure_matrix_retains_originals_and_restart_recovers() {
    for unloading in [true, false] {
        for after in [true, false] {
            let mut c = fixture();
            if unloading {
                c.runtime.transport.fail = Some((true, after));
            }
            drive(&mut c, Intent::Pause, 0);
            let saved = c.store.journal().unwrap();
            let originals = saved.providers[0]
                .payload
                .models
                .iter()
                .map(|model| model.original.clone())
                .collect::<Vec<_>>();
            if !unloading {
                c.runtime.transport.fail = Some((false, after));
                drive(&mut c, Intent::Restore, 1);
            }
            assert_eq!(c.statuses()["ollama-main"].state, State::Failed);
            let saved = c.store.journal().unwrap();
            assert_eq!(
                saved.providers[0]
                    .payload
                    .models
                    .iter()
                    .map(|model| model.original.clone())
                    .collect::<Vec<_>>(),
                originals
            );
            let mut transport = c.runtime.transport.clone();
            transport.fail = None;
            let adapter = Adapter::new(transport, c.runtime.clock.clone(), binding()).unwrap();
            let mut resumed = Coordinator::new(
                adapter,
                c.store.clone(),
                vec![binding()],
                Some(saved),
                Duration::from_secs(10),
            )
            .unwrap();
            if unloading {
                drive(&mut resumed, Intent::Pause, 12);
                assert!(resumed.pause_complete());
            }
            drive(&mut resumed, Intent::Restore, 13);
            assert!(resumed.journal().is_none());
            assert_eq!(resumed.runtime.transport.resident.len(), 2);
        }
    }
}
#[test]
fn later_load_eviction_retains_recovery_and_retry_replays_the_final_set() {
    let mut c = fixture();
    drive(&mut c, Intent::Pause, 0);
    c.runtime.transport.evict = true;
    drive(&mut c, Intent::Restore, 1);
    assert_eq!(c.statuses()["ollama-main"].state, State::Failed);
    assert!(c.store.journal().is_some());
    let calls = c.runtime.transport.posts.len();
    c.runtime.transport.evict = false;
    drive(&mut c, Intent::Restore, 10);
    assert_eq!(
        c.runtime.transport.posts.len(),
        calls,
        "no retry before deadline"
    );
    drive(&mut c, Intent::Restore, 11);
    assert!(c.journal().is_none());
    assert_eq!(c.runtime.transport.resident.len(), 2);
}
#[test]
fn unsupported_or_changed_capture_never_sends_control() {
    for case in 1..4 {
        let mut c = legacy_fixture();
        match case {
            1 => c.runtime.transport.changed_capture = true,
            2 => {
                c.runtime
                    .transport
                    .catalog
                    .get_mut("fixture-a:latest")
                    .unwrap()["digest"] = json!("c".repeat(64))
            }
            _ => {
                c.runtime
                    .transport
                    .catalog
                    .get_mut("fixture-a:latest")
                    .unwrap()["remote_host"] = json!("remote-fixture")
            }
        }
        drive(&mut c, Intent::Pause, 0);
        assert_eq!(c.statuses()["ollama-main"].state, State::Failed);
        assert!(c.runtime.transport.posts.is_empty());
        assert!(c.store.journal().is_none());
    }
}
#[test]
fn expired_restart_resolves_without_load_and_cannot_resurrect() {
    let mut c = legacy_fixture();
    drive(&mut c, Intent::Pause, 0);
    let saved = c.store.journal().unwrap();
    c.runtime.clock.0.set((301, 201));
    let adapter = Adapter::new(
        c.runtime.transport.clone(),
        c.runtime.clock.clone(),
        binding(),
    )
    .unwrap();
    let mut resumed = Coordinator::new(
        adapter,
        c.store.clone(),
        vec![binding()],
        Some(saved),
        Duration::from_secs(10),
    )
    .unwrap();
    // Begin restore, then resolve one expired model and retain its outcome.
    resumed
        .advance(Intent::Restore, &[], Duration::ZERO, &mut || false)
        .unwrap();
    let saved = resumed.store.journal().unwrap();
    assert_eq!(saved.providers[0].payload.models[0].stage, Stage::Expired);
    let mut tampered = saved.providers[0].payload.clone();
    tampered.models[0].stage = Stage::Captured;
    assert!(
        resumed
            .runtime
            .validate_transition(&saved.providers[0].payload, &tampered)
            .is_err()
    );
    drive(&mut resumed, Intent::Restore, 1);
    assert!(resumed.journal().is_none());
    assert_eq!(
        resumed.runtime.transport.posts.len(),
        2,
        "only original unload requests"
    );
}
#[test]
fn game_guard_between_loads_retains_originals_and_repause_does_not_recapture() {
    let mut c = fixture();
    drive(&mut c, Intent::Pause, 0);
    let originals = c.journal().unwrap().providers[0]
        .payload
        .models
        .iter()
        .map(|model| model.original.clone())
        .collect::<Vec<_>>();
    c.advance(Intent::Restore, &[], Duration::from_secs(1), &mut || false)
        .unwrap();
    let posts = c.runtime.transport.posts.len();
    let bytes = c.store.0.lock().unwrap().clone();
    assert_eq!(
        c.advance(Intent::Restore, &[], Duration::from_secs(1), &mut || true)
            .unwrap(),
        Step::Interrupted
    );
    assert_eq!(*c.store.0.lock().unwrap(), bytes);
    assert_eq!(c.runtime.transport.posts.len(), posts);
    drive(&mut c, Intent::Pause, 2);
    assert!(c.pause_complete());
    assert_eq!(
        c.journal().unwrap().providers[0]
            .payload
            .models
            .iter()
            .map(|model| model.original.clone())
            .collect::<Vec<_>>(),
        originals
    );
    assert!(c.runtime.transport.resident.is_empty());
}
#[test]
fn changed_catalog_before_replay_refuses_control_and_keeps_original_identity() {
    let mut c = fixture();
    drive(&mut c, Intent::Pause, 0);
    let original = c.journal().unwrap().providers[0].payload.models[0]
        .original
        .clone();
    c.runtime
        .transport
        .catalog
        .get_mut("fixture-a:latest")
        .unwrap()["digest"] = json!("c".repeat(64));
    drive(&mut c, Intent::Restore, 1);
    assert_eq!(c.statuses()["ollama-main"].state, State::Failed);
    assert_eq!(
        c.runtime.transport.posts.len(),
        2,
        "only original unload requests"
    );
    assert_eq!(
        c.store.journal().unwrap().providers[0].payload.models[0].original,
        original
    );
}
#[test]
fn load_past_expiry_retains_read_only_reconciliation_across_restart() {
    for response_lost in [false, true] {
        let mut c = legacy_fixture();
        drive(&mut c, Intent::Pause, 0);
        c.runtime.transport.load_delay = 201;
        if response_lost {
            c.runtime.transport.fail = Some((false, true));
        }
        drive(&mut c, Intent::Restore, 1);
        assert!(c.journal().is_some());
        assert_eq!(c.runtime.transport.posts.len(), 3);
        let saved = c.store.journal().unwrap();
        let mut transport = c.runtime.transport.clone();
        transport.fail = None;
        let adapter = Adapter::new(transport, c.runtime.clock.clone(), binding()).unwrap();
        let mut resumed = Coordinator::new(
            adapter,
            c.store.clone(),
            vec![binding()],
            Some(saved),
            Duration::from_secs(10),
        )
        .unwrap();
        drive(&mut resumed, Intent::Restore, 2);
        assert!(
            resumed.store.journal().is_some(),
            "resident content cannot be declared expired and cleared"
        );
        assert_eq!(
            resumed.runtime.transport.posts.len(),
            3,
            "no load/unload fight after expiry"
        );
        resumed.runtime.transport.resident.clear(); // fixture reports natural expiration
        drive(&mut resumed, Intent::Restore, 12);
        assert!(resumed.journal().is_none());
        assert_eq!(resumed.runtime.transport.posts.len(), 3);
    }
}
#[test]
fn expiry_before_final_verification_cannot_clear_still_resident_content() {
    let mut c = legacy_fixture();
    drive(&mut c, Intent::Pause, 0);
    for _ in 0..2 {
        c.advance(Intent::Restore, &[], Duration::from_secs(1), &mut || false)
            .unwrap();
    }
    c.runtime.clock.0.set((301, 201));
    drive(&mut c, Intent::Restore, 1);
    assert_eq!(c.statuses()["ollama-main"].state, State::Failed);
    assert!(
        c.store.journal().unwrap().providers[0]
            .payload
            .models
            .iter()
            .any(|model| model.stage == Stage::ExpiryPending)
    );
    c.runtime.transport.resident.clear();
    drive(&mut c, Intent::Restore, 11);
    assert!(c.journal().is_none());
    assert_eq!(c.runtime.transport.posts.len(), 4);
}
#[test]
fn unknown_or_inconsistent_persisted_contract_is_refused() {
    let mut c = fixture();
    c.advance(Intent::Pause, &[], Duration::ZERO, &mut || false)
        .unwrap();
    let original = c.journal().unwrap().providers[0].payload.clone();
    for path in [
        "version",
        "expiry_policy",
        "source_revision",
        "deadline",
        "duplicate",
        "unload_stage",
        "unload_duplicate",
    ] {
        let mut value = serde_json::to_value(&original).unwrap();
        match path {
            "unload_stage" => {
                value["unload_only"] = json!([{"identity":{"name":"fixture-x:latest",
                        "digest":"d".repeat(64)}, "stage":"loading"}])
            }
            "unload_duplicate" => {
                value["unload_only"] = json!([{"identity":{"name":"fixture-a:latest",
                        "digest":"d".repeat(64)}, "stage":"captured"}])
            }
            "version" => value["version"] = json!(999),
            "expiry_policy" => value["expiry_policy"] = json!("future-policy"),
            "source_revision" => value["source_revision"] = json!("unknown"),
            "deadline" => {
                value["models"][0]["original"]["deadline"]["captured_remaining"]["secs"] =
                    json!(999)
            }
            _ => value["models"][1] = value["models"][0].clone(),
        }
        let parsed: Snapshot = serde_json::from_value(value).unwrap();
        assert!(parsed.validate().is_err(), "{path}");
    }
    let mut value = serde_json::to_value(original).unwrap();
    value["models"][0]["original"]["deadline"]["future"] = json!(true);
    assert!(serde_json::from_value::<Snapshot>(value).is_err());
}
pub(crate) fn http_server(
    count: usize,
    mut response: impl FnMut(&str, Option<Value>) -> (u16, Vec<u8>) + Send + 'static,
) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let thread = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        for _ in 0..count {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "fixture request did not arrive");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            // Windows accepted sockets inherit the listener's nonblocking mode.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = Vec::new();
            let (header_end, length) = loop {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
                assert!(bytes.len() <= 16 * 1024);
                if bytes.ends_with(b"\r\n\r\n") {
                    let header = String::from_utf8(bytes.clone()).unwrap();
                    let length = header
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    assert!(length <= 16 * 1024);
                    break (bytes.len(), length);
                }
            };
            bytes.resize(header_end + length, 0);
            stream.read_exact(&mut bytes[header_end..]).unwrap();
            let header = std::str::from_utf8(&bytes[..header_end]).unwrap();
            let path = header
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap();
            let body = (length > 0).then(|| serde_json::from_slice(&bytes[header_end..]).unwrap());
            let (status, body) = response(path, body);
            let location = if status == 302 {
                "Location: http://127.0.0.1:1/forbidden\r\n"
            } else {
                ""
            };
            write!(stream, "HTTP/1.1 {status} fixture\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n{location}\r\n", body.len()).unwrap();
            let _ = stream.write_all(&body); // capped clients may stop reading early
        }
    });
    (endpoint, thread)
}
#[test]
fn only_a_model_load_gets_the_long_request_timeout() {
    let unload = json!({"model":"m:latest", "prompt":"", "stream":false, "keep_alive":0});
    let finite =
        json!({"model":"m:latest", "prompt":"", "stream":false, "keep_alive":"240000000000ns"});
    let indefinite = json!({"model":"m:latest", "prompt":"", "stream":false, "keep_alive":-1});
    assert!(is_load("/api/generate", Some(&finite)));
    assert!(is_load("/api/generate", Some(&indefinite)));
    assert!(!is_load("/api/generate", Some(&unload)));
    assert!(!is_load("/api/show", Some(&finite)));
    assert!(!is_load("/api/ps", None));
    assert!(LOAD_TIMEOUT >= Duration::from_secs(300) && REQUEST_TIMEOUT < LOAD_TIMEOUT);
}
#[test]
fn a_slow_model_load_is_waited_for_while_other_requests_stay_short() {
    // The service answers after 700 ms; ordinary requests allow 250 ms
    // here and a load allows three seconds.
    let slow = |expected: &'static str| {
        http_server(1, move |path, _| {
            assert_eq!(path, expected);
            std::thread::sleep(Duration::from_millis(700));
            (200, br#"{"models":[]}"#.to_vec())
        })
    };
    let client = |endpoint: &str| {
        let mut transport = Http::new(endpoint).unwrap();
        transport.timeouts = (Duration::from_millis(250), Duration::from_secs(3));
        transport
    };
    let (endpoint, thread) = slow("/api/ps");
    assert!(client(&endpoint).request("/api/ps", None).is_err());
    // The fixture may fail to answer the abandoned request.
    let _ = thread.join();
    let unload = json!({"model":"m:latest", "prompt":"", "stream":false, "keep_alive":0});
    let (endpoint, thread) = slow("/api/generate");
    assert!(
        client(&endpoint)
            .request("/api/generate", Some(unload))
            .is_err(),
        "an unload is acknowledged at once and keeps the short limit"
    );
    let _ = thread.join();
    let load = json!({"model":"m:latest", "prompt":"", "stream":false, "keep_alive":-1});
    let (endpoint, thread) = slow("/api/generate");
    let started = Instant::now();
    assert!(
        client(&endpoint)
            .request("/api/generate", Some(load))
            .is_ok()
    );
    assert!(started.elapsed() >= Duration::from_millis(650));
    thread.join().unwrap();
}
#[test]
fn bounded_http_refuses_redirects_oversize_remote_routes_and_unknown_operations() {
    for (status, body) in [
        (302, vec![]),
        (200, vec![b'x'; contract::MAX_RESPONSE_BYTES + 2]),
    ] {
        let (endpoint, thread) = http_server(1, move |path, body_request| {
            assert_eq!(path, "/api/ps");
            assert!(body_request.is_none());
            (status, body.clone())
        });
        let mut transport = Http::new(&endpoint).unwrap();
        assert!(transport.request("/api/ps", None).is_err());
        thread.join().unwrap();
    }
    assert!(Http::new("example.invalid:11434").is_err());
    // A closed loopback port is the typed "not running" evidence.
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = closed.local_addr().unwrap().to_string();
    drop(closed);
    let error = Http::new(&endpoint)
        .unwrap()
        .request("/api/ps", None)
        .unwrap_err();
    assert!(error.downcast_ref::<Unreachable>().is_some());
    let mut transport = Http::new("127.0.0.1:1").unwrap();
    assert!(transport.request("/api/pull", Some(json!({}))).is_err());
}
#[test]
fn private_http_lifecycle_uses_the_adapter_and_persisted_control_intent() {
    let disk = Disk::default();
    let checked_disk = disk.clone();
    let model = resident("fixture-http:latest", 'c');
    let catalog_model = model.clone();
    let mut present = true;
    let mut controls = 0;
    let (endpoint, thread) = http_server(12, move |path, body| {
        let value = match path {
            "/api/ps" => json!({"models":if present { vec![model.clone()] } else { vec![] }}),
            "/api/tags" => json!({"models":[catalog_model.clone()]}),
            "/api/show" => json!({"details":{"format":"gguf"}, "capabilities":["completion"],
                    "model_info":{"general.architecture":"fixture", "fixture.context_length":8192}}),
            "/api/generate" => {
                let body = body.unwrap();
                let unloading = body["keep_alive"] == 0;
                assert_eq!(body["prompt"], "");
                assert_eq!(body["stream"], false);
                assert_eq!(body["model"], "fixture-http:latest:local");
                let journal = checked_disk.journal().unwrap();
                assert_eq!(
                    journal.session.intent,
                    if unloading {
                        Intent::Pause
                    } else {
                        Intent::Restore
                    }
                );
                assert_eq!(
                    journal.providers[0].payload.models[0].stage,
                    if unloading {
                        Stage::Unloading
                    } else {
                        Stage::Loading
                    }
                );
                controls += 1;
                assert!(controls <= 2);
                present = !unloading;
                json!({"model":body["model"], "done":true,
                        "done_reason":if unloading {"unload"} else {"load"}, "response":""})
            }
            _ => panic!("unexpected request"),
        };
        (200, serde_json::to_vec(&value).unwrap())
    });
    let mut binding = binding();
    binding.endpoint = endpoint.clone();
    binding.configured_endpoint = endpoint.clone();
    let clock = TestClock(Rc::new(Cell::new((100, 0))));
    let adapter = Adapter::new(Http::new(&endpoint).unwrap(), clock, binding.clone()).unwrap();
    let mut c =
        Coordinator::new(adapter, disk, vec![binding], None, Duration::from_secs(10)).unwrap();
    for _ in 0..5 {
        c.advance(Intent::Pause, &[], Duration::ZERO, &mut || false)
            .unwrap();
    }
    assert!(c.pause_complete());
    for _ in 0..5 {
        c.advance(Intent::Restore, &[], Duration::from_secs(1), &mut || false)
            .unwrap();
    }
    assert!(c.journal().is_none());
    assert!(c.store.journal().is_none());
    thread.join().unwrap();
}

#[test]
fn frozen_restore_replays_the_captured_remaining_time_after_a_long_pause() {
    let mut c = fixture();
    drive(&mut c, Intent::Pause, 0);
    assert!(c.pause_complete());
    assert_eq!(
        c.journal().unwrap().providers[0].payload.expiry_policy,
        crate::ollama_expiry::FROZEN_POLICY
    );
    // Three hours of gaming, far past the five-minute keep-alive.
    c.runtime.clock.0.set((100 + 10_800, 10_800));
    drive(&mut c, Intent::Restore, 10_800);
    assert!(c.journal().is_none());
    assert_eq!(c.runtime.transport.resident.len(), 2);
    assert_eq!(c.runtime.transport.posts.len(), 4);
    for body in c.runtime.transport.posts.iter().skip(2) {
        assert_eq!(body["keep_alive"], "200000000000ns");
    }
}
#[test]
fn frozen_restart_and_indefinite_residency_replay_without_a_clock_budget() {
    let mut c = fixture();
    for model in c.runtime.transport.resident.values_mut() {
        model["expires_at"] = json!("2262-01-01T00:00:00Z");
    }
    drive(&mut c, Intent::Pause, 0);
    let saved = c.store.journal().unwrap();
    // Restart with a clock before capture: the frozen policy reads no clock.
    c.runtime.clock.0.set((50, 0));
    let adapter = Adapter::new(
        c.runtime.transport.clone(),
        c.runtime.clock.clone(),
        binding(),
    )
    .unwrap();
    let mut resumed = Coordinator::new(
        adapter,
        c.store.clone(),
        vec![binding()],
        Some(saved),
        Duration::from_secs(10),
    )
    .unwrap();
    drive(&mut resumed, Intent::Restore, 1);
    assert!(resumed.journal().is_none());
    assert_eq!(resumed.runtime.transport.posts.len(), 4);
    for body in resumed.runtime.transport.posts.iter().skip(2) {
        assert_eq!(body["keep_alive"], -1);
    }
}
#[test]
fn unsupported_local_models_are_unloaded_reported_and_never_reloaded() {
    let mut c = fixture();
    let embed = resident("fixture-embed:latest", 'e');
    for map in [
        &mut c.runtime.transport.resident,
        &mut c.runtime.transport.catalog,
    ] {
        map.insert("fixture-embed:latest".into(), embed.clone());
    }
    // Unknown expiry also leaves only the unload half available.
    c.runtime
        .transport
        .resident
        .get_mut("fixture-b:latest")
        .unwrap()["expires_at"] = Value::Null;
    drive(&mut c, Intent::Pause, 0);
    assert!(c.pause_complete());
    assert!(c.runtime.transport.resident.is_empty());
    let saved = c.journal().unwrap().providers[0].payload.clone();
    assert_eq!(saved.models.len(), 1);
    assert_eq!(saved.units(), 3);
    assert_eq!(
        saved
            .unload_only
            .iter()
            .map(|model| model.identity.name.as_str())
            .collect::<Vec<_>>(),
        ["fixture-b:latest", "fixture-embed:latest"]
    );
    let mut tampered = saved.clone();
    tampered.unload_only[0].identity.digest = "f".repeat(64);
    assert!(saved.validate_transition(&tampered).is_err());
    assert_eq!(c.runtime.transport.posts.len(), 3);
    let note = "Unloaded without reload (outside the restore subset): \
                    fixture-b:latest, fixture-embed:latest.";
    assert_eq!(c.reports(Duration::ZERO)[0].note, note);
    drive(&mut c, Intent::Restore, 1);
    assert!(c.journal().is_none());
    let report = &c.reports(Duration::from_secs(1))[0];
    assert_eq!(
        (report.state, report.note.as_str()),
        (State::Restored, note)
    );
    assert_eq!(
        c.runtime
            .transport
            .resident
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["fixture-a:latest"]
    );
    assert_eq!(c.runtime.transport.posts.len(), 4);
}
#[test]
fn absent_service_is_a_quiet_empty_session_without_any_control() {
    let mut c = fixture();
    c.runtime.transport.down = true;
    drive(&mut c, Intent::Pause, 0);
    assert!(c.pause_complete());
    let report = &c.reports(Duration::ZERO)[0];
    assert_eq!(
        (report.state, report.note.as_str()),
        (State::Paused, NOT_RUNNING)
    );
    let saved = c.journal().unwrap().providers[0].payload.clone();
    assert!(saved.absent && saved.units() == 0);
    let mut tampered = saved.clone();
    tampered.absent = false;
    assert!(saved.validate_transition(&tampered).is_err());
    // The service appearing mid-session does not make this session touch it.
    c.runtime.transport.down = false;
    let reads = c.runtime.transport.inventories;
    drive(&mut c, Intent::Restore, 1);
    assert!(c.journal().is_none());
    assert_eq!(c.reports(Duration::from_secs(1))[0].state, State::Restored);
    assert_eq!(c.runtime.transport.inventories, reads);
    assert!(c.runtime.transport.posts.is_empty());
    assert_eq!(c.runtime.transport.resident.len(), 2);
}
#[test]
fn service_lost_after_capture_retains_recovery_instead_of_looking_absent() {
    let mut c = fixture();
    drive(&mut c, Intent::Pause, 0);
    c.runtime.transport.down = true;
    drive(&mut c, Intent::Restore, 1);
    assert_eq!(c.statuses()["ollama-main"].state, State::Failed);
    assert!(c.store.journal().is_some());
    c.runtime.transport.down = false;
    drive(&mut c, Intent::Restore, 11);
    assert!(c.journal().is_none());
    assert_eq!(c.runtime.transport.resident.len(), 2);
}
#[test]
fn held_unload_only_model_defers_pause_until_it_is_absent() {
    let mut c = fixture();
    c.runtime.transport.embedding = true;
    c.runtime.transport.hold_unload = true;
    drive(&mut c, Intent::Pause, 0);
    assert_eq!(c.statuses()["ollama-main"].state, State::Deferred);
    assert!(!c.pause_complete());
    assert!(c.journal().unwrap().providers[0].payload.models.is_empty());
    c.runtime.transport.resident.clear();
    drive(&mut c, Intent::Pause, 10);
    assert!(c.pause_complete());
    assert_eq!(c.runtime.transport.posts.len(), 2);
    drive(&mut c, Intent::Restore, 11);
    assert!(c.journal().is_none());
    assert!(c.runtime.transport.resident.is_empty());
}
#[test]
fn short_frozen_residency_ending_before_verification_is_expiry_not_eviction() {
    let mut c = fixture();
    drive(&mut c, Intent::Pause, 0);
    // Load both, then let the replayed 200 seconds run out before the check.
    for _ in 0..3 {
        c.advance(Intent::Restore, &[], Duration::from_secs(1), &mut || false)
            .unwrap();
    }
    assert_eq!(c.runtime.transport.resident.len(), 2);
    c.runtime.clock.0.set((400, 300));
    c.runtime.transport.resident.clear();
    drive(&mut c, Intent::Restore, 1);
    assert!(c.journal().is_none());
    assert_eq!(c.runtime.transport.posts.len(), 4, "no reload fight");
}
