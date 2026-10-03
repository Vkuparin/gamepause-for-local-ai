use crate::{
    config::{self, Config, write_json},
    discovery::{Discovery, Game},
    engine::Engine,
    lmstudio::{Backend, LMStudio},
    processes::Scanner,
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

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Pause,
    Restore,
    Disable,
    Refresh,
    Quit,
}
pub struct Shared {
    pub message: String,
    pub disabled: bool,
    pub manual_pause: bool,
    pub pending: bool,
    pub active_mode: bool,
}
pub type SharedState = Arc<Mutex<Shared>>;
#[derive(Default)]
struct Options {
    folder: Option<PathBuf>,
    headless: bool,
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
            "GamePause for LM Studio\n--headless --active --observe --duration SECONDS\n--doctor --discover --restore --data-dir PATH\nDefault: observation mode with Windows tray. Configuration takes effect on restart."
        );
        return Ok(());
    }
    let folder = args.folder.unwrap_or_else(config::data_directory);
    fs::create_dir_all(&folder)?;
    let folder = fs::canonicalize(folder)?;
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
    let _lock = config::lock(&folder)?;
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
    let backend = if config.mode == "active" || args.restore {
        Some(backend?)
    } else {
        None
    };
    let mut engine = Engine::new(
        config.clone(),
        OptionalBackend(backend),
        folder.join("state.json"),
    )?;
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
        tray::run(state, tx, folder)?;
        handle
            .join()
            .map_err(|_| anyhow::anyhow!("Monitoring thread panicked"))??;
    }
    Ok(())
}
struct OptionalBackend(Option<LMStudio>);
impl OptionalBackend {
    fn get(&mut self) -> Result<&mut LMStudio> {
        self.0
            .as_mut()
            .context("LM Studio control is unavailable in observe mode")
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
fn run(
    mut engine: Engine<OptionalBackend>,
    folder: PathBuf,
    state: SharedState,
    commands: mpsc::Receiver<Action>,
    duration: f64,
    console: bool,
) -> Result<()> {
    let config = engine.config.clone();
    let mut scanner = Scanner::new(config.clone())?;
    let mut games = vec![];
    let mut errors = std::collections::BTreeMap::<String, String>::new();
    let (request_tx, request_rx) = mpsc::channel::<(f64, bool, bool)>();
    let (result_tx, result_rx) = mpsc::channel();
    let discovery_config = config.clone();
    let inventory_worker = std::thread::spawn(move || {
        let mut discovery = Discovery::default();
        while let Ok((now, force, defer)) = request_rx.recv() {
            let mut games = discovery.refresh(&discovery_config, now, force, defer);
            let installed = games.len();
            for root in &discovery.steam_libraries {
                games.push(Game::new("Steam", root, "Steam game", root));
            }
            if result_tx
                .send((games, installed, discovery.errors.clone()))
                .is_err()
            {
                break;
            }
        }
    });
    let start = Instant::now();
    let (mut next_inventory, mut installed, mut last_status, mut inventory_pending) =
        (0., 0, Value::Null, false);
    let mut gaming = false;
    let mut force_requested = false;
    let result = (|| {
        loop {
            let now = start.elapsed().as_secs_f64();
            if duration > 0. && now >= duration {
                break;
            }
            let mut force = std::mem::take(&mut force_requested);
            let mut quit = false;
            for action in commands.try_iter() {
                match action {
                    Action::Quit => quit = true,
                    Action::Refresh => force = true,
                    Action::Disable => engine.disabled = !engine.disabled,
                    Action::Pause => {
                        if config.mode == "active" {
                            engine.manual_pause = !engine.manual_pause;
                        }
                    }
                    Action::Restore => {
                        engine.manual_pause = false;
                        if !gaming && config.mode == "active" {
                            let result = engine.restore(&mut || {
                                scanner.scan(&games).map_or(true, |a| !a.is_empty())
                            });
                            engine.attempt(result, now);
                        }
                    }
                }
            }
            if quit {
                break;
            }
            if let Ok((updated, count, updated_errors)) = result_rx.try_recv() {
                games = updated;
                installed = count;
                errors = updated_errors;
                inventory_pending = false;
                write_json(&folder.join("inventory.json"), &games[..installed])?;
                log(
                    &folder,
                    &format!("Inventory refreshed: {installed} installed locations"),
                );
            }
            if !inventory_pending && (force || now >= next_inventory) {
                request_tx.send((now, force, gaming))?;
                inventory_pending = true;
                next_inventory = now + config.discovery_seconds;
            }
            let active = scanner.scan(&games)?;
            gaming = !active.is_empty();
            engine.step(gaming, now, &mut || {
                scanner.scan(&games).map_or(true, |a| !a.is_empty())
            });
            let status = json!({"version":env!("CARGO_PKG_VERSION"),"implementation":"Rust","mode":config.mode,"message":engine.message,"active_games":active,"installed_locations":installed,"detection_disabled":engine.disabled,"manual_pause":engine.manual_pause,"last_error":engine.last_error,"discovery_errors":errors,"inaccessible_processes":scanner.inaccessible,"recovery_pending":engine.state.is_some()});
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
            }
            // Timed receive keeps Quit responsive without a polling wakeup loop.
            match commands.recv_timeout(Duration::from_secs_f64(config.poll_seconds)) {
                Ok(Action::Quit) => break,
                Ok(action) => match action {
                    Action::Refresh => {
                        next_inventory = 0.;
                        force_requested = true;
                    }
                    Action::Pause => {
                        if config.mode == "active" {
                            engine.manual_pause = !engine.manual_pause;
                        }
                    }
                    Action::Disable => engine.disabled = !engine.disabled,
                    Action::Restore => {
                        engine.manual_pause = false;
                        if !gaming && config.mode == "active" {
                            let r = engine.restore(&mut || {
                                scanner.scan(&games).map_or(true, |a| !a.is_empty())
                            });
                            engine.attempt(r, start.elapsed().as_secs_f64());
                        }
                    }
                    Action::Quit => break,
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if duration == 0. {
                        break;
                    }
                }
            }
        }
        Ok(())
    })();
    drop(request_tx);
    let _ = inventory_worker.join();
    result
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
