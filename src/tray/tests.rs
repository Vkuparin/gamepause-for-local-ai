#[test]
fn manual_hold_has_one_resume_action_and_no_restore_or_duplicate_resume() {
    let state = crate::app::Shared {
        active_mode: true,
        discovery_ready: true,
        detection_ok: true,
        manual_pause: true,
        pending: true,
        activity: Activity::ManualHold,
        ..Default::default()
    };
    let items = command_items(&state);
    assert_eq!(items.iter().filter(|item| item.1 == "Resume AI").count(), 1);
    assert!(!items.iter().any(|item| item.1.contains("Restore")));
    assert_eq!(
        items
            .iter()
            .find(|item| item.0 == Command::Pause as usize)
            .unwrap()
            .2,
        MF_GRAYED
    );
    assert_eq!(
        items
            .iter()
            .find(|item| item.0 == Command::Resume as usize)
            .unwrap()
            .2,
        0
    );
}

#[test]
fn menu_is_the_same_quick_controls_with_or_without_advanced() {
    let mut state = crate::app::Shared::default();
    let basic = crate::dashboard::shared_command_ids(false);
    for advanced in [false, true] {
        state.config.advanced_settings_visible = advanced;
        let items = super::command_items(&state);
        let ids = items
            .iter()
            .filter_map(|item| crate::ui_commands::Command::from_id(item.0 as i32))
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            [
                crate::ui_commands::Command::Automation,
                crate::ui_commands::Command::Pause,
                crate::ui_commands::Command::Resume,
                crate::ui_commands::Command::OpenDashboard,
                crate::ui_commands::Command::Quit,
            ]
        );
        // Every tray action has a dashboard counterpart that needs no Advanced toggle.
        assert!(ids.iter().all(|command| {
            *command == crate::ui_commands::Command::OpenDashboard
                || basic.contains(&(*command as i32))
        }));
        // No informational rows hide among the commands.
        assert!(
            items
                .iter()
                .all(|item| item.0 != 0 || item.2 == MF_SEPARATOR)
        );
    }
}
use super::*;
#[test]
fn tray_uses_confirmed_activity_not_pending_recovery() {
    assert_eq!(activity_kind(Activity::Paused), StateKind::Paused);
    assert_eq!(activity_kind(Activity::ManualHold), StateKind::Paused);
    assert_eq!(activity_kind(Activity::Countdown), StateKind::Paused);
    assert_eq!(activity_kind(Activity::Recovery), StateKind::Attention);
    assert_eq!(
        activity_kind(Activity::PartialFailure),
        StateKind::Attention
    );
    assert_eq!(activity_kind(Activity::Unloading), StateKind::Idle);
    assert_eq!(activity_kind(Activity::Capturing), StateKind::Idle);
    assert_eq!(activity_kind(Activity::Watching), StateKind::Idle);
}
#[test]
fn toast_spec_returns_right_title_and_severity_per_state() {
    // Pause-success and restore-success → the "good" sound, plain title.
    let paused = toast_spec(StateKind::Paused);
    assert_eq!(paused.title, "GamePause");
    assert_eq!(paused.severity, Severity::Success);
    let idle = toast_spec(StateKind::Idle);
    assert_eq!(idle.title, "GamePause");
    assert_eq!(idle.severity, Severity::Success);
    // Restore/pause-failure → error style, attention title.
    let attention = toast_spec(StateKind::Attention);
    assert_eq!(attention.title, "GamePause needs attention");
    assert_eq!(attention.severity, Severity::Error);
}
#[test]
fn beep_code_maps_success_to_ok_and_failure_to_warning() {
    assert_eq!(beep_code(StateKind::Idle), MB_OK);
    assert_eq!(beep_code(StateKind::Paused), MB_OK);
    assert_eq!(beep_code(StateKind::Attention), MB_ICONASTERISK);
}
#[test]
fn system_is_dark_reads_a_consistent_binary_preference() {
    // The registry is the single source of truth; two reads agree.
    let a = super::system_is_dark();
    let b = super::system_is_dark();
    assert_eq!(a, b, "two consecutive reads of the OS theme must agree");
}
#[test]
fn apply_theme_does_not_panic_on_null_hwnd() {
    // DwmSetWindowAttribute on a null HWND is a documented no-op; the
    // production call site is the same code path, so it must not panic.
    unsafe { super::apply_theme(std::ptr::null_mut()) };
}
#[test]
fn only_real_or_lasting_problems_are_announced_as_failures() {
    // A failed operation and saved AI found waiting are announced at once.
    assert!(failure_due(Activity::PartialFailure, "Restore failed", 0));
    assert!(failure_due(
        Activity::Recovery,
        "Saved AI is waiting to be restored; restoring in 30s",
        0
    ));
    assert!(failure_due(
        Activity::Watching,
        "Needs attention — repeated monitoring failures: x",
        0
    ));
    // Waking from sleep is a short hold, with or without saved AI.
    let wake = "Power state changed; fresh game detection is required before AI control.";
    assert!(!failure_due(Activity::DetectionUnavailable, wake, 1));
    assert!(!failure_due(
        Activity::Recovery,
        "Windows resumed; saved AI recovery in 30s after fresh game detection.",
        0
    ));
    // Detection that stays down is announced once it has lasted.
    assert!(!failure_due(
        Activity::DetectionUnavailable,
        wake,
        DETECTION_DOWN_TICKS - 1
    ));
    assert!(failure_due(
        Activity::DetectionUnavailable,
        wake,
        DETECTION_DOWN_TICKS
    ));
    assert!(!failure_due(Activity::Unknown, "Starting GamePause", 0));
    assert!(!failure_due(Activity::Paused, "AI paused for gaming", 0));
}
#[test]
fn icon_tint_maps_state_to_distinct_colors() {
    // The three states must be visually distinct (P1-6 acceptance: the
    // state→icon mapping is asserted without Win32).
    let idle = icon_tint(StateKind::Idle);
    let paused = icon_tint(StateKind::Paused);
    let attention = icon_tint(StateKind::Attention);
    assert_ne!(idle, paused, "idle and paused icons must differ");
    assert_ne!(idle, attention, "idle and attention icons must differ");
    assert_ne!(paused, attention, "paused and attention icons must differ");
    // DIB pixels are BGR. The attention color is a warning red (low B/G,
    // high R) so it pops against the dark tray and cannot pass for idle.
    assert!(attention[2] > 180 && attention[1] < 120 && attention[0] < 120);
    // Paused uses orange with a dominant red byte; idle is blue.
    assert!(paused[2] > paused[0] && paused[2] > paused[1]);
    assert!(idle[0] > idle[2]);
}
#[test]
fn timer_can_reenter_while_menu_context_is_alive() {
    use crate::app::Shared;
    use std::sync::{Arc, Mutex, mpsc};
    let (tx, rx) = mpsc::channel();
    let shared = Arc::new(Mutex::new(Shared {
        commands: Default::default(),
        activity: Activity::PartialFailure,
        restore_offer: None,
        coexistence: false,
        restore_feedback: None,
        detection_ok: false,
        message: "Needs attention: simulated failure".into(),
        disabled: false,
        manual_pause: false,
        pending: true,
        active_mode: false,
        config: crate::config::Config::default(),
        games: vec![],
        active_games: vec![],
        running_apps: vec![],
        discovery_errors: Default::default(),
        settings_error: String::new(),
        revision: 0,
        discovery_ready: false,
        verify_report: None,
        verifying: false,
        pause_completions: 0,
        restore_completions: 0,
        provider_statuses: vec![],
        doctor_report: None,
        doctor_pending: false,
        lm_missing: false,
        lm_running: false,
        ollama_running: false,
        ollama_installed: false,
        freed_bytes: 0,
        ask_prompt: vec![],
        suggestion: None,
        power: Default::default(),
    }));
    {
        let mut state = shared.lock().unwrap();
        state.config.notifications_enabled = false;
        state.config.sound_enabled = false;
    }
    UI_STATE.with(|state| {
        *state.borrow_mut() = Some(UI {
            shared: shared.clone(),
            tx,
            folder: PathBuf::new(),
            icons: Icons {
                idle: null_mut(),
                paused: null_mut(),
                attention: null_mut(),
            },
            taskbar_message: WM_APP + 9,
            shown: None,
            detection_down: 0,
            notifications: crate::notifications::Queue::default(),
            clock: std::time::Instant::now(),

            menu_open: false,
        })
    });
    let session = begin_menu().expect("first menu opens");
    assert!(
        begin_menu().is_none(),
        "nested clicks cannot start another menu"
    );
    // TrackPopupMenu invokes this callback while its caller is still active.
    unsafe {
        window_proc(null_mut(), WM_TIMER, 1, 0);
    }
    // The nested timer could write the state it sent to the shell.
    assert_eq!(
        ui_snapshot().unwrap().shown,
        Some((
            StateKind::Attention,
            "Needs attention: simulated failure".to_string()
        ))
    );
    shared.lock().unwrap().message = "AI available".into();
    shared.lock().unwrap().activity = Activity::Watching;
    shared.lock().unwrap().pending = false;
    unsafe {
        window_proc(null_mut(), WM_TIMER, 1, 0);
    }
    assert_eq!(
        ui_snapshot().unwrap().shown,
        Some((StateKind::Idle, "AI available".to_string()))
    );
    drop(session);
    assert!(begin_menu().is_some(), "menu opens again after dismissal");
    shared.lock().unwrap().detection_ok = true;
    assert_eq!(
        unsafe { window_proc_inner(null_mut(), WM_POWERBROADCAST, 4, 0) },
        1
    );
    assert!(shared.lock().unwrap().power.snapshot().suspended);
    assert!(!shared.lock().unwrap().detection_ok);
    assert!(matches!(rx.try_recv(), Ok(Action::PowerChanged)));
    assert_eq!(
        unsafe { window_proc_inner(null_mut(), WM_POWERBROADCAST, 18, 0) },
        1
    );
    assert!(!shared.lock().unwrap().power.snapshot().suspended);
    let generation = shared.lock().unwrap().power.snapshot().generation;
    assert!(matches!(rx.try_recv(), Ok(Action::PowerChanged)));
    unsafe {
        window_proc_inner(null_mut(), WM_POWERBROADCAST, 7, 0);
    }
    assert_eq!(
        shared.lock().unwrap().power.snapshot().generation,
        generation
    );
    assert!(
        rx.try_recv().is_err(),
        "user-interaction resume must not reset grace a second time"
    );
    UI_STATE.with(|state| state.borrow_mut().take());
}
#[test]
#[ignore = "requires an interactive Windows desktop; opens only the test application's menus"]
fn native_popup_survives_repeated_timer_reentry() {
    use crate::app::Shared;
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::{Duration, Instant};
    FINISHED.store(false, Ordering::Relaxed);
    MENU_TIMER_TICKS.store(0, Ordering::Relaxed);
    MENU_OPENINGS.store(0, Ordering::Relaxed);
    let shared = Arc::new(Mutex::new(Shared {
        commands: Default::default(),
        activity: Activity::Observation,
        restore_offer: None,
        coexistence: false,
        restore_feedback: None,
        detection_ok: false,
        message: "Tray regression test (observation only)".into(),
        disabled: false,
        manual_pause: false,
        pending: false,
        active_mode: false,
        config: crate::config::Config::default(),
        games: vec![],
        active_games: vec![],
        running_apps: vec![],
        discovery_errors: Default::default(),
        settings_error: String::new(),
        revision: 0,
        discovery_ready: false,
        verify_report: None,
        verifying: false,
        pause_completions: 0,
        restore_completions: 0,
        provider_statuses: vec![],
        doctor_report: None,
        doctor_pending: false,
        lm_missing: false,
        lm_running: false,
        ollama_running: false,
        ollama_installed: false,
        freed_bytes: 0,
        ask_prompt: vec![],
        suggestion: None,
        power: Default::default(),
    }));
    let (tx, _rx) = mpsc::channel();
    {
        let mut state = shared.lock().unwrap();
        state.config.notifications_enabled = false;
        state.config.sound_enabled = false;
    }
    let driver_state = shared.clone();
    let driver = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while WINDOW.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let hwnd = WINDOW.load(Ordering::Relaxed) as HWND;
        assert!(!hwnd.is_null(), "test window did not initialize");
        unsafe {
            SendMessageW(hwnd, WM_POWERBROADCAST, 4, 0);
        }
        assert!(driver_state.lock().unwrap().power.snapshot().suspended);
        unsafe {
            SendMessageW(hwnd, WM_POWERBROADCAST, 18, 0);
        }
        let resumed = driver_state.lock().unwrap().power.snapshot();
        assert!(!resumed.suspended);
        unsafe {
            SendMessageW(hwnd, WM_POWERBROADCAST, 7, 0);
        }
        assert_eq!(driver_state.lock().unwrap().power.snapshot(), resumed);
        let mut counts = Vec::new();
        for iteration in 0..3 {
            let before = MENU_TIMER_TICKS.load(Ordering::Relaxed);
            unsafe {
                PostMessageW(hwnd, CALLBACK, 0, WM_RBUTTONUP as LPARAM);
            }
            driver_state
                .lock()
                .unwrap()
                .config
                .advanced_settings_visible = iteration % 2 == 0;
            driver_state.lock().unwrap().config.appearance = if iteration % 2 == 0 {
                crate::config::Appearance::Dark
            } else {
                crate::config::Appearance::Light
            };
            let deadline = Instant::now() + Duration::from_secs(10);
            while MENU_TIMER_TICKS.load(Ordering::Relaxed) < before + 2 && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            counts.push(MENU_TIMER_TICKS.load(Ordering::Relaxed) - before);
            unsafe {
                PostMessageW(hwnd, WM_CANCELMODE, 0, 0);
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        request_exit();
        counts
    });
    run(shared, tx, PathBuf::from("."), false).unwrap();
    let counts = driver.join().unwrap();
    assert_eq!(
        MENU_OPENINGS.load(Ordering::Relaxed),
        3,
        "three distinct menus must open"
    );
    assert!(
        counts.iter().all(|ticks| *ticks >= 2),
        "each real popup must survive at least two timer callbacks: {counts:?}"
    );
    FINISHED.store(false, Ordering::Relaxed);
}
#[test]
fn startup_keeps_custom_folder() {
    let command = startup_command(
        Path::new(r"C:\Program Files\GamePause\GamePause.exe"),
        Path::new(r"D:\AI Data\GamePause"),
    );
    assert_eq!(
        command,
        r#""C:\Program Files\GamePause\GamePause.exe" --background --data-dir "D:\AI Data\GamePause""#
    );
}

