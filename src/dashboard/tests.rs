use super::{bridge::dispatch, native::native_options, *};
use crate::{
    control::Activity,
    coordinator::{Report, State},
    discovery::Game,
    provider::{Guarantee, Kind},
};
use std::sync::Arc;
fn fixture() -> Shared {
    let mut s = Shared {
        activity: Activity::Paused,
        active_mode: true,
        pending: true,
        discovery_ready: true,
        detection_ok: true,
        ..Default::default()
    };
    s.config.appearance = crate::config::Appearance::Dark;
    for (name, platform) in [
        ("Stardew Valley", "Steam"),
        ("The Last of Us Part II", "Epic"),
        ("The Witcher 3: Wild Hunt — Remastered", "Steam"),
        ("Trine 4: The Nightmare Prince", "Steam"),
    ] {
        s.games.push(Game {
            identity: name.into(),
            name: name.into(),
            launcher: platform.into(),
            path: format!(r"D:\Fixture Games\{name}"),
        });
    }
    let mut active = crate::gameplay::fixtures::game(10416, 11);
    active.game = s.games[2].name.clone();
    active.path = s.games[2].path.clone();
    active.executable = format!(r"{}\game.exe", active.path);
    active.launcher = "Steam".into();
    s.active_games.push(active.clone());
    s.restore_offer = Some(crate::gameplay::RestoreOffer {
        id: 42,
        games: vec![active],
    });
    s.provider_statuses = vec![Report {
        id: s.config.providers[0].id().into(),
        kind: Kind::LMStudio,
        guarantee: Guarantee::CapturedConfiguration,
        state: State::Paused,
        pending: true,
        error: String::new(),
        retry_seconds: None,
        note: String::new(),
    }];
    s.running_apps = vec![
        crate::processes::RunningApp {
            name: "game.exe".into(),
            path: format!(r"{}\game.exe", s.games[2].path),
        },
        crate::processes::RunningApp {
            name: "missing-game.exe".into(),
            path: r"D:\Fixture Games\Unknown\missing-game.exe".into(),
        },
    ];
    s
}
fn dashboard(s: Shared) -> Dashboard {
    let (tx, _) = mpsc::channel();
    let (_, rx) = mpsc::channel();
    Dashboard::new(
        Arc::new(Mutex::new(s)),
        tx,
        PathBuf::from("scratch/ui-fixture"),
        Arc::new(Mutex::new(rx)),
    )
}
#[test]
fn pending_recovery_and_idle_do_not_claim_verified_pause_or_current_residency() {
    let mut s = fixture();
    s.activity = Activity::Recovery;
    assert_eq!(hero(&s).look, Look::Attention);
    assert_ne!(hero(&s).title, "AI PAUSED");
    s.activity = Activity::Watching;
    s.pending = false;
    s.provider_statuses.clear();
    assert_eq!(hero(&s).look, Look::Running);
    // An app that is not running is not "Ready" to be paused.
    assert_eq!(provider_status(&s)[0].1, "Not running");
    // Idle text never states which models are loaded.
    s.lm_running = true;
    assert_eq!(provider_status(&s)[0].1, "Ready");
    if let Some(crate::config::Provider::Ollama { enabled, .. }) = s.config.providers.get_mut(1) {
        *enabled = true;
    }
    // Ollama is not on this PC: no line for it at all.
    assert_eq!(provider_status(&s).len(), 1);
    s.ollama_installed = true;
    let lines = provider_status(&s);
    assert_eq!(
        (lines[1].0.as_str(), lines[1].1),
        ("Ollama:", "Not running")
    );
    s.ollama_installed = false;
    s.ollama_running = true;
    assert_eq!(provider_status(&s)[1].1, "Ready");
    // Found absent at a game start: still nothing to show.
    s.ollama_running = false;
    s.provider_statuses.push(Report {
        id: s.config.providers[1].id().into(),
        kind: Kind::Ollama,
        guarantee: Guarantee::SupportedFields,
        state: State::Paused,
        pending: true,
        error: String::new(),
        retry_seconds: None,
        note: crate::ollama_session::NOT_RUNNING.into(),
    });
    assert_eq!(provider_status(&s).len(), 1);
    // LM Studio without its CLI is not listed either.
    s.lm_missing = true;
    assert!(provider_status(&s).is_empty());
    s.lm_missing = false;
    s.config = fixture().config;
    s.provider_statuses = fixture().provider_statuses;
    s.provider_statuses[0].state = State::Restored;
    assert_eq!(provider_status(&s)[0].1, "Models restored");
    s.config.automation_enabled = false;
    assert!(hero(&s).hint.contains("off"));
    assert!(hero(&s).game.is_some());
    s.detection_ok = false;
    assert!(hero(&s).game.is_none());
}
#[test]
fn every_activity_folds_into_one_of_four_looks() {
    use Activity::*;
    let mut s = fixture();
    for (activity, look, title) in [
        (Unknown, Look::Loading, "LOADING"),
        (Watching, Look::Running, "AI RUNNING"),
        (Observation, Look::Running, "AI RUNNING"),
        (Coexistence, Look::Running, "AI RUNNING"),
        (Paused, Look::Paused, "AI PAUSED"),
        (ManualHold, Look::Paused, "AI PAUSED"),
        (Countdown, Look::Paused, "AI PAUSED"),
        (Capturing, Look::Loading, "LOADING"),
        (WaitingForInference, Look::Loading, "LOADING"),
        (Unloading, Look::Loading, "LOADING"),
        (Restoring, Look::Loading, "LOADING"),
        (Verifying, Look::Loading, "LOADING"),
        (DetectionUnavailable, Look::Attention, "AI NEEDS ATTENTION"),
        (Recovery, Look::Attention, "AI NEEDS ATTENTION"),
        (PartialFailure, Look::Attention, "AI NEEDS ATTENTION"),
    ] {
        s.activity = activity;
        let hero = hero(&s);
        assert_eq!((hero.look, hero.title), (look, title), "{activity:?}");
    }
    s.activity = Capturing;
    s.provider_statuses[0].state = State::Restored;
    assert_eq!(provider_status(&s)[0].1, "Pausing AI");
    s.activity = Restoring;
    s.provider_statuses[0].state = State::Paused;
    assert_eq!(provider_status(&s)[0].1, "Resuming AI");
    s.provider_statuses[0].state = State::Failed;
    assert_eq!(provider_status(&s)[0].2, Tone::Error);
}
#[test]
fn columns_sort_both_ways_and_keep_name_order_within_ties() {
    let mut s = fixture();
    let path = s.games[3].path.clone();
    set_ignored(&mut s.config, &path, true);
    let names = |sort| {
        let mut entries = rows(&s, Page::Games, "");
        sort_rows(&mut entries, sort, false);
        entries
            .into_iter()
            .map(|r| r.name.chars().take(7).collect::<String>())
            .collect::<Vec<_>>()
    };
    assert_eq!(names((0, true))[0], "Stardew");
    assert_eq!(names((0, false))[0], "Trine 4");
    assert_eq!(
        names((1, true)),
        ["The Las", "Stardew", "The Wit", "Trine 4"]
    );
    assert_eq!(names((2, true))[3], "Trine 4");
    assert_eq!(names((2, false))[0], "Trine 4");
}
#[test]
fn game_tables_filter_dedupe_and_preserve_exclusions() {
    let mut s = fixture();
    s.games.push(s.games[0].clone());
    assert_eq!(rows(&s, Page::Games, "").len(), 4);
    assert_eq!(rows(&s, Page::Games, "witcher")[0].pid, Some(10416));
    let path = s.games[0].path.clone();
    set_ignored(&mut s.config, &path, true);
    assert!(rows(&s, Page::Games, "stardew")[0].ignored);
    assert_eq!(rows(&s, Page::Ignored, "").len(), 1);
    set_ignored(&mut s.config, &path, false);
    assert!(rows(&s, Page::Ignored, "").is_empty());
    assert_eq!(
        rows(&s, Page::Running, "missing")[0].platform,
        "Unrecognized"
    );
}
#[test]
fn success_toasts_expire_unchanged_results_and_errors_persist() {
    let start = Instant::now();
    let mut toast = Toast::new();
    let mut s = Shared::default();
    s.commands.local(Outcome::Completed, "Settings saved");
    assert!(toast.message(&s, start).is_some());
    assert!(toast.message(&s, start + Duration::from_secs(6)).is_none());
    s.commands.local(Outcome::Failed, "Save refused");
    assert!(
        toast
            .message(&s, start + Duration::from_secs(60))
            .unwrap()
            .1
    );
    assert!(
        toast
            .message(&s, start + Duration::from_secs(120))
            .is_some()
    );
}
#[test]
fn gameplay_resume_opens_unchecked_modal_and_does_not_send_restore() {
    let s = fixture();
    let (tx, rx) = mpsc::channel();
    let (_, ui_rx) = mpsc::channel();
    let mut d = Dashboard::new(
        Arc::new(Mutex::new(s.clone())),
        tx,
        PathBuf::from("scratch"),
        Arc::new(Mutex::new(ui_rx)),
    );
    d.resume(&s);
    assert!(matches!(d.modal,Some(Modal::Resume(_,ref choices)) if choices.iter().all(|on|!*on)));
    assert!(rx.try_recv().is_err());
}
#[test]
fn gameplay_confirmation_refuses_stale_offers_busy_control_and_unselected_exclusions() {
    let mut s = fixture();
    let offer = s.restore_offer.clone().unwrap();
    assert!(
        matches!(confirmed_resume(&s,&offer,&[false]),Ok(Action::ConfirmedRestore{ignored,..}) if ignored.is_empty())
    );
    assert!(
        matches!(confirmed_resume(&s,&offer,&[true]),Ok(Action::ConfirmedRestore{ignored,..}) if ignored==vec![offer.games[0].executable.clone()])
    );
    s.restore_offer.as_mut().unwrap().id += 1;
    assert!(confirmed_resume(&s, &offer, &[true]).is_err());
    s.restore_offer = Some(offer.clone());
    s.activity = Activity::Restoring;
    assert!(confirmed_resume(&s, &offer, &[true]).is_err());
    s.activity = Activity::Paused;
    s.commands.settings_pending = true;
    assert!(confirmed_resume(&s, &offer, &[true]).is_err());
}
#[test]
fn recent_worker_log_has_a_read_cap_and_discards_partial_first_line() {
    let folder = PathBuf::from(format!("scratch/log-fixture-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let mut text = "x".repeat(70000);
    text.push_str("\nlast complete event\n");
    std::fs::write(folder.join("gamepause.log"), text).unwrap();
    assert_eq!(read_worker_log(&folder).unwrap(), "last complete event\n");
    std::fs::remove_file(folder.join("gamepause.log")).unwrap();
    std::fs::remove_dir(folder).unwrap();
}
#[test]
#[ignore = "opens only a fictional-state dashboard to exercise UI-thread lifecycle"]
fn ui_bridge_hides_reopens_and_stops_without_backend() {
    fn until(label: &str, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready() {
            assert!(Instant::now() < deadline, "UI lifecycle timed out: {label}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            close();
        }
    }
    let _cleanup = Cleanup;
    let s = Arc::new(Mutex::new(fixture()));
    let (tx, rx) = mpsc::channel();
    show(s, tx, PathBuf::from("scratch/ui-fixture"));
    until("window open", || UI_VISIBLE.load(Ordering::Relaxed));
    let ctx = BRIDGE
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .ctx
        .clone()
        .unwrap();
    ctx.send_viewport_cmd(ViewportCommand::Close);
    ctx.request_repaint();
    until("window close", || !UI_VISIBLE.load(Ordering::Relaxed));
    assert!(!needs_running_apps());
    theme_changed();
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !UI_VISIBLE.load(Ordering::Relaxed),
        "A theme change must not reopen a closed dashboard"
    );
    let before = BRIDGE.lock().unwrap().as_ref().unwrap().fingerprint.clone();
    refresh();
    assert_eq!(before, BRIDGE.lock().unwrap().as_ref().unwrap().fingerprint);
    dispatch(UiRequest::Show);
    until("window open", || UI_VISIBLE.load(Ordering::Relaxed));
    dispatch(UiRequest::Resume);
    ctx.request_repaint();
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        rx.try_recv().is_err(),
        "Opening a warning must not send a restore action"
    );
    close();
    assert!(BRIDGE.lock().unwrap().is_none());
}
#[test]
fn activity_is_bounded_and_unchanged_frames_add_no_events() {
    let mut log = ActivityLog::new();
    log.record("same".into());
    log.record("same".into());
    assert_eq!(log.entries.len(), 1);
    for index in 0..300 {
        log.record(format!("event {index}"));
    }
    assert_eq!(log.entries.len(), 160);
}
#[test]
fn all_pages_and_states_render_at_supported_sizes_and_dpi() {
    for dpi in [1.0, 1.25, 1.5, 1.75, 2.0] {
        for size in [vec2(620.0, 580.0), vec2(1114.0, 848.0), vec2(1500.0, 960.0)] {
            let mut d = dashboard(fixture());
            let ctx = Context::default();
            design::fonts(&ctx);
            for activity in [
                Activity::Unknown,
                Activity::Watching,
                Activity::Observation,
                Activity::DetectionUnavailable,
                Activity::Capturing,
                Activity::WaitingForInference,
                Activity::Unloading,
                Activity::Paused,
                Activity::ManualHold,
                Activity::Countdown,
                Activity::Restoring,
                Activity::Recovery,
                Activity::PartialFailure,
                Activity::Verifying,
                Activity::Coexistence,
            ] {
                let mut s = fixture();
                s.activity = activity;
                let input = RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                    ..Default::default()
                };
                ctx.set_pixels_per_point(dpi);
                let output = ctx.run(input, |ctx| d.draw(ctx, &s));
                assert!(!output.shapes.is_empty());
            }
            for page in [Page::Games, Page::Running, Page::Ignored, Page::Activity] {
                d.page = page;
                let output = ctx.run(
                    RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                        ..Default::default()
                    },
                    |ctx| d.draw(ctx, &fixture()),
                );
                assert!(!output.shapes.is_empty());
            }
            for page in [
                SettingsPage::General,
                SettingsPage::Detection,
                SettingsPage::LMStudio,
                SettingsPage::Ollama,
                SettingsPage::Apps,
                SettingsPage::Recovery,
                SettingsPage::Diagnostics,
            ] {
                d.page = Page::Games;
                d.settings_page = page;
                let mut s = fixture();
                s.config.advanced_settings_visible = true;
                let output = ctx.run(
                    RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                        ..Default::default()
                    },
                    |ctx| d.draw(ctx, &s),
                );
                assert!(!output.shapes.is_empty());
            }
        }
    }
}
/// Render only fictional state. No watcher, provider, startup or config writes.
#[test]
#[ignore = "opens an isolated renderer window for visual review"]
fn ui_design_review_snapshots() {
    const STAGES: usize = 19;
    struct Review {
        d: Dashboard,
        stage: usize,
        frames: u32,
        start: Instant,
    }
    impl eframe::App for Review {
        fn update(&mut self, ctx: &Context, frame: &mut eframe::Frame) {
            // Exercise caption tinting on the isolated review window too.
            if let Some(owner) = native_window(frame) {
                self.d.owner = owner;
            }
            assert!(
                self.start.elapsed() < Duration::from_secs(60),
                "Screenshot renderer timed out"
            );
            let mut image = None;
            ctx.input(|i| {
                for e in &i.events {
                    if let Event::Screenshot { image: shot, .. } = e {
                        image = Some(shot.clone());
                    }
                }
            });
            if let Some(image) = image {
                let bytes = image
                    .pixels
                    .iter()
                    .flat_map(|pixel| pixel.to_array())
                    .collect::<Vec<_>>();
                let path = format!("scratch/ui-review/{:02}.png", self.stage);
                image::save_buffer(
                    &path,
                    &bytes,
                    image.size[0] as u32,
                    image.size[1] as u32,
                    image::ColorType::Rgba8,
                )
                .unwrap();
                self.stage += 1;
                self.frames = 0;
                if self.stage == STAGES {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                    return;
                }
            }
            if self.stage >= STAGES {
                return;
            }
            let mut s = fixture();
            self.d.page = Page::Games;
            self.d.modal = None;
            let size = match self.stage {
                1 => vec2(620.0, 580.0),
                2 => vec2(1500.0, 960.0),
                _ => vec2(1114.0, 848.0),
            };
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
            match self.stage {
                3 => self.d.page = Page::Running,
                4 => {
                    self.d.page = Page::Ignored;
                    let path = s.games[0].path.clone();
                    set_ignored(&mut s.config, &path, true);
                }
                5..=11 => {
                    s.config.advanced_settings_visible = true;
                    self.d.settings_page = [
                        SettingsPage::General,
                        SettingsPage::Detection,
                        SettingsPage::LMStudio,
                        SettingsPage::Ollama,
                        SettingsPage::Apps,
                        SettingsPage::Recovery,
                        SettingsPage::Diagnostics,
                    ][self.stage - 5];
                }
                12 => {
                    self.d.modal =
                        Some(Modal::Resume(s.restore_offer.clone().unwrap(), vec![false]))
                }
                13 => {
                    self.d.modal = Some(Modal::Add {
                        name: "Fixture game".into(),
                        path: r"D:\Fixture Games\play.exe".into(),
                        auto: true,
                    })
                }
                14 => self.d.page = Page::Activity,
                15 => {
                    s.activity = Activity::PartialFailure;
                    s.config.appearance = crate::config::Appearance::Light;
                }
                16 | 18 => {
                    s.activity = Activity::Watching;
                    s.pending = false;
                    s.active_games.clear();
                    s.restore_offer = None;
                    s.provider_statuses[0].state = State::Restored;
                    s.provider_statuses[0].pending = false;
                    if self.stage == 18 {
                        s.config.appearance = crate::config::Appearance::Light;
                    }
                }
                17 => {
                    s.activity = Activity::Capturing;
                    s.pending = false;
                    s.provider_statuses[0].state = State::Pausing;
                }
                _ => (),
            }
            self.d.draw(ctx, &s);
            self.frames += 1;
            if self.frames == 6 {
                ctx.send_viewport_cmd(ViewportCommand::Screenshot(Default::default()));
            }
            ctx.request_repaint_after(Duration::from_millis(40));
        }
    }
    /// Stand-in artwork, so the review shows icon tiles without reading any game.
    fn tile(size: usize, seed: usize) -> ColorImage {
        let tint = [[96, 168, 92], [70, 96, 150], [150, 132, 96], [60, 104, 190]][seed % 4];
        let mut image = ColorImage::filled([size, size], Color32::BLACK);
        for (index, pixel) in image.pixels.iter_mut().enumerate() {
            let (x, y) = (index % size, index / size);
            let shade = 0.55 + 0.45 * (x + y) as f32 / (2 * size) as f32;
            let band = if (y * 5 / size + seed).is_multiple_of(2) {
                1.0
            } else {
                0.82
            };
            let channel = |value: u8| (value as f32 * shade * band) as u8;
            *pixel = Color32::from_rgb(channel(tint[0]), channel(tint[1]), channel(tint[2]));
        }
        image
    }
    std::fs::create_dir_all("scratch/ui-review").unwrap();
    eframe::run_native(
        "GamePause isolated UI review",
        native_options(),
        Box::new(|cc| {
            design::fonts(&cc.egui_ctx);
            let mut d = dashboard(fixture());
            d.selected = Some(fixture().games[3].path.clone());
            for (seed, game) in fixture().games.iter().enumerate() {
                for size in [64, 128] {
                    d.icons
                        .preload(&cc.egui_ctx, &game.path, size, tile(size as usize, seed));
                }
            }
            Ok(Box::new(Review {
                d,
                stage: 0,
                frames: 0,
                start: Instant::now(),
            }))
        }),
    )
    .unwrap();
    assert!(std::path::Path::new("scratch/ui-review/18.png").exists());
}
