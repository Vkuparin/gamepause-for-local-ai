use crate::{
    commands::{Commands, Outcome, Waiting},
    config::{self, Config, write_json},
    control::{Activity, ControlState, CoreCommand},
    discovery::{Discovery, Game},
    engine::Engine,
    gameplay::{GameEvidence, RestoreFeedback, RestoreOffer, selected_exclusions},
    lmstudio::{Backend, LMStudio},
    processes::{ActiveGame, RunningApp, Scanner},
    tray,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
#[cfg(test)]
use std::time::SystemTime;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub enum Action {
    Tracked {
        id: u64,
        action: Box<Action>,
    },
    RemoveCustom {
        path: String,
        name: String,
    },
    Pause,
    /// Answer to an "ask" game: pause for it as for any other game, and
    /// restore normally when it exits.
    PauseForGame,
    Resume,
    Restore,
    ConfirmedRestore {
        offer_id: u64,
        ignored: Vec<String>,
    },
    RetryGameplayRestore,
    Disable,
    Refresh,
    Verify,
    Doctor,
    PowerChanged,
    Quit,
    Settings(Box<Config>),
    AdvancedSettings(Box<Config>),
    AdvancedVisibility(bool),
    NotificationPreferences {
        visual: bool,
        sound: bool,
    },
    Appearance(crate::config::Appearance),
}
impl Action {
    /// The user chose to pause for the running "ask" games.
    fn answers_ask(&self) -> bool {
        match self {
            Action::Tracked { action, .. } => action.answers_ask(),
            action => matches!(action, Action::PauseForGame),
        }
    }
    fn changes_settings(&self) -> bool {
        matches!(
            self,
            Action::Settings(_)
                | Action::AdvancedSettings(_)
                | Action::AdvancedVisibility(_)
                | Action::NotificationPreferences { .. }
                | Action::Appearance(_)
                | Action::Disable
                | Action::RemoveCustom { .. }
        ) || matches!(self, Action::ConfirmedRestore { ignored, .. } if !ignored.is_empty())
    }
}
#[derive(Clone, Default)]
pub struct Shared {
    pub commands: Commands,
    pub restore_offer: Option<RestoreOffer>,
    pub coexistence: bool,
    pub restore_feedback: Option<RestoreFeedback>,
    pub activity: Activity,
    pub detection_ok: bool,
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
    /// P2-1: the latest round-trip verify report, for the dashboard/CLI to
    /// render. `None` until a verify has been run.
    pub verify_report: Option<crate::engine::VerifyReport>,
    pub verifying: bool,
    pub pause_completions: u64,
    pub restore_completions: u64,
    pub provider_statuses: Vec<crate::coordinator::Report>,
    pub doctor_report: Option<Value>,
    pub doctor_pending: bool,
    /// LM Studio is enabled but its CLI was not found; shown as not installed.
    pub lm_missing: bool,
    /// LM Studio and Ollama processes seen by the last accepted scan. The
    /// services are not contacted while idle, so this is all "running" means.
    pub lm_running: bool,
    pub ollama_running: bool,
    /// Ollama's program file was found on this PC, running or not.
    pub ollama_installed: bool,
    /// Approximate model memory released by the current pause; 0 if unknown.
    pub freed_bytes: u64,
    /// Running games whose rule is "ask" and that have not been answered.
    pub ask_prompt: Vec<String>,
    /// Executable path of a program that looks like an unregistered game.
    pub suggestion: Option<String>,
    pub power: Arc<crate::power::Signal>,
}

pub fn local_result(state: &SharedState, outcome: Outcome, message: impl Into<String>) {
    if let Ok(mut shared) = state.lock() {
        shared.settings_error.clear();
        shared.commands.local(outcome, message);
    }
}

pub fn request_action(state: &SharedState, tx: &mpsc::Sender<Action>, action: Action, label: &str) {
    if let Ok(mut shared) = state.lock() {
        let settings = action.changes_settings();
        let doctor = matches!(action, Action::Doctor);
        if doctor {
            if !shared.config.advanced_settings_visible {
                shared.commands.local(
                    Outcome::Failed,
                    "Advanced settings is hidden; show it before using diagnostics.",
                );
                return;
            }
            if shared.doctor_pending {
                shared.commands.local(
                    Outcome::NoChange,
                    "Read-only diagnostics are already running.",
                );
                return;
            }
            shared.doctor_pending = true;
        }
        if settings && shared.commands.settings_pending {
            shared.commands.local(
                Outcome::NoChange,
                "A settings change is still being saved. Wait before changing another preference.",
            );
            return;
        }
        shared.settings_error.clear();
        let refresh = matches!(action, Action::Refresh);
        let id = if refresh {
            let Some(id) = shared.commands.request_refresh() else {
                return;
            };
            id
        } else {
            shared.commands.begin(format!("{label} requested."))
        };
        shared.commands.settings_pending |= settings;
        if tx
            .send(Action::Tracked {
                id,
                action: Box::new(action),
            })
            .is_err()
        {
            shared.doctor_pending &= !doctor;
            shared.commands.settings_pending &= !settings;
            if refresh {
                shared
                    .commands
                    .refresh_failed(id, "The discovery worker is unavailable.".into());
            } else {
                shared
                    .commands
                    .update(id, Outcome::Failed, "The control worker is unavailable.");
            }
        }
    }
}
pub type SharedState = Arc<Mutex<Shared>>;
pub fn request_quit(state: &SharedState, tx: &mpsc::Sender<Action>) {
    if tx.send(Action::Quit).is_err() {
        local_result(
            state,
            Outcome::Failed,
            "The control worker is unavailable; Quit could not be delivered.",
        );
    }
}
impl Shared {
    pub fn provider_pending(&self, kind: crate::provider::Kind) -> bool {
        (self.pending && self.provider_statuses.is_empty())
            || self
                .provider_statuses
                .iter()
                .any(|report| report.kind == kind && report.pending)
    }
    pub fn controls(&self) -> ControlState {
        ControlState {
            activity: if self.verifying {
                Activity::Verifying
            } else {
                self.activity
            },
            active_mode: self.active_mode,
            provider_enabled: self.config.any_provider_enabled(),
            detection_ready: self.discovery_ready
                && self.detection_ok
                && !self.disabled
                && self.discovery_errors.is_empty(),
            gaming: !self.active_games.is_empty(),
            pending: self.pending,
            manual_hold: self.manual_pause,
            gameplay_restore: self.restore_offer.is_some() || self.coexistence,
            coexistence: self.coexistence,
        }
    }
}

pub fn request_core(state: &SharedState, tx: &mpsc::Sender<Action>, command: CoreCommand) {
    if let Ok(mut shared) = state.lock() {
        let availability = shared.controls().availability();
        if !availability.allows(command) {
            shared.settings_error.clear();
            shared.commands.local(Outcome::Failed, availability.reason);
            return;
        }
        let action = match command {
            CoreCommand::Pause => Action::Pause,
            CoreCommand::Resume => Action::Resume,
            CoreCommand::Restore => Action::Restore,
        };
        drop(shared);
        request_action(
            state,
            tx,
            action,
            match command {
                CoreCommand::Pause => "Pause AI",
                CoreCommand::Resume => "Resume AI",
                CoreCommand::Restore => "Resume AI",
            },
        );
    }
}
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
    verify: bool,
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
            "--verify" => options.verify = true,
            "--duration" => {
                options.duration = args.next().context("--duration needs seconds")?.parse()?
            }
            "--version" => options.version = true,
            "--help" | "-h" => options.help = true,
            _ => bail!("Unknown argument {arg}"),
        }
    }
    if [
        options.doctor,
        options.discover,
        options.restore,
        options.verify,
    ]
    .into_iter()
    .filter(|v| *v)
    .count()
        > 1
    {
        bail!("Choose only one of doctor/discover/restore/verify");
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
            "GamePause for Local AI: pauses local AI for games\n--background --data-dir PATH --restore\nDevelopment and diagnostics: --headless --active --observe --duration SECONDS --doctor --discover --verify\nDefault: automatic pausing with a native dashboard. --background starts in the tray. --observe is a diagnostic override. --verify unloads/reloads the models LM Studio and Ollama have loaded, using durable recovery; close games and the GUI first. --doctor never starts/stops the server or unloads models. --restore retries a pending restore when GamePause is not running and reports the result. Ollama is used when it is running and ignored when it is not; turn it off in Advanced settings or its enabled config field. Tested with Ollama 0.35.1 and 0.40.0. Local GGUF completion models are restored with their identity, context and the keep-alive time left at pause; other local models are unloaded without reload; full load options and conversations are not preserved."
        );
        return Ok(());
    }
    let folder = args.folder.unwrap_or_else(config::data_directory);
    if args.doctor {
        let _ = fs::create_dir_all(&folder);
        let config_result = (|| -> Result<Config> {
            if folder.exists() && !folder.is_dir() {
                bail!("The data directory path is not a folder; no settings were read");
            }
            let value: Config = match fs::read_to_string(folder.join("config.json")) {
                Ok(text) => Config::parse(&text)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
                Err(e) => return Err(e.into()),
            };
            value.validate()?;
            Ok(value)
        })();
        let mut report = match config_result {
            Ok(config) => crate::diagnostics::doctor(&config, &folder),
            Err(error) => crate::diagnostics::configuration_error(&folder, format!("{error:#}")),
        };
        if let Err(e) = write_json(&folder.join("doctor-report.json"), &report) {
            report["report_write_error"] = json!(format!("{e:#}"));
        }
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    fs::create_dir_all(&folder)?;
    let folder = fs::canonicalize(folder)?;
    install_panic_hook(&folder);
    let _lock = match config::lock(&folder) {
        Ok(lock) => lock,
        Err(e) => {
            if !args.headless
                && !args.discover
                && !args.restore
                && !args.verify
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
    let backend = OptionalBackend::new(config.clone(), Some(folder.clone()));
    let mut engine = Engine::new(config.clone(), backend, folder.join("state.json"))?;
    if args.restore || args.verify {
        if config.mode != "active" {
            bail!("Observe mode cannot restore or verify models");
        }
        let power = Arc::new(crate::power::Signal::default());
        let (wake, _power_events) = mpsc::channel();
        let power_registration = crate::power::Registration::headless(power.clone(), wake)?;
        engine.observe_power(move || !power.permits(0));
        let mut discovery = Discovery::default();
        let games = discovery.recover_refresh(&config, 0., true, false).0;
        if !discovery.errors.is_empty() {
            bail!(
                "Resolve discovery errors before restoring or verifying: {:?}",
                discovery.errors
            );
        }
        let guard_games = recovery_games(&engine, &games);
        let mut scanner = recovery_scanner(&config)?;
        if scan_evidence(&mut scanner, &config, &guard_games)
            .is_none_or(|evidence| evidence.gaming())
        {
            bail!(
                "Close detected games and resolve unknown detection before restoring or verifying AI"
            );
        }
        let mut cancelled = || {
            scan_evidence(&mut scanner, &config, &guard_games)
                .is_none_or(|evidence| evidence.gaming())
        };
        if args.verify {
            let report = engine.verify_round_trip(&mut cancelled);
            println!("{}", serde_json::to_string_pretty(&report)?);
            write_json(&folder.join("verify-report.json"), &report)?;
            if !report.ok {
                bail!("{}", report.summary);
            }
        } else {
            engine.restore(&mut cancelled)?;
            if engine.pending() {
                bail!("Restoration deferred; recovery pending");
            }
            println!("{}", engine.message);
            // The windowed program has no console to print to.
            if !console {
                tray::info(&engine.message);
            }
        }
        power_registration.close()?;
        return Ok(());
    }
    let state = Arc::new(Mutex::new(Shared {
        commands: Commands::default(),
        activity: engine.activity,
        restore_offer: None,
        coexistence: false,
        restore_feedback: None,
        detection_ok: false,
        message: "Starting GamePause".into(),
        disabled: false,
        manual_pause: false,
        pending: engine.pending(),
        active_mode: config.mode == "active",
        config: config.clone(),
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
        ollama_installed: ollama_installed(),
        freed_bytes: 0,
        ask_prompt: vec![],
        suggestion: None,
        power: Default::default(),
    }));
    let (tx, rx) = mpsc::channel();
    if args.headless {
        let power_signal = state
            .lock()
            .map_err(|_| anyhow::anyhow!("Shared state unavailable"))?
            .power
            .clone();
        let power_registration = crate::power::Registration::headless(power_signal, tx)?;
        let result = run(engine, folder, state, rx, args.duration, console);
        let cleanup = power_registration.close();
        result?;
        cleanup?;
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
    claims: crate::ownership::SharedClaims,
    config: Config,
    folder: Option<PathBuf>,
    /// When the CLI was last looked for and not found, and why.
    missing: Option<(Instant, String)>,
}
/// How long a failed search for the LM Studio CLI stands before the disks are
/// asked again. The watcher asks whether it is installed on every tick.
const LMS_SEARCH_INTERVAL: Duration = Duration::from_secs(30);
impl OptionalBackend {
    fn new(config: Config, folder: Option<PathBuf>) -> Self {
        Self {
            backend: None,
            claims: Default::default(),
            config,
            folder,
            missing: None,
        }
    }
    /// Forget the transport and any failed search, after a settings change.
    fn reset(&mut self, config: Config) {
        self.config = config;
        self.backend = None;
        self.missing = None;
    }
    fn get(&mut self) -> Result<&mut LMStudio> {
        if self.backend.is_none() {
            if let Some((at, error)) = &self.missing
                && at.elapsed() < LMS_SEARCH_INTERVAL
            {
                bail!("{error}");
            }
            match LMStudio::new(self.config.clone()) {
                Ok(backend) => {
                    self.missing = None;
                    self.backend = Some(backend);
                }
                Err(error) => {
                    self.missing = Some((Instant::now(), format!("{error:#}")));
                    return Err(error);
                }
            }
        }
        let backend = self.backend.as_mut().unwrap();
        backend.use_claims(self.claims.clone());
        if let Some(folder) = &self.folder {
            backend.set_log_folder(folder.clone());
        }
        Ok(backend)
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
    fn ensure_server(&mut self, p: u16) -> Result<()> {
        self.get()?.ensure_server(p)
    }
    fn unload(&mut self, id: &str) -> Result<()> {
        self.get()?.unload(id)
    }
    fn restore(&mut self, m: &crate::lmstudio::Model) -> Result<()> {
        self.get()?.restore(m)
    }
    fn read_config(&mut self, m: &crate::lmstudio::Model) -> Result<Value> {
        self.get()?.read_config(m)
    }
    fn server_state(&mut self) -> Result<Value> {
        self.get()?.server_state()
    }
    fn verify_restored(&mut self, m: &crate::lmstudio::Model) -> Result<()> {
        self.get()?.verify_restored(m)
    }
    fn select_control_port(&mut self, port: u16) -> Result<()> {
        self.get()?.select_control_port(port)
    }
    fn installed(&mut self) -> bool {
        self.get().is_ok()
    }
    fn captured_bytes(&mut self) -> u64 {
        self.backend
            .as_mut()
            .map_or(0, |backend| backend.captured_bytes())
    }
}
#[cfg(test)]
fn apply_action(
    action: Action,
    engine: &mut Engine<OptionalBackend>,
    folder: &std::path::Path,
    scanner: &mut Scanner,
    games: &[Game],
    now: f64,
    state: &SharedState,
) -> Result<bool> {
    apply_action_detected(action, engine, folder, scanner, games, now, state, None)
}
#[allow(clippy::too_many_arguments)]
fn apply_action_detected(
    action: Action,
    engine: &mut Engine<OptionalBackend>,
    folder: &std::path::Path,
    scanner: &mut Scanner,
    games: &[Game],
    now: f64,
    state: &SharedState,
    detection: Option<&BackgroundDetection>,
) -> Result<bool> {
    if let Action::Tracked { id, action } = action {
        let action = *action;
        let settings = action.changes_settings();
        let refresh = matches!(action, Action::Refresh);
        let before = engine.config.clone();
        let waiting = match action {
            Action::Pause => Some(Waiting::Pause),
            Action::Restore | Action::ConfirmedRestore { .. } | Action::RetryGameplayRestore => {
                Some(Waiting::Restore)
            }
            _ => None,
        };
        let success = match &action {
            Action::RemoveCustom { name, .. } => format!("Removed custom game \"{name}\"; saved."),
            Action::Settings(_)
            | Action::AdvancedSettings(_)
            | Action::AdvancedVisibility(_)
            | Action::NotificationPreferences { .. }
            | Action::Appearance(_)
            | Action::Disable => "Settings saved.".into(),
            Action::Resume => {
                "Manual hold released; normal recovery guards and delay apply.".into()
            }
            Action::Doctor => {
                "Read-only diagnostics finished; see Advanced provider details.".into()
            }
            Action::PauseForGame => "Pausing AI for this game.".into(),
            _ => "Command completed.".into(),
        };
        let gameplay = matches!(
            action,
            Action::ConfirmedRestore { .. } | Action::RetryGameplayRestore
        );
        let verify = matches!(action, Action::Verify);
        let doctor = matches!(action, Action::Doctor);
        if let Ok(mut shared) = state.lock() {
            shared.settings_error.clear();
            shared
                .commands
                .update(id, Outcome::Working, "Processing request...");
        }
        let result = apply_action_detected(
            action, engine, folder, scanner, games, now, state, detection,
        );
        if let Ok(mut shared) = state.lock() {
            if doctor {
                shared.doctor_pending = false;
            }
            if settings {
                shared.commands.settings_pending = false;
            }
            if verify && result.is_err() {
                shared.verifying = false;
            }
            let failure = result
                .as_ref()
                .err()
                .map(|e| format!("Command failed: {e:#}"))
                .or_else(|| {
                    (!shared.settings_error.is_empty()).then(|| shared.settings_error.clone())
                });
            if let Some(failure) = failure {
                shared.commands.update(id, Outcome::Failed, failure);
                shared.settings_error.clear();
            } else if refresh {
                // run() binds this request to the next dispatch generation.
            } else if gameplay {
                let text = shared
                    .restore_feedback
                    .as_ref()
                    .map(RestoreFeedback::text)
                    .unwrap_or_default();
                let failed = !engine.last_error.is_empty()
                    || shared
                        .restore_feedback
                        .as_ref()
                        .is_some_and(|feedback| feedback.preference_failure)
                    || !shared
                        .restore_feedback
                        .as_ref()
                        .is_some_and(|feedback| feedback.accepted);
                shared.commands.update(
                    id,
                    if failed {
                        Outcome::Failed
                    } else if engine.pending() {
                        Outcome::Working
                    } else {
                        Outcome::Completed
                    },
                    text,
                );
                if !failed && engine.pending() {
                    let exclusions = shared
                        .restore_feedback
                        .as_ref()
                        .map(|f| f.exclusions.clone())
                        .unwrap_or_default();
                    shared.commands.waiting = Some((id, Waiting::GameplayRestore(exclusions)));
                }
            } else if verify {
                if let Some(report) = &shared.verify_report {
                    let summary = crate::dashboard::render_verify_report(report);
                    let outcome = if report.ok {
                        Outcome::Completed
                    } else {
                        Outcome::Failed
                    };
                    shared.commands.update(id, outcome, summary);
                }
            } else if let Some(waiting) = waiting {
                shared.commands.waiting = Some((id, waiting));
            } else {
                let unchanged = settings && before == engine.config;
                shared.commands.update(
                    id,
                    if unchanged {
                        Outcome::NoChange
                    } else {
                        Outcome::Completed
                    },
                    if unchanged {
                        "Settings unchanged.".into()
                    } else {
                        success
                    },
                );
            }
        }
        return result;
    }
    let core = match action {
        Action::Pause => Some(CoreCommand::Pause),
        Action::Resume => Some(CoreCommand::Resume),
        Action::Restore => Some(CoreCommand::Restore),
        _ => None,
    };
    if let Some(command) = core {
        // Revalidate against engine evidence and a fresh, exclusion-free scan.
        // A queued UI click cannot create/release a hold by toggling stale state.
        let ready = state
            .lock()
            .map(|s| s.discovery_ready && s.detection_ok && s.discovery_errors.is_empty())
            .unwrap_or(false);
        let mut guard = recovery_scanner(&engine.config)?;
        let current = action_evidence(
            detection,
            &mut guard,
            &engine.config,
            &recovery_games(engine, games),
            ready,
        );
        let controls = ControlState {
            activity: engine.activity,
            active_mode: engine.config.mode == "active",
            provider_enabled: engine.config.any_provider_enabled(),
            detection_ready: ready && !engine.disabled && current.is_some(),
            gaming: current.as_ref().is_some_and(GameEvidence::gaming),
            pending: engine.pending(),
            manual_hold: engine.manual_pause,
            gameplay_restore: false,
            coexistence: engine.gameplay.active(),
        };
        let availability = controls.availability();
        if !availability.allows(command) {
            if let Ok(mut shared) = state.lock() {
                shared.settings_error = availability.reason.into();
            }
            return Ok(false);
        }
    }
    match action {
        Action::Tracked { .. } => unreachable!(),
        // Consumed by the watcher loop, which owns the answer's lifetime.
        Action::PauseForGame => (),
        Action::RemoveCustom { path, name } => {
            let updated = remove_custom(&engine.config, &path, &name)?;
            return apply_action_detected(
                Action::Settings(Box::new(updated)),
                engine,
                folder,
                scanner,
                games,
                now,
                state,
                detection,
            );
        }
        Action::AdvancedSettings(updated) => {
            if !engine.config.advanced_settings_visible {
                bail!("Advanced settings is hidden; stale settings command refused");
            }
            return apply_action_detected(
                Action::Settings(updated),
                engine,
                folder,
                scanner,
                games,
                now,
                state,
                detection,
            );
        }
        Action::AdvancedVisibility(visible) => {
            let mut updated = engine.config.clone();
            updated.advanced_settings_visible = visible;
            return apply_action_detected(
                Action::Settings(Box::new(updated)),
                engine,
                folder,
                scanner,
                games,
                now,
                state,
                detection,
            );
        }
        Action::Appearance(appearance) => {
            if !engine.config.advanced_settings_visible {
                bail!("Advanced settings is hidden; stale appearance preference refused");
            }
            let mut updated = engine.config.clone();
            updated.appearance = appearance;
            write_json(&folder.join("config.json"), &updated)?;
            engine.config.appearance = appearance;
            engine.backend.config.appearance = appearance;
            if let Ok(mut shared) = state.lock() {
                shared.config.appearance = appearance;
                shared.settings_error.clear();
                shared.revision += 1;
            }
        }
        Action::NotificationPreferences { visual, sound } => {
            if !engine.config.advanced_settings_visible {
                bail!("Advanced settings is hidden; stale notification preference refused");
            }
            let mut updated = engine.config.clone();
            updated.notifications_enabled = visual;
            updated.sound_enabled = sound;
            return apply_action_detected(
                Action::Settings(Box::new(updated)),
                engine,
                folder,
                scanner,
                games,
                now,
                state,
                detection,
            );
        }
        Action::Settings(updated) => {
            engine.gameplay.invalidate_offer();
            let result = (|| -> Result<()> {
                updated.validate()?;
                if engine.pending() {
                    engine.validate_recovery_edit(&updated)?;
                }
                let replacement = Scanner::new((*updated).clone())?;
                write_json(&folder.join("config.json"), updated.as_ref())?;
                if !engine.provider_pending(crate::provider::Kind::LMStudio)
                    && updated.providers != engine.config.providers
                {
                    engine.backend.claims = Default::default();
                }
                engine.backend.reset((*updated).clone());
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
            return apply_action_detected(
                Action::Settings(Box::new(updated)),
                engine,
                folder,
                scanner,
                games,
                now,
                state,
                detection,
            );
        }
        Action::Pause => {
            engine.remember_games(engine.gameplay.remembered_games())?;
            engine.gameplay.revoke();
            engine.request_manual_pause();
        }
        Action::Resume => {
            engine.manual_pause = false;
        }
        Action::Restore => {
            if engine.config.mode == "observe" {
                return Ok(false);
            }
            if !state.lock().map(|s| s.discovery_ready).unwrap_or(false) {
                return Ok(false);
            }
            engine.manual_pause = false;
            let mut guard = recovery_scanner(&engine.config)?;
            let guard_games = recovery_games(engine, games);
            let config = engine.config.clone();
            let result = engine.restore(&mut || {
                action_evidence(detection, &mut guard, &config, &guard_games, true)
                    .is_none_or(|evidence| evidence.gaming())
            });
            engine.restore_failed = true;
            engine.attempt(result, now);
        }
        Action::ConfirmedRestore { offer_id, ignored } => {
            let ready = state
                .lock()
                .map(|s| s.discovery_ready && s.detection_ok && s.discovery_errors.is_empty())
                .unwrap_or(false);
            let known = recovery_games(engine, games);
            let mut guard = recovery_scanner(&engine.config)?;
            let feedback = confirmed_restore(
                engine,
                offer_id,
                &ignored,
                now,
                ready,
                &mut |config| action_evidence(detection, &mut guard, config, &known, ready),
                &mut |config| {
                    write_json(&folder.join("config.json"), config)?;
                    if let Ok(mut shared) = state.lock() {
                        shared.config = config.clone();
                        shared.revision += 1;
                    }
                    Ok(())
                },
            );
            engine.backend.config = engine.config.clone();
            *scanner = Scanner::new(engine.config.clone())?;
            if let Ok(mut shared) = state.lock() {
                shared.restore_feedback = Some(feedback);
                shared.restore_offer = engine.gameplay.offer();
                shared.coexistence = engine.gameplay.active();
                shared.config = engine.config.clone();
                shared.revision += 1;
            }
        }
        Action::RetryGameplayRestore => {
            let ready = state
                .lock()
                .map(|s| s.discovery_ready && s.detection_ok && s.discovery_errors.is_empty())
                .unwrap_or(false);
            if !ready || !engine.pending() || !engine.gameplay.active() || engine.activity.busy() {
                if !ready {
                    engine.gameplay.revoke();
                }
                if let Ok(mut shared) = state.lock() {
                    let feedback = shared.restore_feedback.get_or_insert_with(Default::default);
                    feedback.accepted = false;
                    feedback.restoration =
                        "Gameplay retry is unavailable; no model operation performed.".into();
                }
                return Ok(false);
            }
            let known = recovery_games(engine, games);
            let mut guard = recovery_scanner(&engine.config)?;
            let config = engine.config.clone();
            let result = engine.restore_gameplay(&mut || {
                action_evidence(detection, &mut guard, &config, &known, ready)
            });
            let outcome = restore_outcome(engine, &result);
            engine.restore_failed = true;
            engine.attempt(result, now);
            if let Ok(mut shared) = state.lock() {
                let feedback = shared.restore_feedback.get_or_insert_with(Default::default);
                feedback.accepted = true;
                feedback.restoration = outcome;
            }
        }
        Action::Doctor => {
            if !engine.config.advanced_settings_visible {
                bail!("Advanced settings is hidden; stale diagnostics command refused");
            }
            let report = crate::diagnostics::doctor(&engine.config, folder);
            if let Ok(mut shared) = state.lock() {
                shared.doctor_report = Some(report);
                shared.revision += 1;
            }
        }
        Action::Verify => {
            if !engine.config.advanced_settings_visible {
                bail!("Advanced settings is hidden; stale test command refused");
            }
            let ready = state
                .lock()
                .map(|s| s.discovery_ready && s.discovery_errors.is_empty())
                .unwrap_or(false);
            let guard_games = recovery_games(engine, games);
            let mut guard = recovery_scanner(&engine.config)?;
            let config = engine.config.clone();
            let report = engine.verify_round_trip(&mut || {
                !ready
                    || action_evidence(detection, &mut guard, &config, &guard_games, ready)
                        .is_none_or(|evidence| evidence.gaming())
            });
            if let Ok(mut shared) = state.lock() {
                shared.verifying = false;
                shared.pending = engine.pending();
                shared.verify_report = Some(report.clone());
                shared.message = report.summary.clone();
                shared.revision += 1;
            }
            return Ok(true);
        }
        Action::Refresh => return Ok(true),
        Action::Quit | Action::PowerChanged => (),
    }
    Ok(false)
}
pub fn request_verify(state: &SharedState, tx: &mpsc::Sender<Action>) {
    if let Ok(mut shared) = state.lock() {
        if shared.verifying
            || !shared.config.advanced_settings_visible
            || !shared.active_mode
            || !shared.config.any_provider_enabled()
            || shared.disabled
            || shared.pending
            || shared.manual_pause
            || !shared.discovery_ready
            || !shared.detection_ok
            || shared.activity.busy()
            || !shared.active_games.is_empty()
            || !shared.discovery_errors.is_empty()
        {
            shared.settings_error.clear();
            shared.commands.local(Outcome::Failed, "Round-trip unavailable: finish recovery, close games, enable active detection, and wait for discovery.");
            return;
        }
        shared.settings_error.clear();
        shared.verifying = true;
        let id = shared.commands.begin("Test round-trip requested.");
        if tx
            .send(Action::Tracked {
                id,
                action: Box::new(Action::Verify),
            })
            .is_err()
        {
            shared.verifying = false;
            shared
                .commands
                .update(id, Outcome::Failed, "The control worker is unavailable.");
        }
    }
}
pub fn remove_custom(config: &Config, path: &str, name: &str) -> Result<Config> {
    let index = config
        .extra_games
        .iter()
        .position(|game| crate::discovery::same_path(&game.path, path) && game.name == name)
        .context("Selected custom game changed or was removed; select it again")?;
    let mut updated = config.clone();
    updated.extra_games.remove(index);
    updated.validate()?;
    Ok(updated)
}
fn evidence_from(
    all: Vec<ActiveGame>,
    trigger_scanner: &Scanner,
    config: &Config,
    ask_approved: bool,
) -> GameEvidence {
    let triggers = all
        .iter()
        .filter(|game| {
            let name = game.executable.rsplit(['\\', '/']).next().unwrap_or("");
            !trigger_scanner.excluded(name, &game.executable)
                && !config
                    .ignored_games
                    .iter()
                    .any(|path| crate::discovery::same_path(path, &game.path))
                && (ask_approved || !config.asks(&game.path))
        })
        .cloned()
        .collect();
    GameEvidence { all, triggers }
}
fn scan_evidence(guard: &mut Scanner, config: &Config, games: &[Game]) -> Option<GameEvidence> {
    let all = guard.scan(games).ok()?;
    if guard.uncertain_games {
        return None;
    }
    let trigger_scanner = Scanner::new(config.clone()).ok()?;
    let evidence = evidence_from(all, &trigger_scanner, config, false);
    evidence.reliable().then_some(evidence)
}
fn restore_outcome<B: Backend>(engine: &Engine<B>, result: &Result<()>) -> String {
    if let Err(error) = result {
        format!("Restore failed: {error:#}. Recovery retained.")
    } else if engine.pending() {
        "Restore interrupted or deferred; recovery retained.".into()
    } else {
        "AI restoration completed and verified.".into()
    }
}
pub(crate) fn confirmed_restore<B: Backend>(
    engine: &mut Engine<B>,
    offer_id: u64,
    ignored: &[String],
    now: f64,
    ready: bool,
    scan: &mut dyn FnMut(&Config) -> Option<GameEvidence>,
    save: &mut dyn FnMut(&Config) -> Result<()>,
) -> RestoreFeedback {
    let mut feedback = RestoreFeedback {
        accepted: false,
        preference_failure: false,
        exclusions: "Ignore preferences unchanged.".into(),
        restoration: String::new(),
        tracking_recovery: false,
    };
    let accepted = (|| -> Result<Vec<ActiveGame>> {
        if !ready
            || engine.disabled
            || engine.config.mode != "active"
            || engine.activity.busy()
            || !engine.pending()
        {
            if !ready {
                engine.gameplay.revoke();
            }
            bail!(
                "Restore confirmation is unavailable; wait for successful detection and pending recovery"
            );
        }
        let Some(current) = scan(&engine.config).filter(GameEvidence::reliable) else {
            engine.gameplay.revoke();
            bail!("Game detection is unknown; gameplay Restore refused");
        };
        let offer = engine
            .gameplay
            .offer()
            .context("Restore confirmation expired")?;
        selected_exclusions(&engine.config, &offer.games, ignored)?;
        engine.gameplay.confirm(offer_id, &current)
    })();
    let approved = match accepted {
        Ok(games) => games,
        Err(error) => {
            feedback.restoration = format!("Restore refused: {error:#}");
            return feedback;
        }
    };
    feedback.accepted = true;
    if !ignored.is_empty() {
        let saved = selected_exclusions(&engine.config, &approved, ignored).and_then(|updated| {
            save(&updated)?;
            engine.config = updated;
            Ok(())
        });
        feedback.exclusions = match saved {
            Ok(()) => "Selected Ignore in future preferences saved.".into(),
            Err(error) => {
                feedback.preference_failure = true;
                format!("Ignore preferences could not be saved: {error:#}")
            }
        };
    }
    let config = engine.config.clone();
    let result = engine.restore_gameplay(&mut || scan(&config));
    feedback.restoration = restore_outcome(engine, &result);
    feedback.tracking_recovery = engine.pending();
    engine.restore_failed = true;
    engine.attempt(result, now);
    feedback
}
/// Whether Ollama's program file is in its per-user install folder or on
/// `PATH`. Looked up with each inventory refresh, never on the scan path, and
/// used only to decide whether the status card mentions Ollama.
fn ollama_installed() -> bool {
    std::env::var_os("LOCALAPPDATA")
        .map(|root| PathBuf::from(root).join(r"Programs\Ollama"))
        .into_iter()
        .chain(
            std::env::var_os("PATH")
                .iter()
                .flat_map(std::env::split_paths)
                .collect::<Vec<_>>(),
        )
        .any(|folder| folder.join("ollama.exe").is_file())
}
fn recovery_scanner(config: &Config) -> Result<Scanner> {
    let mut guard = config.clone();
    guard.excluded_paths.clear();
    guard.excluded_executables.clear();
    Scanner::new(guard)
}
fn recovery_games(engine: &Engine<OptionalBackend>, games: &[Game]) -> Vec<Game> {
    let mut known = games.to_vec();
    let approved = engine.gameplay.remembered_games();
    for game in engine.remembered_games.iter().chain(approved.iter()) {
        if !known
            .iter()
            .any(|g| crate::discovery::same_path(&g.path, &game.path))
        {
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
        self.failures = self.failures.saturating_add(1);
        log(
            folder,
            &format!("Transient failure ({} consecutive): {err}", self.failures),
        );
        if self.failures >= Self::NEEDS_ATTENTION_AFTER {
            self.needs_attention = true;
            if let Ok(mut shared) = state.lock() {
                shared.message = format!("Needs attention — repeated monitoring failures: {err}");
            }
        }
    }
    fn reset(&mut self) {
        self.failures = 0;
    }
}
#[derive(Clone)]
struct DetectionInput {
    power: Arc<crate::power::Signal>,
    power_generation: u64,
    config: Config,
    games: Vec<Game>,
    guard_games: Vec<Game>,
    steam_roots: Vec<String>,
    guarded: bool,
    ready: bool,
    /// The user answered the current "ask" games with Pause.
    ask_approved: bool,
}
#[derive(Clone)]
struct DetectionFrame {
    power_generation: u64,
    config: Config,
    /// Running "ask" games, answered or not.
    asking: Vec<String>,
    /// A fullscreen program nothing recognises, once it has stayed in front.
    suggestion: Option<String>,
    active: Vec<ActiveGame>,
    all: Vec<ActiveGame>,
    evidence: Option<GameEvidence>,
    inaccessible: usize,
    candidate: u64,
    lm_running: bool,
    ollama_running: bool,
    running_apps: Vec<RunningApp>,
}
/// Scans a fullscreen program must lead before it is suggested.
const SUGGEST_AFTER_SCANS: u32 = 5;
struct NativeDetection {
    power_generation: u64,
    config: Config,
    scanner: Scanner,
    guard: Scanner,
    candidate: u64,
    /// Unrecognised fullscreen executable and how many scans it has led.
    fullscreen: (String, u32),
}
impl NativeDetection {
    fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            power_generation: 0,
            config: config.clone(),
            scanner: Scanner::new(config.clone())?,
            guard: recovery_scanner(config)?,
            candidate: 0,
            fullscreen: (String::new(), 0),
        })
    }
    fn scan(&mut self, input: &DetectionInput) -> Result<DetectionFrame> {
        if !input.power.permits(input.power_generation) {
            bail!("Power state changed; fresh post-resume detection required");
        }
        if input.config != self.config || self.power_generation != input.power_generation {
            let scanner = Scanner::new(input.config.clone())?;
            let guard = recovery_scanner(&input.config)?;
            self.scanner = scanner;
            self.guard = guard;
            self.config = input.config.clone();
            self.power_generation = input.power_generation;
        }
        let scanned = self.scanner.scan(&input.games)?;
        if self
            .scanner
            .new_game_candidate(&input.games, &input.steam_roots)
        {
            self.candidate = self.candidate.saturating_add(1);
        }
        // Suggest only what stays fullscreen and in front for several scans.
        let front = input
            .config
            .suggest_unknown_games
            .then(crate::processes::fullscreen_foreground)
            .flatten();
        match self.scanner.suggestion(&input.games, front) {
            Some(path) if path == self.fullscreen.0 => {
                self.fullscreen.1 = self.fullscreen.1.saturating_add(1)
            }
            Some(path) => self.fullscreen = (path, 1),
            None => self.fullscreen = (String::new(), 0),
        }
        let suggestion = (self.fullscreen.1 >= SUGGEST_AFTER_SCANS)
            .then(|| self.fullscreen.0.clone())
            .filter(|path| {
                !input
                    .config
                    .dismissed_suggestions
                    .iter()
                    .chain(&input.config.ignored_games)
                    .any(|known| crate::discovery::same_path(known, path))
            });
        let all = if input.guarded {
            self.guard.scan(&input.guard_games)?
        } else {
            scanned.clone()
        };
        let mut asking = scanned
            .iter()
            .filter(|game| input.config.asks(&game.path))
            .map(|game| game.game.clone())
            .collect::<Vec<_>>();
        asking.dedup();
        let active = scanned
            .into_iter()
            .filter(|game| {
                !input
                    .config
                    .ignored_games
                    .iter()
                    .any(|path| crate::discovery::same_path(path, &game.path))
                    && (input.ask_approved || !input.config.asks(&game.path))
            })
            .collect();
        let evidence = (input.ready
            && !self.scanner.uncertain_games
            && (!input.guarded || !self.guard.uncertain_games))
            .then(|| {
                evidence_from(
                    all.clone(),
                    &self.scanner,
                    &input.config,
                    input.ask_approved,
                )
            })
            .filter(GameEvidence::reliable);
        if !input.power.permits(input.power_generation) {
            bail!("Power state changed during game detection");
        }
        Ok(DetectionFrame {
            power_generation: input.power_generation,
            config: input.config.clone(),
            asking,
            suggestion,
            active,
            all,
            evidence,
            inaccessible: self.scanner.inaccessible,
            candidate: self.candidate,
            lm_running: self.scanner.lmstudio_running(),
            ollama_running: self.scanner.ollama_running(),
            running_apps: if crate::dashboard::needs_running_apps() {
                self.scanner.running_apps()
            } else {
                vec![]
            },
        })
    }
}
type BackgroundDetection = crate::detection_worker::DetectionWorker<
    DetectionInput,
    std::result::Result<DetectionFrame, String>,
>;
const FRESH_DETECTION_TIMEOUT: Duration = Duration::from_secs(2);
fn publish_detection(state: &SharedState, result: &std::result::Result<DetectionFrame, String>) {
    if let Ok(mut shared) = state.lock() {
        match result {
            Ok(frame)
                if shared.power.permits(frame.power_generation)
                    && shared.config == frame.config =>
            {
                shared.detection_ok = frame.evidence.is_some();
                shared.active_games = frame.all.clone();
                shared.running_apps = frame.running_apps.clone();
                shared.lm_running = frame.lm_running;
                shared.ollama_running = frame.ollama_running;
                if !shared.detection_ok {
                    shared.restore_offer = None;
                    shared.coexistence = false;
                } else if shared.restore_offer.as_ref().is_some_and(|offer| {
                    offer.games.len() != frame.all.len()
                        || offer.games.iter().any(|game| {
                            !frame.all.iter().any(|current| {
                                current.pid == game.pid
                                    && current.created_at == game.created_at
                                    && crate::discovery::same_path(
                                        &current.executable,
                                        &game.executable,
                                    )
                            })
                        })
                }) {
                    shared.restore_offer = None;
                }
            }
            Err(_) => {
                shared.detection_ok = false;
                shared.restore_offer = None;
                shared.coexistence = false;
            }
            _ => {}
        }
    }
}
fn fresh_detection(worker: &BackgroundDetection, input: DetectionInput) -> Result<DetectionFrame> {
    let signal = input.power.clone();
    let generation = input.power_generation;
    if !signal.permits(generation) {
        bail!("Power state changed; detection held");
    }
    let frame = worker
        .fresh(input, FRESH_DETECTION_TIMEOUT)?
        .map_err(anyhow::Error::msg)?;
    if !signal.permits(generation) || frame.power_generation != generation {
        bail!("Power state changed during game detection");
    }
    Ok(frame)
}
fn action_evidence(
    worker: Option<&BackgroundDetection>,
    fallback: &mut Scanner,
    config: &Config,
    games: &[Game],
    ready: bool,
) -> Option<GameEvidence> {
    if !ready {
        return None;
    }
    if let Some(worker) = worker {
        worker
            .fresh_with(FRESH_DETECTION_TIMEOUT, |input| {
                input.config = config.clone();
                input.games = games.to_vec();
                input.guard_games = games.to_vec();
                input.guarded = true;
                input.ready = ready;
            })
            .ok()?
            .ok()?
            .evidence
    } else {
        scan_evidence(fallback, config, games)
    }
}
fn hold_for_discovery<B: Backend>(engine: &mut Engine<B>, inventory_ready: bool) {
    if !inventory_ready && engine.awaiting_resume_detection() {
        // Waiting for the requested inventory is not failed process detection.
        // Preserve the scope for later revalidation, without exposing/using it.
        engine.gameplay.invalidate_offer();
        engine.activity = Activity::DetectionUnavailable;
        engine.message = "Windows resumed; refreshing game discovery before AI control. Saved recovery and any previous gameplay choice are held for revalidation.".into();
    } else {
        engine.gameplay.revoke();
        engine.activity = if inventory_ready {
            Activity::DetectionUnavailable
        } else {
            Activity::Unknown
        };
        engine.message = if inventory_ready {
            "Game discovery has errors; AI control and recovery are held until discovery succeeds"
                .into()
        } else {
            "Discovering installed games — existing recovery is held until discovery finishes"
                .into()
        };
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
    let power = state
        .lock()
        .map_err(|_| anyhow::anyhow!("Shared state unavailable"))?
        .power
        .clone();
    let accepted_power = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let progress_state = state.clone();
    let progress_signal = power.clone();
    let progress_generation = accepted_power.clone();
    engine.observe_progress(move |activity| {
        if let Ok(mut shared) = progress_state.lock() {
            if !progress_signal
                .permits(progress_generation.load(std::sync::atomic::Ordering::Acquire))
            {
                shared.activity = Activity::DetectionUnavailable;
                shared.message =
                    "Power state changed; AI control is held for fresh detection.".into();
                return;
            }
            shared.activity = activity;
            if activity.busy() {
                shared.message = activity.progress_message().into();
            }
        }
    });
    let provider_state = state.clone();
    let provider_signal = power.clone();
    let provider_generation = accepted_power.clone();
    engine.observe_providers(move |reports| {
        if let Ok(mut shared) = provider_state.lock() {
            if provider_signal
                .permits(provider_generation.load(std::sync::atomic::Ordering::Acquire))
            {
                shared.provider_statuses = reports.to_vec();
            } else {
                shared.provider_statuses.clear();
            }
        }
    });
    let mut scanner = Scanner::new(engine.config.clone())?;
    let mut power_generation = 0;
    let guard_power = power.clone();
    let guard_generation = accepted_power.clone();
    engine.observe_power(move || {
        !guard_power.permits(guard_generation.load(std::sync::atomic::Ordering::Acquire))
    });
    let initial_detection = DetectionInput {
        power: power.clone(),
        power_generation: power.snapshot().generation,
        config: engine.config.clone(),
        games: vec![],
        guard_games: recovery_games(&engine, &[]),
        steam_roots: vec![],
        guarded: engine.pending(),
        ready: false,
        ask_approved: false,
    };
    let mut native_detection = NativeDetection::new(&engine.config)?;
    let detection_state = state.clone();
    let detection = BackgroundDetection::start(
        initial_detection,
        |input| Duration::from_secs_f64(input.config.poll_seconds),
        move |input| {
            native_detection
                .scan(input)
                .map_err(|error| format!("{error:#}"))
        },
        move |frame| match frame {
            Some(frame) => publish_detection(&detection_state, frame),
            None => publish_detection(&detection_state, &Err("Detection worker stopped".into())),
        },
    )?;
    let mut games = vec![];
    let mut errors = std::collections::BTreeMap::<String, String>::new();
    let (request_tx, request_rx) = mpsc::channel::<(u64, Config, f64, bool, bool)>();
    let (result_tx, result_rx) = mpsc::channel();
    let worker_log = folder.clone();
    let inventory_worker = std::thread::spawn(move || {
        let mut discovery = Discovery::default();
        while let Ok((generation, config, now, force, defer)) = request_rx.recv() {
            let (games, panic_payload) = discovery.recover_refresh(&config, now, force, defer);
            if !panic_payload.is_empty() {
                crate::app::log(
                    &worker_log,
                    &format!("Inventory scan recovered from panic: {panic_payload}"),
                );
            }
            if result_tx
                .send((
                    generation,
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
    let mut ask_approved = false;
    let mut generation = 0u64;
    let mut minimum_inventory_generation = 0u64;
    let mut queued = None;
    let mut inventory_ready = false;
    let mut inventory_dirty = false;
    let mut steam_roots = vec![];
    let mut last_candidate = 0;
    let mut monitor = Monitor::default();
    loop {
        let now = start.elapsed().as_secs_f64();
        if duration > 0. && now >= duration {
            break;
        }
        let power_state = power.snapshot();
        if power_state.generation != power_generation {
            power_generation = power_state.generation;
            accepted_power.store(power_generation, std::sync::atomic::Ordering::Release);
            engine.resume_detected();
            if power_state.suspended {
                engine.message =
                    "Windows is suspending; AI control held and saved recovery retained.".into();
            }
            scanner = Scanner::new(engine.config.clone())?;
            force_requested = true;
            inventory_ready = false;
            minimum_inventory_generation = generation.saturating_add(1);
            if let Ok(mut shared) = state.lock() {
                shared.detection_ok = false;
                shared.discovery_ready = false;
                shared.restore_offer = None;
                shared.coexistence = false;
                shared.activity = Activity::DetectionUnavailable;
                shared.doctor_report = None;
                shared.message = engine.message.clone();
            }
        }
        let actions = queued.take().into_iter().chain(commands.try_iter());
        let mut quit = false;
        for action in actions {
            if matches!(action, Action::PowerChanged) {
                continue;
            }
            if matches!(action, Action::Quit) {
                quit = true;
                break;
            }
            // Honoured wherever it sits in the queue, tracked or not.
            ask_approved |= action.answers_ask();
            if let Action::Tracked { id, action } = &action
                && matches!(action.as_ref(), Action::Refresh)
                && let Ok(mut shared) = state.lock()
            {
                shared.commands.bind_refresh(*id, generation + 1);
            }
            if match apply_action_detected(
                action,
                &mut engine,
                &folder,
                &mut scanner,
                &games,
                now,
                &state,
                Some(&detection),
            ) {
                Ok(refresh) => refresh,
                Err(e) => {
                    log(&folder, &format!("Action failed: {e:#}"));
                    if let Ok(mut shared) = state.lock() {
                        shared.verifying = false;
                        shared.message = format!("Needs attention: {e:#}");
                    }
                    false
                }
            } {
                force_requested = true;
            }
        }
        if quit {
            break;
        }
        if power.snapshot().suspended {
            match commands.recv_timeout(Duration::from_secs_f64(engine.config.poll_seconds)) {
                Ok(Action::Quit) => break,
                Ok(action) => queued = Some(action),
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            continue;
        }
        let tick = (|| -> Result<()> {
            let mut persistence_error = None;
            let inventory_result = result_rx.try_recv();
            if matches!(inventory_result, Err(mpsc::TryRecvError::Disconnected)) {
                if let Ok(mut shared) = state.lock() {
                    shared
                        .commands
                        .discovery_unavailable("Discovery worker stopped; refresh failed.".into());
                }
                bail!("Discovery worker stopped");
            }
            if let Ok((accepted_generation, updated, updated_errors, roots)) = inventory_result {
                inventory_pending = false;
                if accepted_generation < minimum_inventory_generation {
                    force_requested = true;
                } else {
                    let changed = games != updated;
                    let ollama = ollama_installed();
                    let was_ready = inventory_ready;
                    inventory_ready = true;
                    steam_roots = roots;
                    games = updated;
                    errors = updated_errors;
                    inventory_pending = false;
                    inventory_dirty = true;
                    // A refresh every 30 seconds is not news unless it changed something.
                    if changed || !was_ready {
                        log(
                            &folder,
                            &format!("Inventory refreshed: {} installed locations", games.len()),
                        );
                    }
                    if let Ok(mut shared) = state.lock() {
                        let error_text = errors
                            .iter()
                            .map(|(name, error)| format!("{name}: {error}"))
                            .collect::<Vec<_>>()
                            .join("; ");
                        shared
                            .commands
                            .accept_inventory(accepted_generation, changed, &error_text);
                        shared.revision += 1;
                        shared.discovery_ready = true;
                        shared.ollama_installed = ollama;
                    }
                }
            }
            let guard_games = recovery_games(&engine, &games);
            let guarded_scan = engine.pending() || engine.gameplay.active() || engine.manual_pause;
            let detection_input = DetectionInput {
                power: power.clone(),
                power_generation,
                config: engine.config.clone(),
                games: games.clone(),
                guard_games: guard_games.clone(),
                steam_roots: steam_roots.clone(),
                guarded: guarded_scan,
                ready: inventory_ready && errors.is_empty(),
                ask_approved,
            };
            let frame = fresh_detection(&detection, detection_input.clone())?;
            // An answer lasts while an "ask" game runs; the next launch asks again.
            if frame.asking.is_empty() {
                ask_approved = false;
            }
            let suggestion = frame.suggestion.clone();
            let ask_prompt = if ask_approved {
                vec![]
            } else {
                frame.asking.clone()
            };
            let new_candidate = frame.candidate != last_candidate;
            last_candidate = frame.candidate;
            force_requested |= new_candidate;
            let all_active = frame.all;
            let active = frame.active;
            let gaming = !active.is_empty() || (engine.pending() && !all_active.is_empty());
            let mut evidence = frame.evidence;
            if !inventory_pending && (force_requested || now >= next_inventory) {
                generation = generation
                    .checked_add(1)
                    .context("Discovery generation exhausted")?;
                if request_tx
                    .send((
                        generation,
                        engine.config.clone(),
                        now,
                        force_requested,
                        gaming || new_candidate,
                    ))
                    .is_err()
                {
                    if let Ok(mut shared) = state.lock() {
                        shared.commands.discovery_unavailable(
                            "Discovery worker unavailable; refresh failed.".into(),
                        );
                    }
                    bail!("Discovery worker unavailable");
                }
                inventory_pending = true;
                force_requested = false;
                next_inventory = now + engine.config.discovery_seconds;
            }
            let records = guard_games
                .iter()
                .filter(|g| {
                    all_active
                        .iter()
                        .any(|a| crate::discovery::same_path(&a.path, &g.path))
                })
                .cloned()
                .collect();
            engine.remember_games(records)?;
            engine.lm_closed = !frame.lm_running;
            if inventory_ready && errors.is_empty() {
                let mut guard_input = detection_input.clone();
                guard_input.guarded = true;
                engine.step_games(evidence.clone(), now, &mut || {
                    evidence = fresh_detection(&detection, guard_input.clone())
                        .ok()
                        .and_then(|frame| frame.evidence);
                    evidence.clone()
                });
            } else {
                hold_for_discovery(&mut engine, inventory_ready);
            }
            if engine.awaiting_resume_detection() {
                engine.gameplay.invalidate_offer();
            } else {
                engine.gameplay.refresh_offer(
                    evidence.as_ref(),
                    engine.pending()
                        && engine.config.mode == "active"
                        && !engine.disabled
                        && !engine.activity.busy(),
                );
            }
            if inventory_ready
                && errors.is_empty()
                && evidence.as_ref().is_some_and(GameEvidence::reliable)
                && !engine.pending()
                && !gaming
                && !engine.gameplay.active()
                && engine.config.automation_enabled
                && engine.config.mode != "observe"
                && engine.config.lm_enabled()
                && !engine.lm_missing
                && !frame.lm_running
            {
                // A closed AI app is nothing to pause, not a problem to flag.
                engine.message = if engine.config.ollama_enabled() {
                    "Watching games; LM Studio is not open".into()
                } else {
                    "Watching games; LM Studio is not open, so there is nothing to pause".into()
                };
            }
            if !ask_prompt.is_empty()
                && !gaming
                && !engine.pending()
                && !engine.manual_pause
                && engine.config.mode != "observe"
            {
                engine.message = format!(
                    "{} is running; AI kept running. Choose Pause AI for this game to free memory.",
                    ask_prompt.join(", ")
                );
            }
            let status = json!({"version":env!("CARGO_PKG_VERSION"),"implementation":"Rust","mode":engine.config.mode,"automation_enabled":engine.config.automation_enabled,"message":engine.message,"active_games":active,"installed_locations":games.len(),"detection_disabled":engine.disabled,"manual_pause":engine.manual_pause,"last_error":engine.last_error,"discovery_errors":errors,"inaccessible_processes":frame.inaccessible,"recovery_pending":engine.pending(),"provider_outcomes":engine.provider_statuses});
            if let Ok(mut shared) = state.lock() {
                shared.restore_offer = engine.gameplay.offer();
                shared.coexistence =
                    engine.gameplay.active() && !engine.awaiting_resume_detection();
                if let Some(feedback) = &mut shared.restore_feedback
                    && feedback.tracking_recovery
                {
                    if !engine.pending() {
                        feedback.restoration = "AI restoration completed and verified.".into();
                        feedback.tracking_recovery = false;
                    } else if !engine.gameplay.active() {
                        feedback.restoration =
                            "Gameplay approval ended; recovery retained under normal game guards."
                                .into();
                    } else if engine.activity == Activity::PartialFailure {
                        feedback.restoration =
                            format!("Restore failed: {}. Recovery retained.", engine.last_error);
                    }
                }
                shared.activity = engine.activity;
                shared.message = engine.message.clone();
                shared.disabled = engine.disabled;
                shared.manual_pause = engine.manual_pause;
                shared.pause_completions = engine.pause_completions;
                shared.restore_completions = engine.restore_completions;
                shared.provider_statuses = engine.provider_statuses.clone();
                shared.lm_missing = engine.lm_missing;
                shared.freed_bytes = engine.freed_bytes;
                shared.ask_prompt = ask_prompt.clone();
                shared.suggestion = suggestion.clone();
                shared.pending = engine.pending();
                shared.active_mode = engine.config.mode == "active";
                shared.config = engine.config.clone();
                shared.games = games.clone();
                shared.discovery_errors = errors.clone();
                shared.commands.observe_engine(
                    engine.pause_verified(),
                    engine.pending(),
                    if matches!(
                        engine.activity,
                        Activity::PartialFailure | Activity::WaitingForInference
                    ) {
                        &engine.last_error
                    } else {
                        ""
                    },
                    &engine.message,
                );
            }
            if inventory_dirty {
                match write_json(&folder.join("inventory.json"), &games) {
                    Ok(()) => inventory_dirty = false,
                    Err(e) => persistence_error = Some(e),
                }
            }
            if status != last_status {
                if let Err(e) = write_json(&folder.join("status.json"), &status) {
                    persistence_error = Some(e);
                } else {
                    last_status = status;
                }
                log(&folder, &engine.message);
                if console {
                    println!("{}", engine.message);
                }
            }
            if let Some(error) = persistence_error {
                return Err(error);
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
                engine.gameplay.revoke();
                engine.activity = Activity::DetectionUnavailable;
                if let Ok(mut shared) = state.lock() {
                    shared.activity = Activity::DetectionUnavailable;
                    shared.detection_ok = false;
                    shared.coexistence = false;
                    shared.restore_offer = None;
                    shared.message = "Game detection or state persistence failed; AI control is held until a successful tick.".into();
                }
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
        let _ = writeln!(file, "{} {message}", local_timestamp());
    }
}
/// Local wall-clock time for log lines, as `2026-10-06 21:44:07`.
fn local_timestamp() -> String {
    let mut now = windows_sys::Win32::Foundation::SYSTEMTIME::default();
    unsafe {
        windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut now);
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond
    )
}
#[cfg(test)]
mod tests {
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
        let control =
            std::thread::spawn(move || held.recv_timeout(Duration::from_secs(3)).unwrap());
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
        let base =
            std::env::temp_dir().join(format!("gamepause-resilience-{}", std::process::id()));
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
            log_text.contains("app.rs"),
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
        let folder =
            std::env::temp_dir().join(format!("gamepause-appearance-{}", std::process::id()));
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
        let folder =
            std::env::temp_dir().join(format!("gamepause-advanced-{}", std::process::id()));
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
}