#[test]
fn open_path_failure_is_classified_as_a_visible_error() {
    // ShellExecuteW returns an HINSTANCE > 32 on success; 0 and 1..=32 are
    // documented failure codes. The classifier is the testable core of
    // "open_path surfaces a visible error on failure" (P1-7).
    let v = |n: isize| n as *mut core::ffi::c_void;
    assert!(
        super::shell_execute_failed(v(0)),
        "0 is the generic failure code"
    );
    assert!(
        super::shell_execute_failed(v(2)),
        "ERROR_FILE_NOT_FOUND (2)"
    );
    assert!(
        super::shell_execute_failed(v(3)),
        "ERROR_PATH_NOT_FOUND (3)"
    );
    assert!(
        super::shell_execute_failed(v(32)),
        "32 is still in the failure range"
    );
    assert!(
        !super::shell_execute_failed(v(33)),
        "33 is the first success value"
    );
    assert!(
        !super::shell_execute_failed(v(0x0040_0000)),
        "a real HINSTANCE (a pointer-sized handle) is a success"
    );
}
#[test]
fn notifications_follow_completions_not_countdowns_or_retry_states() {
    assert_eq!(
        notification_flags(StateKind::Paused, false) & NIIF_NOSOUND,
        NIIF_NOSOUND
    );
    assert_eq!(notification_flags(StateKind::Idle, true) & NIIF_NOSOUND, 0);
    assert_ne!(
        notification_flags(StateKind::Attention, true) & NIIF_ERROR,
        0
    );
    for kind in [StateKind::Paused, StateKind::Idle, StateKind::Attention] {
        assert_ne!(notification_flags(kind, true) & NIIF_RESPECT_QUIET_TIME, 0);
    }
}
