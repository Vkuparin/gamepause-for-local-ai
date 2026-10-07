use super::view_data::ignored;
use super::*;
#[test]
fn adding_executable_deduplicates_and_clears_ignore() {
    let mut config = Config::default();
    config.ignored_games.push(r"D:\Games\game.exe".into());
    add_game(&mut config, r"d:\games\GAME.exe".into(), "Game".into());
    add_game(&mut config, r"D:\Games\game.exe".into(), "Game".into());
    assert_eq!(config.extra_games.len(), 1);
    assert!(config.ignored_games.is_empty());
}
#[test]
fn scale_follows_dpi_at_100_150_200() {
    assert_eq!(scale(100, 96), 100);
    assert_eq!(scale(100, 144), 150);
    assert_eq!(scale(100, 192), 200);
    assert_eq!(scale(960, 96), 960);
    assert_eq!(scale(960, 144), 1440);
    assert_eq!(scale(960, 192), 1920);
}
#[test]
fn render_verify_report_shows_every_step_and_the_failing_one() {
    // A failed report must name each step, mark the failing one FAIL, and
    // carry the summary so the FEEDBACK line explains the outcome inline.
    let report = crate::engine::VerifyReport {
        steps: vec![
            crate::engine::VerifyStep {
                name: "capture".into(),
                ok: true,
                detail: "1 model(s) captured".into(),
            },
            crate::engine::VerifyStep {
                name: "unload".into(),
                ok: true,
                detail: "1 model(s) unloaded".into(),
            },
            crate::engine::VerifyStep {
                name: "verify-unloaded".into(),
                ok: false,
                detail: "models still loaded after unload".into(),
            },
        ],
        ok: false,
        summary: "Round-trip verify failed at: verify-unloaded".into(),
    };
    let rendered = super::render_verify_report(&report);
    for needle in [
        "capture: 1 model(s) captured (ok)",
        "unload: 1 model(s) unloaded (ok)",
        "verify-unloaded: models still loaded after unload (FAIL)",
        "Round-trip verify failed at: verify-unloaded",
    ] {
        assert!(rendered.contains(needle), "missing {needle} in: {rendered}");
    }

    // The success path renders every step as ok and the passing summary.
    let pass = crate::engine::VerifyReport {
        steps: vec![crate::engine::VerifyStep {
            name: "capture".into(),
            ok: true,
            detail: "0 model(s) captured".into(),
        }],
        ok: true,
        summary:
            "Round-trip verify passed: capture, unload, restore, and field-compare all succeeded"
                .into(),
    };
    let rendered = super::render_verify_report(&pass);
    assert!(rendered.contains("capture: 0 model(s) captured (ok)"));
    assert!(rendered.contains("Round-trip verify passed"));
    assert!(
        !rendered.contains("FAIL"),
        "a passing report must not say FAIL"
    );
}
#[test]
fn save_validation_routes_to_feedback_not_modal() {
    // A bad value yields the Err(String) variant — the exact value dispatch
    // routes to the FEEDBACK line (set_feedback), never to a modal.
    let cfg = crate::config::Config::default();
    let bad = super::apply_save(&cfg, "not-a-number", "127.0.0.1:1234");
    let err = bad.expect_err("a non-numeric delay must fail validation");
    assert!(
        err.contains("number of seconds"),
        "the feedback message should name the field; got {err}"
    );

    let cfg = crate::config::Config::default();
    assert!(
        super::apply_save(&cfg, "10", "   ").is_err(),
        "an empty API host must fail validation"
    );

    // a valid save succeeds and applies the edited fields
    let cfg = crate::config::Config::default();
    let ok = super::apply_save(&cfg, "45", " 127.0.0.1:8080 ")
        .expect("a valid delay + host should pass");
    assert_eq!(ok.restore_delay_seconds, 45.0);
    assert_eq!(ok.lm_endpoint(), "127.0.0.1:8080");
}
#[test]
fn rename_updates_the_matching_custom_entry() {
    let mut cfg = crate::config::Config::default();
    cfg.extra_games.push(crate::config::ExtraGame {
        name: "Old Name".into(),
        path: "C:\\Games\\App\\game.exe".into(),
    });
    let ok = super::apply_rename(&cfg, "c:/games/app/game.exe", "New Name")
        .expect("renaming an existing custom entry should succeed");
    assert_eq!(ok.extra_games[0].name, "New Name");
    assert_eq!(ok.extra_games[0].path, "C:\\Games\\App\\game.exe");

    let err = super::apply_rename(&cfg, "C:\\Games\\Nope\\missing.exe", "X")
        .expect_err("a non-custom path must be rejected");
    assert!(err.contains("not a custom entry"), "got: {err}");

    let err = super::apply_rename(&cfg, "C:\\Games\\App\\game.exe", "   ")
        .expect_err("a blank name must be rejected");
    assert!(err.contains("empty"), "got: {err}");
}

