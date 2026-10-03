use crate::{
    config::{self, Config, write_json},
    discovery::{Discovery, Game},
    engine::Engine,
    lmstudio::{Backend, LMStudio},
    processes::{ActiveGame, RunningApp, Scanner},
    tray,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Debug)]
pub enum Action {
    Pause,
    Restore,
    Disable,
    Refresh,
    Quit,
    Settings(Box<Config>),
}
#[derive(Clone, Default)]
pub struct Shared {
    pub message: String,
    pub disabled: bool,
    pub manual_pause: bool,
    pub pending: bool,
    pub active_mode: bool,
    pub config: Config,
    pub games: Vec<Game>,
    pub active_games: Vec<ActiveGame>,
    pub running_apps: Vec<RunningApp>,
    pub discovery_errors: std::collections::BTreeMap<String, String>,
    pub settings_error: String,
    pub revision: u64,
    pub discovery_ready: bool,
}
pub type SharedState = Arc<Mutex<Shared>>;
#[derive(Default)]
struct Options {
    folder: Option<PathBuf>,
    headless: bool,
    background: bool,
    active: bool,
    observe: bool,
    discover: bool,
    doctor: bool,
    restore: bool,
    duration: f64,
    version: bool,
    help: bool,
}
fn options() -> Result<Options> {
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                options.folder = Some(args.next().context("--data-dir needs a path")?.into())
            }
            "--headless" => options.headless = true,
            "--background" => options.background = true,
            "--active" => options.active = true,
            "--observe" => options.observe = true,
            "--discover" => options.discover = true,
            "--doctor" => options.doctor = true,
            "--restore" => options.restore = true,
            "--duration" => {
                options.duration = args.next().context("--duration needs seconds")?.parse()?
            }
            "--version" => options.version = true,
            "--help" | "-h" => options.help = true,
            _ => bail!("Unknown argument {arg}"),
        }
    }
    if options.active && options.observe {
        bail!("Choose active or observe, not both");
    }
    if !options.duration.is_finite() || options.duration < 0. {
        bail!("duration must be finite and nonnegative");
    }
    Ok(options)
}
pub fn main(console: bool) -> Result<()> {
    let args = options()?;
    if args.version {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.help {
        println!(
            "GamePause for LM Studio\n--headless --active --observe --duration SECONDS\n--doctor --discover --restore --data-dir PATH\nDefault: automatic pausing with a native dashboard. --background starts in the tray. --observe is a diagnostic override."
        );
        return Ok(());
    }
    let folder = args.folder.unwrap_or_else(config::data_directory);
    fs::create_dir_all(&folder)?;
    let folder = fs::canonicalize(folder)?;
    install_panic_hook(&folder);
    let _lock = match config::lock(&folder) {
        Ok(lock) => lock,
        Err(e) => {
            if !args.headless
                && !args.doctor
                && !args.discover
                && !args.restore
                && tray::show_existing(&folder)
            {
                return Ok(());
            }
            return Err(e);
        }
    };
    let mut config = Config::load(&folder.join("config.json"))?;
    if args.active {
        config.mode = "active".into();
    }
    if args.observe {
        config.mode = "observe".into();
    }
    if args.discover {
        let mut discovery = Discovery::default();
        let games = discovery.refresh(&config, 0., true, false);
        let report = json!({"games":games,"errors":discovery.errors});
        write_json(&folder.join("discovery-report.json"), &report)?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let mut backend = LMStudio::new(config.clone());
    // Observe mode must work even when LM Studio is absent.
    if args.doctor {
        let backend = backend.as_mut().map_err(|e| anyhow::anyhow!("{e:#}"))?;
        let snapshot = backend.snapshot()?;
        let report = json!({"cli":backend.lms,"server":snapshot.server,"snapshot_ok":true,"models":snapshot.models.iter().map(|m|json!({"identifier":m.identifier,"model_key":m.model_key,"namespace":m.namespace,"ttl_ms":m.ttl_ms,"load_fields":m.load_config["fields"].as_array().map(Vec::len)})).collect::<Vec<_>>()});
        write_json(&folder.join("doctor-report.json"), &report)?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let backend = OptionalBackend {
        backend: backend.ok(),
        config: config.clone(),
    };
    let mut engine = Engine::new(config.clone(), backend, folder.join("state.json"))?;
    if args.restore {
        let mut discovery = Discovery::default();
        let games = discovery.refresh(&config, 0., true, false);
        let mut scanner = Scanner::new(config)?;
        if !scanner.scan(&games)?.is_empty() {
            bail!("Close the detected game before restoring AI");
        }
        engine.restore(&mut || {
            scanner
                .scan(&games)
                .map(|active| !active.is_empty())
                .unwrap_or(true)
        })?;
        println!("{}", engine.message);
        return Ok(());
    }
    let state = Arc::new(Mutex::new(Shared {
        message: "Starting GamePause".into(),
        disabled: false,
        manual_pause: false,
        pending: engine.state.is_some(),
        active_mode: config.mode == "active",
        config: config.clone(),
        games: vec![],
        active_games: vec![],
        running_apps: vec![],
        discovery_errors: Default::default(),
        settings_error: String::new(),
        revision: 0,
        discovery_ready: false,
    }));
    let (tx, rx) = mpsc::channel();
    if args.headless {
        run(engine, folder, state, rx, args.duration, console)?;
    } else {
        let worker_state = state.clone();
        let worker_folder = folder.clone();
        let handle = std::thread::spawn(move || {
            let result = run(
                engine,
                worker_folder.clone(),
                worker_state.clone(),
                rx,
                args.duration,
                false,
            );
            if let Err(ref e) = result {
                log(&worker_folder, &format!("Monitoring stopped: {e:#}"));
                if let Ok(mut shared) = worker_state.lock() {
                    shared.message = format!("Stopped: {e:#}");
                }
            }
            tray::request_exit();
            result
        });
        tray::run(state, tx, folder, !args.background)?;
        handle
            .join()
            .map_err(|_| anyhow::anyhow!("Monitoring thread panicked"))??;
    }
    Ok(())
}
struct OptionalBackend {
    backend: Option<LMStudio>,
    config: Config,
}
impl OptionalBackend {
    fn get(&mut self) -> Result<&mut LMStudio> {
        if self.backend.is_none() {
            self.backend = Some(LMStudio::new(self.config.clone())?);
        }
        Ok(self.backend.as_mut().unwrap())
    }
}
impl Backend for OptionalBackend {
    fn snapshot(&mut self) -> Result<crate::lmstudio::Snapshot> {
        self.get()?.snapshot()
    }
    fn loaded(&mut self) -> Result<Vec<Value>> {
        self.get()?.loaded()
    }
    fn stop_server(&mut self) -> Result<()> {
        self.get()?.stop_server()
    }
    fn start_server(&mut self, p: u16) -> Result<()> {
        self.get()?.start_server(p)
    }
    fn unload(&mut self, id: &str) -> Result<()> {
        self.get()?.unload(id)
    }
    fn restore(&mut self, m: &crate::lmstudio::Model) -> Result<()> {
        self.get()?.restore(m)
    }
}
fn apply_action(
    action: Action,
    engine: &mut Engine<OptionalBackend>,
    folder: &std::path::Path,
    scanner: &mut Scanner,
    games: &[Game],
    now: f64,
    state: &SharedState,
) -> Result<bool> {
    match action {
        Action::Settings(updated) => {
            let result = (|| -> Result<()> {
                updated.validate()?;
                let replacement = Scanner::new((*updated).clone())?;
                write_json(&folder.join("config.json"), updated.as_ref())?;
                engine.backend.config = (*updated).clone();
                engine.backend.backend = None;
                engine.config = *updated;
                engine.settings_changed();
                *scanner = replacement;
                Ok(())
            })();
            if let Ok(mut shared) = state.lock() {
                shared.settings_error = result
                    .as_ref()
                    .err()
                    .map(|e| format!("Could not save settings: {e:#}"))
                    .unwrap_or_default();
                shared.config = engine.config.clone();
                shared.revision += 1;
            }
            return Ok(result.is_ok());
        }
        Action::Disable => {
            let mut updated = engine.config.clone();
            updated.automation_enabled = !updated.automation_enabled;
            return apply_action(
                Action::Settings(Box::new(updated)),
                engine,
                folder,
                scanner,
                games,
                now,
                state,
            );
        }
        Action::Pause => {
            if engine.config.mode == "active" {
                engine.manual_pause = !engine.manual_pause;
            }
        }
        Action::Restore => {
            if engine.config.mode == "observe" {
                return Ok(false);
            }
            if !state.lock().map(|s| s.discovery_ready).unwrap_or(false) {
                return Ok(false);
            }
            engine.manual_pause = false;
            let mut guard_config = engine.config.clone();
            guard_config.excluded_paths.clear();
            guard_config.excluded_executables.clear();
            let mut guard = Scanner::new(guard_config)?;
            let guard_games = recovery_games(engine, games);
            let result =
                engine.restore(&mut || guard.scan(&guard_games).map_or(true, |a| !a.is_empty()));
            engine.restore_failed = true;
            engine.attempt(result, now);
        }
        Action::Refresh => return Ok(true),
        Action::Quit => (),
    }
    Ok(false)
}
fn recovery_games(engine: &Engine<OptionalBackend>, games: &[Game]) -> Vec<Game> {
    let mut known = games.to_vec();
    for game in &engine.remembered_games {
        if !known.iter().any(|g| {
            crate::discovery::canonical(&g.path) == crate::discovery::canonical(&game.path)
        }) {
            known.push(game.clone());
        }
    }
    known
}
/// Counts consecutive hard failures in the worker loop and, once a threshold
/// is exceeded, flips the shared status to a visible "needs attention" state.
/// Transient failures are logged and the loop continues — the monitor must
/// degrade, not die, so a pending restore is never orphaned.
#[derive(Default)]
struct Monitor {
    failures: u32,
    needs_attention: bool,
}
impl Monitor {
    const NEEDS_ATTENTION_AFTER: u32 = 10;
    fn handle(&mut self, err: &str, folder: &std::path::Path, state: &SharedState) {
        self.failures += 1;
        log(
            folder,
            &format!("Transient failure ({} consecutive): {err}", self.failures),
        );
        if !self.needs_attention && self.failures >= Self::NEEDS_ATTENTION_AFTER {
            self.needs_attention = true;
            if let Ok(mut shared) = state.lock() {
                shared.message =
                    "Needs attention — repeated data-directory failures; check the data dir".into();
            }
        }
    }
    fn reset(&mut self) {
        self.failures = 0;
    }
}
fn run(
    mut engine: Engine<OptionalBackend>,
    folder: PathBuf,
    state: SharedState,
    commands: mpsc::Receiver<Action>,
    duration: f64,
    console: bool,
) -> Result<()> {
    let mut scanner = Scanner::new(engine.config.clone())?;
    let mut guard_config = engine.config.clone();
    guard_config.excluded_paths.clear();
    guard_config.excluded_executables.clear();
    let mut recovery_scanner = Scanner::new(guard_config)?;
    let mut games = vec![];
    let mut errors = std::collections::BTreeMap::<String, String>::new();
    let (request_tx, request_rx) = mpsc::channel::<(Config, f64, bool, bool)>();
    let (result_tx, result_rx) = mpsc::channel();
    let worker_log = folder.clone();
    let inventory_worker = std::thread::spawn(move || {
        let mut discovery = Discovery::default();
        while let Ok((config, now, force, defer)) = request_rx.recv() {
            let (games, panic_payload) = discovery.recover_refresh(&config, now, force, defer);
            if !panic_payload.is_empty() {
                crate::app::log(
                    &worker_log,
                    &format!("Inventory scan recovered from panic: {panic_payload}"),
                );
            }
            if result_tx
                .send((
                    games,
                    discovery.errors.clone(),
                    discovery.steam_libraries.clone(),
                ))
                .is_err()
            {
                break;
            }
        }
    });
    let start = Instant::now();
    let mut next_inventory = 0.;
    let mut last_status = Value::Null;
    let mut inventory_pending = false;
    let mut force_requested = false;
    let mut queued = None;
    let mut inventory_ready = false;
    let mut steam_roots = vec![];
    let mut monitor = Monitor::default();
    loop {
        let now = start.elapsed().as_secs_f64();
        if duration > 0. && now >= duration {
            break;
        }
        let actions = queued.take().into_iter().chain(commands.try_iter());
        let mut quit = false;
        for action in actions {
            if matches!(action, Action::Quit) {
                quit = true;
                break;
            }
            if apply_action(
                action,
                &mut engine,
                &folder,
                &mut scanner,
                &games,
                now,
                &state,
            )? {
                force_requested = true;
            }
        }
        if quit {
            break;
        }
        let tick = (|| -> Result<()> {
            if let Ok((updated, updated_errors, roots)) = result_rx.try_recv() {
                inventory_ready = true;
                steam_roots = roots;
                games = updated;
                errors = updated_errors;
                inventory_pending = false;
                write_json(&folder.join("inventory.json"), &games)?;
                log(
                    &folder,
                    &format!("Inventory refreshed: {} installed locations", games.len()),
                );
                if let Ok(mut shared) = state.lock() {
                    shared.revision += 1;
                    shared.discovery_ready = true;
                }
            }
            let scanned = scanner.scan(&games)?;
            let new_candidate = scanner.new_game_candidate(&games, &steam_roots);
            force_requested |= new_candidate;
            let guard_games = recovery_games(&engine, &games);
            let all_active = if engine.state.is_some() {
                recovery_scanner.scan(&guard_games)?
            } else {
                scanned.clone()
            };
            let active = scanned
                .into_iter()
                .filter(|game| {
                    !engine.config.ignored_games.iter().any(|path| {
                        crate::discovery::canonical(path) == crate::discovery::canonical(&game.path)
                    })
                })
                .collect::<Vec<_>>();
            let gaming = !active.is_empty() || (engine.state.is_some() && !all_active.is_empty());
            if !inventory_pending && (force_requested || now >= next_inventory) {
                request_tx.send((
                    engine.config.clone(),
                    now,
                    force_requested,
                    gaming || new_candidate,
                ))?;
                inventory_pending = true;
                force_requested = false;
                next_inventory = now + engine.config.discovery_seconds;
            }
            let records = games
                .iter()
                .filter(|g| {
                    all_active.iter().any(|a| {
                        crate::discovery::canonical(&a.path) == crate::discovery::canonical(&g.path)
                    })
                })
                .cloned()
                .collect();
            engine.remember_games(records)?;
            if inventory_ready {
                engine.step(gaming, now, &mut || {
                    recovery_scanner
                        .scan(&guard_games)
                        .map_or(true, |a| !a.is_empty())
                });
            } else {
                engine.message = "Discovering installed games — existing recovery is held until discovery finishes".into();
            }
            if inventory_ready
                && engine.state.is_none()
                && !gaming
                && engine.config.automation_enabled
                && engine.config.mode != "observe"
                && !scanner.lmstudio_running()
            {
                engine.message =
                    "Waiting for LM Studio — open it; automatic pausing will resume".into();
            }
            let status = json!({"version":env!("CARGO_PKG_VERSION"),"implementation":"Rust","mode":engine.config.mode,"automation_enabled":engine.config.automation_enabled,"message":engine.message,"active_games":active,"installed_locations":games.len(),"detection_disabled":engine.disabled,"manual_pause":engine.manual_pause,"last_error":engine.last_error,"discovery_errors":errors,"inaccessible_processes":scanner.inaccessible,"recovery_pending":engine.state.is_some()});
            if status != last_status {
                write_json(&folder.join("status.json"), &status)?;
                log(&folder, &engine.message);
                if console {
                    println!("{}", engine.message);
                }
                last_status = status;
            }
            if let Ok(mut shared) = state.lock() {
                shared.message = engine.message.clone();
                shared.disabled = engine.disabled;
                shared.manual_pause = engine.manual_pause;
                shared.pending = engine.state.is_some();
                shared.active_mode = engine.config.mode == "active";
                shared.config = engine.config.clone();
                shared.games = games.clone();
                shared.active_games = all_active;
                // Only copy the running-app list while the dashboard asks for it.
                shared.running_apps = if crate::dashboard::needs_running_apps() {
                    scanner.running_apps()
                } else {
                    vec![]
                };
                shared.discovery_errors = errors.clone();
            }
            Ok(())
        })();
        match tick {
            Ok(()) => {
                monitor.reset();
                if monitor.needs_attention {
                    monitor.needs_attention = false;
                    if let Ok(mut shared) = state.lock() {
                        shared.message = "Recovered — monitoring resumed".into();
                    }
                    log(
                        &folder,
                        "Recovered from transient failures — monitoring resumed",
                    );
                }
            }
            Err(err) => {
                let msg = format!("{err:#}");
                monitor.handle(&msg, &folder, &state);
            }
        }
        match commands.recv_timeout(Duration::from_secs_f64(engine.config.poll_seconds)) {
            Ok(Action::Quit) => break,
            Ok(action) => queued = Some(action),
            Err(mpsc::RecvTimeoutError::Timeout) => (),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if duration == 0. {
                    break;
                }
            }
        }
    }
    drop(request_tx);
    let _ = inventory_worker.join();
    Ok(())
}
/// Install a global panic hook (P0-3): any panic on any thread is appended to
/// the app log with its message and location, then the previous hook is run so
/// the default behavior (and any debugger output) is preserved. Best-effort:
/// the log write itself can never panic.
pub fn install_panic_hook(folder: &std::path::Path) {
    // The hook is 'static, so it must own the path rather than borrow it.
    let owned = folder.to_path_buf();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown location>".into());
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic payload".into()
        };
        log(&owned, &format!("PANIC in {}: {payload}", location));
        previous(info);
    }));
}
pub fn log(folder: &std::path::Path, message: &str) {
    let file = folder.join("gamepause.log");
    if fs::metadata(&file).is_ok_and(|m| m.len() > 1_000_000) {
        for i in (1..3).rev() {
            let _ = fs::rename(
                folder.join(format!("gamepause.log.{i}")),
                folder.join(format!("gamepause.log.{}", i + 1)),
            );
        }
        let _ = fs::rename(&file, folder.join("gamepause.log.1"));
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(file) {
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let _ = writeln!(file, "{timestamp} {message}");
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manual_restore_waits_for_first_discovery_and_retains_journal() {
        let folder =
            std::env::temp_dir().join(format!("gamepause-startup-recovery-{}", std::process::id()));
        let path = folder.join("state.json");
        write_json(&path,&json!({"schema":2,"server":{"running":false,"port":1234},"server_stopped":false,"models":[],"pause_complete":true})).unwrap();
        let before = fs::read(&path).unwrap();
        let config = Config::default();
        let mut engine = Engine::new(
            config.clone(),
            OptionalBackend {
                backend: None,
                config: config.clone(),
            },
            path.clone(),
        )
        .unwrap();
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
        assert!(engine.state.is_some());
        assert_eq!(before, fs::read(&path).unwrap());
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn worker_survives_repeated_data_dir_failures_and_flags_attention() {
        // The data dir is a *file*, so every write_json (status.json, and
        // inventory.json once discovery replies) fails. Pre-P0-1 this was fatal:
        // run() returned Err and the app died, orphaning any pending restore.
        // Now each tick degrades, and after a bounded number of consecutive
        // failures the shared status flips to a visible "needs attention" state.
        let base =
            std::env::temp_dir().join(format!("gamepause-resilience-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let folder = base.join("data"); // created as a FILE below
        fs::write(&folder, "block").unwrap();

        let config = Config::default();
        let engine = Engine::new(
            config.clone(),
            OptionalBackend {
                backend: None,
                config: config.clone(),
            },
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
    fn settings_apply_live_persist_and_invalid_updates_leave_previous_settings() {
        let folder =
            std::env::temp_dir().join(format!("gamepause-live-settings-{}", std::process::id()));
        let config = Config::default();
        let backend = OptionalBackend {
            backend: None,
            config: config.clone(),
        };
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
        invalid.api_host = "example.com:1234".into();
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
            log_text.contains("app.rs"),
            "log should contain the panic location (file): {log_text:?}"
        );
        std::panic::set_hook(previous);
        let _ = fs::remove_file(folder.join("gamepause.log"));
    }
}
