use gamepause_lmstudio::config::{self, Config, write_json};
use serde_json::json;
use std::{
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
static SERIAL: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let folder = std::env::temp_dir().join(format!(
            "gamepause-cli-integration-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&folder).unwrap();
        let config = Config {
            lms_path: folder.join("absent-lms.exe").to_string_lossy().into(),
            ..Default::default()
        };
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
        &json!({"message":"AI available","active_games":[],"recovery_pending":false}),
    )
    .unwrap();
    let lock = config::lock(&f.0).unwrap();
    let output = f.run(&["--status"]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("active_games=[]"));
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