#[test]
fn ignoring_a_running_executable_uses_the_list_detection_reads() {
    let exe = r"D:\Fixture Games\Trine 4\bin\trine4.exe";
    let mut config = Config::default();
    config.ignored_games.push(exe.to_uppercase());
    exclude_executable(&mut config, exe);
    assert_eq!(config.excluded_paths, vec![exe.to_string()]);
    assert!(
        config.ignored_games.is_empty(),
        "an earlier entry in the wrong list is replaced"
    );
    // The scanner that decides triggers honours it for a launcher game.
    let scanner = crate::processes::Scanner::new(config.clone()).unwrap();
    let game = crate::discovery::Game::new("Steam", "1", "Trine 4", r"D:\Fixture Games\Trine 4");
    assert!(scanner.match_path(exe, &[game]).is_none());
    assert!(ignored(&config, exe), "and the Ignored page lists it");
    exclude_executable(&mut config, exe);
    assert_eq!(config.excluded_paths.len(), 1);
    set_ignored(&mut config, exe, false);
    assert!(config.excluded_paths.is_empty());
}
#[test]
fn unsaved_advanced_edits_survive_other_saves() {
    let base = Config::default();
    let mut draft = base.clone();
    draft.pause_hotkey = "Ctrl+Alt+P".into();
    draft.restore_delay_seconds = 45.;
    if let Some(crate::config::Provider::LMStudio { connection, .. }) = draft.providers.first_mut()
    {
        connection.endpoint = "127.0.0.1:4321".into();
    }
    // Meanwhile: an instant preference, a game rule and the Ollama entry.
    let mut saved = base.clone();
    saved.notifications_enabled = false;
    saved
        .ignored_games
        .push(r"D:\Fixture Games\Stardew Valley".into());
    if let Some(crate::config::Provider::Ollama { endpoint, .. }) = saved.providers.get_mut(1) {
        *endpoint = "127.0.0.1:11500".into();
    }
    let merged = rebase_draft(&base, &draft, &saved).unwrap();
    assert_eq!(merged.pause_hotkey, "Ctrl+Alt+P");
    assert_eq!(merged.restore_delay_seconds, 45.);
    assert_eq!(merged.lm_endpoint(), "127.0.0.1:4321");
    assert!(!merged.notifications_enabled);
    assert_eq!(merged.ignored_games, saved.ignored_games);
    assert_eq!(merged.providers[1].endpoint(), "127.0.0.1:11500");
    assert_ne!(merged, saved, "the draft is still unsaved");
    // A refresh that changes no setting leaves the draft exactly as typed.
    assert_eq!(rebase_draft(&base, &draft, &base).unwrap(), draft);
    // The same value changed on both sides: the edit on screen wins.
    let mut both = base.clone();
    both.pause_hotkey = "Ctrl+Alt+R".into();
    assert_eq!(
        rebase_draft(&base, &draft, &both).unwrap().pause_hotkey,
        "Ctrl+Alt+P"
    );
    // A merge that is not valid settings is refused rather than applied.
    let mut clash = base.clone();
    if let Some(crate::config::Provider::Ollama { endpoint, .. }) = clash.providers.get_mut(1) {
        *endpoint = "127.0.0.1:4321".into();
    }
    assert!(rebase_draft(&base, &draft, &clash).is_none());
}
#[test]
fn a_remembered_ask_answer_becomes_the_games_rule() {
    let mut s = Shared::default();
    s.active_games.push(crate::gameplay::fixtures::game(42, 10));
    let path = s.active_games[0].path.clone();
    assert!(asking_paths(&s).is_empty());
    set_ask(&mut s.config, &path, true);
    assert_eq!(asking_paths(&s), vec![path.clone()]);
    let always = answer_ask(&s.config, &asking_paths(&s), true);
    assert!(!always.asks(&path) && !ignored(&always, &path));
    let never = answer_ask(&s.config, &asking_paths(&s), false);
    assert!(!never.asks(&path) && ignored(&never, &path));
    // Other games keep their rules.
    let other = crate::gameplay::fixtures::game(7, 2).path;
    set_ask(&mut s.config, &other, true);
    assert!(answer_ask(&s.config, &[path], true).asks(&other));
}
#[test]
fn ask_and_ignore_are_exclusive_rules_for_one_game() {
    let mut config = Config::default();
    let path = r"D:\Games\Fixture";
    set_ask(&mut config, path, true);
    assert!(config.asks(path) && !ignored(&config, path));
    set_ignored(&mut config, path, true);
    assert!(!config.asks(path) && ignored(&config, path));
    set_ask(&mut config, path, true);
    assert!(config.asks(path) && !ignored(&config, path));
    set_ask(&mut config, path, false);
    assert!(!config.asks(path) && !ignored(&config, path));
}
