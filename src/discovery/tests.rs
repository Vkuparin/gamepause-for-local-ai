use super::{launchers::gog_games, *};
use std::fs;
#[test]
fn steam_metadata_refresh_finds_new_installation_without_restart() {
    let root = std::env::temp_dir().join(format!("gamepause-steam-refresh-{}", std::process::id()));
    let apps = root.join("steamapps");
    let common = apps.join("common");
    fs::create_dir_all(common.join("Fixture One")).unwrap();
    fs::create_dir_all(common.join("Fixture Two")).unwrap();
    let one = apps.join("appmanifest_900000001.acf");
    let two = apps.join("appmanifest_900000002.acf");
    fs::write(&one, r#""AppState" { "appid" "900000001" "name" "GamePause Fixture One" "installdir" "Fixture One" "StateFlags" "4" }"#).unwrap();
    let config = Config {
        steam_roots: vec![root.to_string_lossy().into_owned()],
        ..Config::default()
    };
    let mut discovery = Discovery::default();
    let initial = discovery.steam(&config).unwrap();
    assert!(initial.iter().any(|g| g.identity == "900000001"));
    assert!(!initial.iter().any(|g| g.identity == "900000002"));
    fs::write(&two, r#""AppState" { "appid" "900000002" "name" "GamePause Fixture Two" "installdir" "Fixture Two" "StateFlags" "4" }"#).unwrap();
    let refreshed = discovery.steam(&config).unwrap();
    assert!(refreshed.iter().any(|g| g.identity == "900000002"));
    fs::remove_file(one).unwrap();
    fs::remove_file(two).unwrap();
    fs::remove_dir(common.join("Fixture One")).unwrap();
    fs::remove_dir(common.join("Fixture Two")).unwrap();
    fs::remove_dir(common).unwrap();
    fs::remove_dir(apps).unwrap();
    fs::remove_dir(root).unwrap();
}
#[test]
fn background_utilities_are_not_games() {
    assert!(background_utility(&Game::new(
        "Steam",
        "431960",
        "Wallpaper Engine",
        r"D:\Steam\Wallpaper"
    )));
    assert!(!background_utility(&Game::new(
        "Steam",
        "292030",
        "The Witcher 3",
        r"D:\Steam\Witcher"
    )));
}
#[test]
fn gog_registry_rows_become_games_and_dlc_is_skipped() {
    let row = |id: &str, values: &[(&str, &str)]| {
        (
            id.to_owned(),
            values
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect::<BTreeMap<_, _>>(),
        )
    };
    let games = gog_games(vec![
        row(
            "1001",
            &[
                ("gameName", "Fixture Quest"),
                ("path", r"D:\GOG\Fixture Quest"),
                ("exe", r"D:\GOG\Fixture Quest\bin\quest.exe"),
                ("dependsOn", ""),
            ],
        ),
        row(
            "1002",
            &[
                ("gameName", "Fixture Quest: Expansion"),
                ("path", r"D:\GOG\Fixture Quest"),
                ("dependsOn", "1001"),
            ],
        ),
        row("1003", &[("exe", r"D:\GOG\Second Game\second.exe")]),
        row("1004", &[("gameName", "No location")]),
    ]);
    assert_eq!(
        games,
        vec![
            Game::new("GOG", "1001", "Fixture Quest", r"D:\GOG\Fixture Quest"),
            Game::new("GOG", "1003", "Second Game", r"D:\GOG\Second Game"),
        ]
    );
}
#[test]
fn path_boundaries() {
    assert!(inside(
        r"D:\Games\Witcher\bin\game.exe",
        r"d:/games/witcher"
    ));
    assert!(!inside(r"D:\Games\Witcher2\game.exe", r"D:\Games\Witcher"));
}
#[test]
fn in_place_path_checks_agree_with_canonical() {
    let paths = [
        "",
        r"\",
        r"D:\Games\Witcher",
        r"d:/games/witcher/",
        r"D:\Games\Witcher\",
        r"D:\Games\Witcher\bin\game.exe",
        r"D:\Games\Witcher2",
        r"D:\Games\WITCHER\Ünïcode\Spiel.EXE",
        r"D:\Games",
    ];
    for path in paths {
        for root in paths {
            let (p, r) = (canonical(path), canonical(root));
            assert_eq!(same_path(path, root), p == r, "{path:?} == {root:?}");
            assert_eq!(
                inside(path, root),
                !r.is_empty() && (p == r || p.starts_with(&format!("{r}\\"))),
                "{path:?} inside {root:?}"
            );
        }
    }
}
#[test]
fn worker_survives_a_panicking_adapter_and_keeps_last_good_inventory() {
    // P0-2: the discovery worker runs adapters on a background thread. A
    // panic there (a future adapter with an unwrapping bug, a filesystem
    // race in the Xbox `.GamingRoot` reader) must not kill the worker and
    // orphan the last good inventory. This drives a REAL `panic!` through
    // the exact `run_catchable` path the worker uses (via
    // `recover_refresh`) and verifies the worker's contract: no
    // propagation, the payload is recorded, and the last successful
    // inventory is retained.
    let mut discovery = Discovery::default();
    // Seed a "last good" inventory the way a prior successful refresh would.
    let good = Game::new("Steam", "100", "Last Good Game", std::env::temp_dir());
    discovery
        .retained
        .insert("Steam".into(), vec![good.clone()]);
    // `refresh` clears errors and re-runs every adapter, so a panic in a
    // body that models a panicking adapter is equivalent to one of the real
    // adapters panicking mid-refresh.
    let (games, payload) = discovery.run_catchable(|_d| panic!("adapter blew up"));
    assert_eq!(payload, "adapter blew up", "panic payload must be captured");
    assert!(
        discovery.errors.contains_key("Discovery"),
        "the panic must be recorded in discovery.errors for the UI/doctor panel"
    );
    assert_eq!(
        games.len(),
        1,
        "last good inventory must be retained after a panic"
    );
    assert_eq!(games[0].identity, good.identity);
    assert_eq!(games[0].path, good.path);
}
#[test]
fn empty_vdf_strings() {
    let v = parse_vdf(r#""root" { "empty" "" "nested" { "path" "D:\\Games" } }"#).unwrap();
    assert_eq!(v["root"]["empty"], "");
    assert_eq!(v["root"]["nested"]["path"], r"D:\Games");
}
#[test]
fn incomplete_vdf_refused() {
    assert!(parse_vdf(r#""root" { "key" "value" "#).is_err());
}
#[test]
fn protobuf_boundaries() {
    assert_eq!(
        protobuf_fields(&[10, 3, b'a', b'b', b'c']).unwrap()[0].1,
        b"abc"
    );
    assert!(protobuf_fields(&[10, 50, 1]).is_err());
    assert!(protobuf_fields(&[128]).is_err());
}
#[test]
fn excessive_metadata_nesting_is_rejected_without_stack_exhaustion() {
    let input = "\"key\" {".repeat(100) + &"}".repeat(100);
    assert!(
        parse_vdf(&input)
            .unwrap_err()
            .to_string()
            .contains("nesting")
    );
}
