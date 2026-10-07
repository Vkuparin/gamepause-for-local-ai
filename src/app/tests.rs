use super::detection::{DetectionFrame, evidence_from};
use super::*;
#[test]
fn log_lines_start_with_a_readable_local_time() {
    let stamp = local_timestamp();
    let bytes = stamp.as_bytes();
    assert_eq!(stamp.len(), 19);
    assert!(bytes[4] == b'-' && bytes[7] == b'-' && bytes[10] == b' ');
    assert!(bytes[13] == b':' && bytes[16] == b':');
    assert!(
        stamp
            .chars()
            .all(|c| c.is_ascii_digit() || "-: ".contains(c))
    );
}
#[test]
fn ask_answer_counts_whether_or_not_it_is_tracked() {
    assert!(Action::PauseForGame.answers_ask());
    assert!(
        Action::Tracked {
            id: 7,
            action: Box::new(Action::PauseForGame)
        }
        .answers_ask()
    );
    assert!(!Action::Pause.answers_ask());
    assert!(
        !Action::Tracked {
            id: 8,
            action: Box::new(Action::Refresh)
        }
        .answers_ask()
    );
}
#[test]
fn diagnostics_requests_coalesce_and_failed_dispatch_releases_pending() {
    let state = Arc::new(Mutex::new(Shared::default()));
    let (tx, rx) = mpsc::channel();
    request_action(&state, &tx, Action::Doctor, "Diagnostics");
    assert!(rx.try_recv().is_err());
    state.lock().unwrap().config.advanced_settings_visible = true;
    request_action(&state, &tx, Action::Doctor, "Diagnostics");
    request_action(&state, &tx, Action::Doctor, "Diagnostics");
    assert!(
        matches!(rx.try_recv(), Ok(Action::Tracked { action, .. }) if matches!(*action, Action::Doctor))
    );
    assert!(rx.try_recv().is_err());
    assert!(state.lock().unwrap().doctor_pending);
    state.lock().unwrap().doctor_pending = false;
    drop(rx);
    request_action(&state, &tx, Action::Doctor, "Diagnostics");
    assert!(!state.lock().unwrap().doctor_pending);
    assert_eq!(
        state
            .lock()
            .unwrap()
            .commands
            .latest
            .as_ref()
            .unwrap()
            .outcome,
        Outcome::Failed
    );
}
#[test]
fn manual_control_uses_background_guard_failure_instead_of_a_successful_fallback_scan() {
    let config = Config::default();
    let path = std::env::temp_dir().join(format!(
        "gamepause-background-guard-{}-{}.json",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut engine = Engine::new(
        config.clone(),
        OptionalBackend::new(config.clone(), None),
        path,
    )
    .unwrap();
    engine.activity = Activity::Watching;
    let state = Arc::new(Mutex::new(Shared {
        config: config.clone(),
        discovery_ready: true,
        detection_ok: true,
        active_mode: true,
        ..Default::default()
    }));
    let input = DetectionInput {
        power: Default::default(),
        power_generation: 0,
        config: config.clone(),
        games: vec![],
        guard_games: vec![],
        steam_roots: vec![],
        guarded: true,
        ready: true,
        ask_approved: false,
    };
    let worker = BackgroundDetection::start(
        input,
        |_| Duration::from_secs(10),
        |_| Err("fixture detection failure".into()),
        |_| {},
    )
    .unwrap();
    let mut scanner = Scanner::new(config).unwrap();
    apply_action_detected(
        Action::Pause,
        &mut engine,
        &std::env::temp_dir(),
        &mut scanner,
        &[],
        0.,
        &state,
        Some(&worker),
    )
    .unwrap();
    assert!(!engine.manual_pause);
    assert!(engine.backend.backend.is_none());
    assert!(!state.lock().unwrap().settings_error.is_empty());
}
#[test]
fn background_detection_publishes_during_control_and_rejects_old_settings() {
    let state = Arc::new(Mutex::new(Shared {
        config: Config::default(),
        ..Default::default()
    }));
    let game = crate::gameplay::fixtures::game(42, 10);
    let mut frame = DetectionFrame {
        power_generation: 0,
        config: Config::default(),
        asking: vec![],
        suggestion: None,
        active: vec![game.clone()],
        all: vec![game.clone()],
        evidence: Some(crate::gameplay::fixtures::evidence(vec![game.clone()])),
        inaccessible: 0,
        candidate: 0,
        lm_running: true,
        ollama_running: false,
        running_apps: vec![],
    };
    state.lock().unwrap().activity = Activity::Restoring;
    publish_detection(&state, &Ok(frame.clone()));
    assert_eq!(state.lock().unwrap().active_games, vec![game]);
    assert_eq!(state.lock().unwrap().activity, Activity::Restoring);
    assert!(state.lock().unwrap().detection_ok);
    state.lock().unwrap().power.notify(18);
    state.lock().unwrap().detection_ok = false;
    publish_detection(&state, &Ok(frame.clone()));
    assert!(
        !state.lock().unwrap().detection_ok,
        "pre-resume scan cannot restore availability"
    );
    frame.power_generation = state.lock().unwrap().power.snapshot().generation;
    publish_detection(&state, &Ok(frame.clone()));
    assert!(state.lock().unwrap().detection_ok);
    state.lock().unwrap().config.automation_enabled = false;
    frame.all.clear();
    publish_detection(&state, &Ok(frame));
    assert_eq!(
        state.lock().unwrap().active_games.len(),
        1,
        "old settings must not erase current detection"
    );
    publish_detection(&state, &Err("fixture scanner stopped".into()));
    assert!(!state.lock().unwrap().detection_ok);
    assert_eq!(
        state.lock().unwrap().active_games.len(),
        1,
        "failed detection retains last seen games"
    );
}
#[test]
fn power_change_during_fresh_detection_cannot_authorize_control() {
    let signal = Arc::new(crate::power::Signal::default());
    let input = DetectionInput {
        power: signal.clone(),
        power_generation: 0,
        config: Config::default(),
        games: vec![],
        guard_games: vec![],
        steam_roots: vec![],
        guarded: true,
        ready: true,
        ask_approved: false,
    };
    let worker = BackgroundDetection::start(
        input.clone(),
        |_| Duration::from_secs(30),
        |input| {
            input.power.notify(18);
            Ok(DetectionFrame {
                power_generation: input.power_generation,
                config: input.config.clone(),
                asking: vec![],
                suggestion: None,
                active: vec![],
                all: vec![],
                evidence: Some(crate::gameplay::fixtures::evidence(vec![])),
                inaccessible: 0,
                candidate: 0,
                lm_running: false,
                ollama_running: false,
                running_apps: vec![],
            })
        },
        |_| {},
    )
    .unwrap();
    assert!(fresh_detection(&worker, input).is_err());
    assert!(!signal.permits(0));
}
#[test]
fn resume_inventory_wait_holds_approval_for_revalidation_but_discovery_failure_revokes() {
    let config = Config::default();
    let path = std::env::temp_dir().join(format!(
        "gamepause-resume-hold-{}-{}.json",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut engine = Engine::new(
        config.clone(),
        OptionalBackend::new(config.clone(), None),
        path,
    )
    .unwrap();
    let current =
        crate::gameplay::fixtures::evidence(vec![crate::gameplay::fixtures::game(42, 10)]);
    engine.gameplay.refresh_offer(Some(&current), true);
    engine
        .gameplay
        .confirm(engine.gameplay.offer().unwrap().id, &current)
        .unwrap();
    engine.resume_detected();
    hold_for_discovery(&mut engine, false);
    assert!(engine.gameplay.active());
    assert!(engine.awaiting_resume_detection());
    assert_eq!(engine.activity, Activity::DetectionUnavailable);
    assert!(engine.gameplay.offer().is_none());
    hold_for_discovery(&mut engine, true);
    assert!(!engine.gameplay.active());
    assert!(engine.awaiting_resume_detection());
    assert!(engine.message.contains("discovery has errors"));
}
#[test]
fn native_background_detection_scans_during_a_blocked_control_operation() {
    let config = Config {
        mode: "observe".into(),
        ..Default::default()
    };
    let input = DetectionInput {
        power: Default::default(),
        power_generation: 0,
        config: config.clone(),
        games: vec![],
        guard_games: vec![],
        steam_roots: vec![],
        guarded: false,
        ready: true,
        ask_approved: false,
    };
    let mut native = NativeDetection::new(&config).unwrap();
    let (published, frames) = mpsc::channel();
    let worker = BackgroundDetection::start(
        input.clone(),
        |_| Duration::from_millis(20),
        move |input| native.scan(input).map_err(|error| format!("{error:#}")),
        move |frame| {
            if let Some(frame) = frame {
                published.send((Instant::now(), frame.is_ok())).unwrap();
            }
        },
    )
    .unwrap();
    fresh_detection(&worker, input).unwrap();
    let (release, held) = mpsc::channel();
    let control = std::thread::spawn(move || held.recv_timeout(Duration::from_secs(3)).unwrap());
    let mut observations = vec![];
    while observations.len() < 6 {
        let (time, valid) = frames.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(valid);
        observations.push(time);
    }
    assert!(!control.is_finished());
    let gap = observations
        .windows(2)
        .map(|pair| pair[1].duration_since(pair[0]))
        .max()
        .unwrap();
    eprintln!(
        "Native Toolhelp scans during blocked mock control: {} scans, maximum gap {:?}",
        observations.len(),
        gap
    );
    assert!(gap < Duration::from_secs(1));
    release.send(()).unwrap();
    control.join().unwrap();
}
#[test]
fn tracked_requests_do_not_publish_unsaved_settings_and_report_send_failures() {
    let state = Arc::new(Mutex::new(Shared::default()));
    let (tx, rx) = mpsc::channel();
    let updated = Config {
        restore_delay_seconds: 17.,
        ..Default::default()
    };
    request_action(
        &state,
        &tx,
        Action::Settings(Box::new(updated)),
        "Save settings",
    );
    assert_eq!(state.lock().unwrap().config.restore_delay_seconds, 30.);
    assert!(state.lock().unwrap().commands.settings_pending);
    request_action(&state, &tx, Action::Disable, "Toggle");
    assert_eq!(
        state
            .lock()
            .unwrap()
            .commands
            .latest
            .as_ref()
            .unwrap()
            .outcome,
        Outcome::NoChange
    );
    assert!(
        matches!(rx.try_recv(), Ok(Action::Tracked { action, .. }) if matches!(*action, Action::Settings(_)))
    );
    assert!(rx.try_recv().is_err());
    drop(rx);
    state.lock().unwrap().commands.settings_pending = false;
    request_action(&state, &tx, Action::Refresh, "Refresh");
    assert_eq!(
        state
            .lock()
            .unwrap()
            .commands
            .latest
            .as_ref()
            .unwrap()
            .outcome,
        Outcome::Failed
    );
    assert!(state.lock().unwrap().commands.request_refresh().is_some());
}
#[test]
fn selected_custom_removal_saves_one_entry_and_preserves_recovery_on_failure() {
    let folder =
        std::env::temp_dir().join(format!("gamepause-selected-removal-{}", std::process::id()));
    let config = Config {
        extra_games: vec![
            config::ExtraGame {
                name: "Fixture A".into(),
                path: r"D:\Fixture Games\a.exe".into(),
            },
            config::ExtraGame {
                name: "Fixture B".into(),
                path: r"D:\Fixture Games\b.exe".into(),
            },
        ],
        ..Default::default()
    };
    let remembered = Game::new("Custom", "a", "Fixture A", &config.extra_games[0].path);
    write_json(&folder.join("state.json"), &json!({"schema":2,"server":{"running":false,"port":1234},"server_stopped":false,"models":[],"pause_complete":true,"games":[remembered]})).unwrap();
    write_json(&folder.join("config.json"), &config).unwrap();
    let backend = OptionalBackend::new(config.clone(), None);
    let mut engine = Engine::new(config.clone(), backend, folder.join("state.json")).unwrap();
    let journal = fs::read(folder.join("state.json")).unwrap();
    let mut scanner = Scanner::new(config.clone()).unwrap();
    let state = Arc::new(Mutex::new(Shared {
        config: config.clone(),
        ..Default::default()
    }));
    let (tx, rx) = mpsc::channel();
    let launcher = Game::new(
        "Steam",
        "fixture",
        "Fixture launcher game",
        r"D:\Fixture Steam\game",
    );
    let games = vec![remembered.clone(), launcher.clone()];
    request_action(
        &state,
        &tx,
        Action::RemoveCustom {
            path: config.extra_games[0].path.to_uppercase(),
            name: "Fixture A".into(),
        },
        "Remove selected",
    );
    assert_eq!(state.lock().unwrap().config.extra_games.len(), 2);
    assert!(
        apply_action(
            rx.recv().unwrap(),
            &mut engine,
            &folder,
            &mut scanner,
            &games,
            0.,
            &state
        )
        .unwrap()
    );
    assert_eq!(
        Config::load(&folder.join("config.json"))
            .unwrap()
            .extra_games[0]
            .name,
        "Fixture B"
    );
    assert_eq!(engine.config.extra_games.len(), 1);
    assert_eq!(fs::read(folder.join("state.json")).unwrap(), journal);
    assert_eq!(
        recovery_games(&engine, &[launcher])[1].path,
        remembered.path
    );
    assert_eq!(
        state
            .lock()
            .unwrap()
            .commands
            .latest
            .as_ref()
            .unwrap()
            .outcome,
        Outcome::Completed
    );
    assert!(
        state
            .lock()
            .unwrap()
            .commands
            .latest
            .as_ref()
            .unwrap()
            .message
            .contains("Fixture A")
    );
    assert!(remove_custom(&engine.config, &games[1].path, &games[1].name).is_err());
    assert!(remove_custom(&engine.config, &config.extra_games[0].path, "Fixture A").is_err());
    fs::remove_file(folder.join("config.json")).unwrap();
    fs::create_dir(folder.join("config.json")).unwrap();
    request_action(
        &state,
        &tx,
        Action::RemoveCustom {
            path: config.extra_games[1].path.clone(),
            name: "Fixture B".into(),
        },
        "Remove selected",
    );
    assert!(
        !apply_action(
            rx.recv().unwrap(),
            &mut engine,
            &folder,
            &mut scanner,
            &games,
            0.,
            &state
        )
        .unwrap()
    );
    assert_eq!(engine.config.extra_games.len(), 1);
    assert_eq!(state.lock().unwrap().config.extra_games.len(), 1);
    assert_eq!(
        state
            .lock()
            .unwrap()
            .commands
            .latest
            .as_ref()
            .unwrap()
            .outcome,
        Outcome::Failed
    );
    assert_eq!(fs::read(folder.join("state.json")).unwrap(), journal);
    fs::remove_dir(folder.join("config.json")).unwrap();
    fs::remove_file(folder.join("state.json")).unwrap();
    fs::remove_file(folder.join("state.v2.backup.json")).unwrap();
    fs::remove_dir(folder).unwrap();
}
#[test]
fn manual_restore_waits_for_first_discovery_and_retains_journal() {
    let folder =
        std::env::temp_dir().join(format!("gamepause-startup-recovery-{}", std::process::id()));
    let path = folder.join("state.json");
    write_json(&path,&json!({"schema":2,"server":{"running":false,"port":1234},"server_stopped":false,"models":[],"pause_complete":true})).unwrap();
    let legacy = fs::read(&path).unwrap();
    let config = Config::default();
    let mut engine = Engine::new(
        config.clone(),
        OptionalBackend::new(config.clone(), None),
        path.clone(),
    )
    .unwrap();
    assert_eq!(
        fs::read(folder.join("state.v2.backup.json")).unwrap(),
        legacy
    );
    let before = fs::read(&path).unwrap();
    let mut scanner = Scanner::new(config).unwrap();
    let shared = Arc::new(Mutex::new(Shared::default()));
    apply_action(
        Action::Restore,
        &mut engine,
        &folder,
        &mut scanner,
        &[],
        0.,
        &shared,
    )
    .unwrap();
    assert!(engine.pending());
    assert_eq!(before, fs::read(&path).unwrap());
    fs::remove_file(path).unwrap();
    fs::remove_file(folder.join("state.v2.backup.json")).unwrap();
    fs::remove_dir(folder).unwrap();
}
#[test]
fn stale_pause_cannot_toggle_a_hold_or_change_completed_recovery() {
    let folder =
        std::env::temp_dir().join(format!("gamepause-command-policy-{}", std::process::id()));
    let config = Config::default();
    let mut engine = Engine::new(
        config.clone(),
        OptionalBackend::new(config.clone(), None),
        folder.join("state.json"),
    )
    .unwrap();
    engine.activity = Activity::Watching;
    let state = Arc::new(Mutex::new(Shared {
        discovery_ready: true,
        detection_ok: true,
        active_mode: true,
        activity: Activity::Watching,
        ..Default::default()
    }));
    let mut scanner = Scanner::new(config).unwrap();
    for _ in 0..2 {
        apply_action(
            Action::Pause,
            &mut engine,
            &folder,
            &mut scanner,
            &[],
            0.,
            &state,
        )
        .unwrap();
    }
    assert!(engine.manual_pause, "second Pause cannot release the hold");
    engine.manual_pause = false;
    engine.state = Some(crate::lmstudio::Snapshot {
        schema: 2,
        server: json!({"running":false,"port":1234}),
        server_stopped: false,
        models: vec![],
        pause_complete: true,
        games: vec![],
    });
    engine.activity = Activity::Paused;
    apply_action(
        Action::Pause,
        &mut engine,
        &folder,
        &mut scanner,
        &[],
        1.,
        &state,
    )
    .unwrap();
    assert!(
        !engine.manual_pause,
        "stale enabled UI must not create a hidden hold"
    );
    assert!(engine.state.as_ref().unwrap().pause_complete);
    assert!(
        engine.backend.backend.is_none(),
        "rejection performs no provider probe"
    );
    state.lock().unwrap().detection_ok = false;
    apply_action(
        Action::Restore,
        &mut engine,
        &folder,
        &mut scanner,
        &[],
        2.,
        &state,
    )
    .unwrap();
    assert!(engine.pending());
    assert!(engine.backend.backend.is_none());
}
#[test]
fn explicit_pause_revokes_coexistence_and_retains_removed_approved_registration() {
    use crate::gameplay::fixtures::{evidence, game};
    let current = evidence(vec![game(42, 10)]);
    let config = Config::default();
    let folder =
        std::env::temp_dir().join(format!("gamepause-pause-revocation-{}", std::process::id()));
    let mut engine = Engine::new(
        config.clone(),
        OptionalBackend::new(config.clone(), None),
        folder.join("state.json"),
    )
    .unwrap();
    engine.gameplay.refresh_offer(Some(&current), true);
    engine
        .gameplay
        .confirm(engine.gameplay.offer().unwrap().id, &current)
        .unwrap();
    engine.activity = Activity::Coexistence;
    let state = Arc::new(Mutex::new(Shared {
        active_mode: true,
        discovery_ready: true,
        detection_ok: true,
        coexistence: true,
        activity: Activity::Coexistence,
        ..Default::default()
    }));
    let mut scanner = Scanner::new(config).unwrap();
    apply_action(
        Action::Pause,
        &mut engine,
        &folder,
        &mut scanner,
        &[],
        0.,
        &state,
    )
    .unwrap();
    assert!(!engine.gameplay.active());
    assert!(engine.manual_pause);
    assert_eq!(recovery_games(&engine, &[])[0].path, current.all[0].path);
    assert!(engine.backend.backend.is_none());
}
#[test]
fn ollama_only_core_actions_and_independent_recovery_edit_state() {
    let mut shared = Shared {
        active_mode: true,
        discovery_ready: true,
        detection_ok: true,
        activity: Activity::Watching,
        config: Config::default(),
        ..Default::default()
    };
    for provider in &mut shared.config.providers {
        match provider {
            config::Provider::LMStudio { enabled, .. } => *enabled = false,
            config::Provider::Ollama { enabled, .. } => *enabled = true,
            config::Provider::Process { .. } => (),
        }
    }
    shared.config.validate().unwrap();
    assert!(shared.controls().availability().pause);
    shared.pending = true;
    shared.activity = Activity::Recovery;
    assert!(
        shared.provider_pending(crate::provider::Kind::LMStudio),
        "unpublished recovery is conservative"
    );
    shared.provider_statuses.push(crate::coordinator::Report {
        id: "ollama-main".into(),
        kind: crate::provider::Kind::Ollama,
        guarantee: crate::provider::Guarantee::SupportedFields,
        state: crate::coordinator::State::Failed,
        pending: true,
        error: "fixture failure".into(),
        retry_seconds: Some(10),
        note: String::new(),
    });
    assert!(!shared.provider_pending(crate::provider::Kind::LMStudio));
    assert!(shared.provider_pending(crate::provider::Kind::Ollama));
    assert!(shared.controls().availability().restore);
    shared.detection_ok = false;
    assert!(!shared.controls().availability().restore);
    shared.detection_ok = true;
    for provider in &mut shared.config.providers {
        if let config::Provider::Ollama { enabled, .. } = provider {
            *enabled = false;
        }
    }
    assert!(
        shared.controls().availability().restore,
        "pending recovery remains actionable with disabled providers"
    );
    assert!(!shared.controls().availability().pause);
    shared.pending = false;
    assert!(!shared.controls().availability().restore);
}
#[test]
fn shared_policy_rejects_busy_clicks_and_uses_explicit_resume() {
    let state = Arc::new(Mutex::new(Shared {
        active_mode: true,
        discovery_ready: true,
        detection_ok: true,
        activity: Activity::Unloading,
        pending: true,
        ..Default::default()
    }));
    let (tx, rx) = mpsc::channel();
    for command in [
        CoreCommand::Pause,
        CoreCommand::Resume,
        CoreCommand::Restore,
    ] {
        request_core(&state, &tx, command);
    }
    assert!(rx.try_recv().is_err());
    {
        let mut shared = state.lock().unwrap();
        shared.activity = Activity::ManualHold;
        shared.manual_pause = true;
    }
    request_core(&state, &tx, CoreCommand::Pause);
    assert!(rx.try_recv().is_err());
    request_core(&state, &tx, CoreCommand::Resume);
    assert!(
        matches!(rx.try_recv(), Ok(Action::Tracked { action, .. }) if matches!(*action, Action::Resume))
    );
    state
        .lock()
        .unwrap()
        .discovery_errors
        .insert("fixture".into(), "unreadable metadata".into());
    request_core(&state, &tx, CoreCommand::Restore);
    assert!(
        rx.try_recv().is_err(),
        "partial discovery cannot establish safe restoration"
    );
}
#[test]
fn worker_survives_repeated_data_dir_failures_and_flags_attention() {
    // The data dir is a *file*, so every write_json (status.json, and
    // inventory.json once discovery replies) fails. Pre-P0-1 this was fatal:
    // run() returned Err and the app died, orphaning any pending restore.
    // Now each tick degrades, and after a bounded number of consecutive
    // failures the shared status flips to a visible "needs attention" state.
    let base = std::env::temp_dir().join(format!("gamepause-resilience-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let folder = base.join("data"); // created as a FILE below
    fs::write(&folder, "block").unwrap();

    let config = Config {
        mode: "observe".into(),
        ..Default::default()
    };
    let engine = Engine::new(
        config.clone(),
        OptionalBackend::new(config.clone(), None),
        folder.join("state.json"),
    )
    .unwrap();
    let state = Arc::new(Mutex::new(Shared::default()));
    let (tx, rx) = mpsc::channel();
    drop(tx); // force recv_timeout to Disconnected immediately -> fast loop

    let result = run(engine, folder.clone(), state.clone(), rx, 1.0, false);
    assert!(result.is_ok(), "worker must degrade, not die: {result:?}");
    let shared = state.lock().unwrap();
    assert!(
        shared.message.contains("Needs attention"),
        "expected the needs-attention flip, got: {:#}",
        shared.message
    );
    drop(shared);
    fs::remove_file(&folder).unwrap();
    fs::remove_dir_all(&base).unwrap();
}
#[test]
fn single_failure_does_not_flag_and_ten_consecutive_do() {
    // P0-1 acceptance: one transient failure must NOT flip the visible status;
    // only a bounded run of consecutive failures (N=10) does. The Monitor is
    // the unit under test so the threshold is exact, not timing-dependent.
    let base = std::env::temp_dir().join(format!("gamepause-monitor-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let state = Arc::new(Mutex::new(Shared::default()));
    let mut monitor = Monitor::default();

    monitor.handle("boom", &base, &state);
    assert_eq!(monitor.failures, 1);
    assert!(!monitor.needs_attention);
    assert!(
        !state.lock().unwrap().message.contains("Needs attention"),
        "a single failure must not flip the status"
    );

    for _ in 0..9 {
        monitor.handle("boom", &base, &state);
    }
    assert_eq!(monitor.failures, 10);
    assert!(
        monitor.needs_attention,
        "10 consecutive failures must flip the status"
    );
    assert!(state.lock().unwrap().message.contains("Needs attention"));

    // A recovery resets the counter; one more single failure must not re-flag.
    monitor.reset();
    assert_eq!(monitor.failures, 0);
    monitor.handle("boom", &base, &state);
    assert_eq!(monitor.failures, 1);

    fs::remove_dir_all(&base).unwrap();
}
#[test]
fn pending_recovery_rejects_provider_reassignment_but_saves_presentation_preferences() {
    let folder =
        std::env::temp_dir().join(format!("gamepause-provider-edit-{}", std::process::id()));
    let config = Config::default();
    write_json(&folder.join("config.json"), &config).unwrap();
    write_json(&folder.join("state.json"), &json!({"schema":2,"server":{"running":false,"port":1234},"server_stopped":false,"models":[],"pause_complete":true})).unwrap();
    let original = fs::read(folder.join("config.json")).unwrap();
    let backend = OptionalBackend::new(config.clone(), None);
    let mut engine = Engine::new(config.clone(), backend, folder.join("state.json")).unwrap();
    let ownership = engine.backend.claims.clone();
    let journal = fs::read(folder.join("state.json")).unwrap();
    let mut scanner = Scanner::new(config.clone()).unwrap();
    let shared = Arc::new(Mutex::new(Shared {
        config: config.clone(),
        ..Default::default()
    }));
    for change in 0..4 {
        let mut updated = config.clone();
        match change {
            0 => {
                if let config::Provider::LMStudio { enabled, .. } = &mut updated.providers[0] {
                    *enabled = false;
                }
            }
            1 => {
                updated.providers.remove(0);
            }
            2 => {
                updated.lm_mut().unwrap().endpoint = "127.0.0.1:4321".into();
            }
            _ => {
                if let config::Provider::LMStudio { id, .. } = &mut updated.providers[0] {
                    *id = "reassigned".into();
                }
            }
        }
        assert!(
            !apply_action(
                Action::Settings(Box::new(updated)),
                &mut engine,
                &folder,
                &mut scanner,
                &[],
                0.,
                &shared
            )
            .unwrap()
        );
        assert_eq!(fs::read(folder.join("config.json")).unwrap(), original);
        assert_eq!(fs::read(folder.join("state.json")).unwrap(), journal);
        assert_eq!(engine.config.providers, config.providers);
        assert!(Arc::ptr_eq(&engine.backend.claims, &ownership));
        assert!(
            shared
                .lock()
                .unwrap()
                .settings_error
                .contains("recovery is pending")
        );
    }
    let mut preferences = config.clone();
    preferences.automation_enabled = false;
    preferences.advanced_settings_visible = true;
    preferences.sound_enabled = false;
    preferences.lm_mut().unwrap().endpoint = "localhost:1234".into();
    assert!(
        apply_action(
            Action::Settings(Box::new(preferences)),
            &mut engine,
            &folder,
            &mut scanner,
            &[],
            0.,
            &shared
        )
        .unwrap()
    );
    let saved = Config::load(&folder.join("config.json")).unwrap();
    assert!(saved.advanced_settings_visible);
    assert!(Arc::ptr_eq(&engine.backend.claims, &ownership));
    assert!(!saved.sound_enabled && !saved.automation_enabled);
    assert_eq!(fs::read(folder.join("state.json")).unwrap(), journal);
    if let config::Provider::LMStudio { enabled, .. } = &mut engine.config.providers[0] {
        *enabled = false;
    }
    write_json(&folder.join("config.json"), &engine.config).unwrap();
    let backend = OptionalBackend::new(engine.config.clone(), None);
    assert!(Engine::new(engine.config.clone(), backend, folder.join("state.json")).is_err());
    assert_eq!(fs::read(folder.join("state.json")).unwrap(), journal);
    fs::remove_dir_all(folder).unwrap();
}
#[test]
fn settings_apply_live_persist_and_invalid_updates_leave_previous_settings() {
    let folder =
        std::env::temp_dir().join(format!("gamepause-live-settings-{}", std::process::id()));
    let config = Config::default();
    let backend = OptionalBackend::new(config.clone(), None);
    let mut engine = Engine::new(config.clone(), backend, folder.join("state.json")).unwrap();
    let mut scanner = Scanner::new(config.clone()).unwrap();
    let shared = Arc::new(Mutex::new(Shared::default()));
    let mut updated = config;
    updated.automation_enabled = false;
    updated.restore_delay_seconds = 17.;
    updated.excluded_paths.push(r"D:\Ignored".into());
    assert!(
        apply_action(
            Action::Settings(Box::new(updated)),
            &mut engine,
            &folder,
            &mut scanner,
            &[],
            0.,
            &shared
        )
        .unwrap()
    );
    assert!(!engine.config.automation_enabled);
    assert!(scanner.excluded("game.exe", r"D:\Ignored\game.exe"));
    let loaded = Config::load(&folder.join("config.json")).unwrap();
    assert!(!loaded.automation_enabled);
    assert_eq!(loaded.restore_delay_seconds, 17.);
    let bytes = fs::read(folder.join("config.json")).unwrap();
    let mut invalid = loaded;
    invalid.lm_mut().unwrap().endpoint = "example.com:1234".into();
    assert!(
        !apply_action(
            Action::Settings(Box::new(invalid)),
            &mut engine,
            &folder,
            &mut scanner,
            &[],
            0.,
            &shared
        )
        .unwrap()
    );
    assert_eq!(bytes, fs::read(folder.join("config.json")).unwrap());
    assert!(
        shared
            .lock()
            .unwrap()
            .settings_error
            .contains("Could not save")
    );
    fs::remove_file(folder.join("config.json")).unwrap();
}
#[test]
fn panic_hook_logs_to_gamepause_log() {
    // P0-3: a real panic must be routed to gamepause.log with its message
    // and location. The hook is process-global, so we capture the current
    // hook, install ours, trigger a contained panic, assert the log, and
    // restore the original hook.
    let folder = std::env::temp_dir().join(format!("gamepause-hook-{}", std::process::id()));
    let _ = fs::create_dir_all(&folder);
    let previous = std::panic::take_hook();
    install_panic_hook(&folder);
    let caught = std::panic::catch_unwind(|| panic!("hook-test panic"));
    assert!(caught.is_err(), "the test panic should have been raised");
    let log_bytes = fs::read(folder.join("gamepause.log")).unwrap();
    let log_text = String::from_utf8_lossy(&log_bytes);
    assert!(
        log_text.contains("hook-test panic"),
        "log should contain the panic message: {log_text:?}"
    );
    assert!(
        log_text.contains("PANIC in"),
        "log should be tagged PANIC: {log_text:?}"
    );
    assert!(
        log_text.contains(file!()),
        "log should contain the panic location (file): {log_text:?}"
    );
    std::panic::set_hook(previous);
    let _ = fs::remove_file(folder.join("gamepause.log"));
}
#[test]
fn recovery_guard_uses_remembered_paths_even_after_exclusion_and_removal() {
    let mut config = Config::default();
    config.excluded_paths.push(r"D:\Removed".into());
    config.excluded_executables.push("game.exe".into());
    let mut engine = Engine::new(
        config.clone(),
        OptionalBackend::new(config.clone(), None),
        std::env::temp_dir().join("nonexistent-guard-state.json"),
    )
    .unwrap();
    engine.remembered_games.push(Game::new(
        "Custom",
        "custom",
        "Removed",
        r"D:\Removed\game.exe",
    ));
    let games = recovery_games(&engine, &[]);
    assert!(
        recovery_scanner(&config)
            .unwrap()
            .match_path(r"D:\Removed\game.exe", &games)
            .is_some()
    );
    assert!(
        Scanner::new(config)
            .unwrap()
            .match_path(r"D:\Removed\game.exe", &games)
            .is_none()
    );
}
#[test]
fn notification_preferences_save_independently_and_fail_without_optimistic_state() {
    let folder = std::env::temp_dir().join(format!(
        "gamepause-notification-settings-{}",
        std::process::id()
    ));
    let config = Config {
        advanced_settings_visible: true,
        ..Default::default()
    };
    write_json(&folder.join("config.json"), &config).unwrap();
    let backend = OptionalBackend::new(config.clone(), None);
    let mut engine = Engine::new(config.clone(), backend, folder.join("state.json")).unwrap();
    let mut scanner = Scanner::new(config.clone()).unwrap();
    let shared = Arc::new(Mutex::new(Shared {
        config: config.clone(),
        ..Default::default()
    }));
    for (visual, sound) in [(true, false), (false, false), (true, true), (false, true)] {
        assert!(
            apply_action(
                Action::NotificationPreferences { visual, sound },
                &mut engine,
                &folder,
                &mut scanner,
                &[],
                0.,
                &shared
            )
            .unwrap()
        );
        let saved = Config::load(&folder.join("config.json")).unwrap();
        assert_eq!(
            (saved.notifications_enabled, saved.sound_enabled),
            (visual, sound)
        );
        assert_eq!(saved.providers, config.providers);
        assert_eq!(saved.automation_enabled, config.automation_enabled);
        assert!(!folder.join("state.json").exists());
    }
    fs::remove_file(folder.join("config.json")).unwrap();
    fs::create_dir(folder.join("config.json")).unwrap();
    assert!(
        !apply_action(
            Action::NotificationPreferences {
                visual: true,
                sound: false
            },
            &mut engine,
            &folder,
            &mut scanner,
            &[],
            0.,
            &shared
        )
        .unwrap()
    );
    assert!(!engine.config.notifications_enabled && engine.config.sound_enabled);
    assert!(!shared.lock().unwrap().config.notifications_enabled);
    engine.config.advanced_settings_visible = false;
    assert!(
        apply_action(
            Action::NotificationPreferences {
                visual: true,
                sound: false
            },
            &mut engine,
            &folder,
            &mut scanner,
            &[],
            0.,
            &shared
        )
        .is_err()
    );
    fs::remove_dir_all(folder).unwrap();
}
#[test]
fn appearance_saves_without_changing_ai_and_failed_saves_keep_the_previous_choice() {
    let folder = std::env::temp_dir().join(format!("gamepause-appearance-{}", std::process::id()));
    let config = Config {
        advanced_settings_visible: true,
        ..Default::default()
    };
    write_json(&folder.join("config.json"), &config).unwrap();
    let backend = OptionalBackend::new(config.clone(), None);
    let mut engine = Engine::new(config.clone(), backend, folder.join("state.json")).unwrap();
    let mut scanner = Scanner::new(config.clone()).unwrap();
    let shared = Arc::new(Mutex::new(Shared {
        config: config.clone(),
        ..Default::default()
    }));
    engine.manual_pause = true;
    for choice in [
        crate::config::Appearance::Dark,
        crate::config::Appearance::Light,
    ] {
        assert!(
            !apply_action(
                Action::Appearance(choice),
                &mut engine,
                &folder,
                &mut scanner,
                &[],
                0.,
                &shared
            )
            .unwrap()
        );
        assert_eq!(
            Config::load(&folder.join("config.json"))
                .unwrap()
                .appearance,
            choice
        );
        assert_eq!(shared.lock().unwrap().config.appearance, choice);
        assert!(engine.manual_pause);
        assert!(!folder.join("state.json").exists());
        assert_eq!(engine.config.providers, config.providers);
        assert!(engine.backend.backend.is_none());
    }
    fs::remove_file(folder.join("config.json")).unwrap();
    fs::create_dir(folder.join("config.json")).unwrap();
    assert!(
        apply_action(
            Action::Appearance(crate::config::Appearance::Dark),
            &mut engine,
            &folder,
            &mut scanner,
            &[],
            0.,
            &shared
        )
        .is_err()
    );
    assert_eq!(
        shared.lock().unwrap().config.appearance,
        crate::config::Appearance::Light
    );
    engine.config.advanced_settings_visible = false;
    assert!(
        apply_action(
            Action::Appearance(crate::config::Appearance::Dark),
            &mut engine,
            &folder,
            &mut scanner,
            &[],
            0.,
            &shared
        )
        .is_err()
    );
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn advanced_visibility_persists_and_stale_tools_fail_without_changing_ai() {
    let folder = std::env::temp_dir().join(format!("gamepause-advanced-{}", std::process::id()));
    let config = Config::default();
    write_json(&folder.join("config.json"), &config).unwrap();
    let backend = OptionalBackend::new(config.clone(), None);
    let mut engine = Engine::new(config.clone(), backend, folder.join("state.json")).unwrap();
    let mut scanner = Scanner::new(config.clone()).unwrap();
    let shared = Arc::new(Mutex::new(Shared {
        config: config.clone(),
        ..Default::default()
    }));
    assert!(
        apply_action(
            Action::AdvancedVisibility(true),
            &mut engine,
            &folder,
            &mut scanner,
            &[],
            0.,
            &shared
        )
        .unwrap()
    );
    let mut stale = engine.config.clone();
    stale.restore_delay_seconds = 12.;
    assert!(
        Config::load(&folder.join("config.json"))
            .unwrap()
            .advanced_settings_visible
    );
    assert_eq!(engine.config.providers, config.providers);
    assert!(engine.config.automation_enabled);
    assert!(
        apply_action(
            Action::AdvancedVisibility(false),
            &mut engine,
            &folder,
            &mut scanner,
            &[],
            0.,
            &shared
        )
        .unwrap()
    );
    let bytes = fs::read(folder.join("config.json")).unwrap();
    for action in [
        Action::AdvancedSettings(Box::new(stale)),
        Action::Verify,
        Action::Doctor,
    ] {
        assert!(
            apply_action(action, &mut engine, &folder, &mut scanner, &[], 0., &shared).is_err()
        );
        assert_eq!(fs::read(folder.join("config.json")).unwrap(), bytes);
        assert!(!folder.join("state.json").exists());
        assert!(engine.backend.backend.is_none());
    }
    let (tx, rx) = mpsc::channel();
    request_verify(&shared, &tx);
    assert!(rx.try_recv().is_err());
    shared.lock().unwrap().verifying = true;
    assert!(
        apply_action(
            Action::Tracked {
                id: 900,
                action: Box::new(Action::Verify)
            },
            &mut engine,
            &folder,
            &mut scanner,
            &[],
            0.,
            &shared
        )
        .is_err()
    );
    assert!(!shared.lock().unwrap().verifying);
    shared.lock().unwrap().doctor_pending = true;
    assert!(
        apply_action(
            Action::Tracked {
                id: 901,
                action: Box::new(Action::Doctor)
            },
            &mut engine,
            &folder,
            &mut scanner,
            &[],
            0.,
            &shared
        )
        .is_err()
    );
    assert!(!shared.lock().unwrap().doctor_pending);
    assert!(shared.lock().unwrap().doctor_report.is_none());
    assert!(!crate::ui_commands::allowed(
        &shared,
        crate::ui_commands::Command::OpenFolder
    ));
    assert!(!crate::ui_commands::allowed(
        &shared,
        crate::ui_commands::Command::Startup
    ));
    fs::remove_file(folder.join("config.json")).unwrap();
    fs::create_dir(folder.join("config.json")).unwrap();
    assert!(
        !apply_action(
            Action::AdvancedVisibility(true),
            &mut engine,
            &folder,
            &mut scanner,
            &[],
            0.,
            &shared
        )
        .unwrap()
    );
    assert!(!engine.config.advanced_settings_visible);
    assert!(!shared.lock().unwrap().config.advanced_settings_visible);
    fs::remove_dir_all(folder).unwrap();
}
#[test]
fn verify_request_rejects_duplicates_and_unsafe_shared_states() {
    let state = Arc::new(Mutex::new(Shared {
        detection_ok: true,
        active_mode: true,
        discovery_ready: true,
        ..Default::default()
    }));
    let (tx, rx) = mpsc::channel();
    state.lock().unwrap().config.advanced_settings_visible = true;
    request_verify(&state, &tx);
    request_verify(&state, &tx);
    assert!(
        matches!(rx.try_recv(), Ok(Action::Tracked { action, .. }) if matches!(*action, Action::Verify))
    );
    assert!(rx.try_recv().is_err());
    state.lock().unwrap().verifying = false;
    state.lock().unwrap().pending = true;
    request_verify(&state, &tx);
    assert!(rx.try_recv().is_err());
}
#[test]
fn ask_rule_withholds_the_trigger_until_answered_and_excludes_ignore() {
    let game = crate::gameplay::fixtures::game(42, 10);
    let other = crate::gameplay::fixtures::game(43, 10);
    let mut config = Config::default();
    config.ask_games.push(game.path.to_uppercase());
    assert!(config.asks(&game.path) && !config.asks(&other.path));
    let scanner = Scanner::new(config.clone()).unwrap();
    let all = vec![game.clone(), other.clone()];
    let unanswered = evidence_from(all.clone(), &scanner, &config, false);
    assert_eq!(unanswered.triggers, vec![other.clone()]);
    assert_eq!(unanswered.all.len(), 2);
    let answered = evidence_from(all.clone(), &scanner, &config, true);
    assert_eq!(answered.triggers.len(), 2);
    // An answer never overrides a game the user ignores.
    config.ignored_games.push(other.path.clone());
    let ignored = evidence_from(all, &scanner, &config, true);
    assert_eq!(ignored.triggers, vec![game]);
    // The setting round-trips and defaults to empty for existing files.
    let saved = serde_json::to_string(&config).unwrap();
    assert_eq!(Config::parse(&saved).unwrap().ask_games.len(), 1);
    assert!(
        Config::parse(r#"{"settings_version":4}"#)
            .unwrap()
            .ask_games
            .is_empty()
    );
}
