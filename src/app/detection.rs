//! What the watcher asks the detection worker for and what a scan publishes.
//! Scheduling and cancellation live in `detection_worker`; this module owns the
//! scan itself, the evidence rules and the fresh guard used before AI control.

use super::SharedState;
use crate::{
    config::Config,
    discovery::Game,
    gameplay::GameEvidence,
    processes::{ActiveGame, RunningApp, Scanner},
};
use anyhow::{Result, bail};
use std::{sync::Arc, time::Duration};
pub(super) fn evidence_from(
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
pub(super) fn scan_evidence(
    guard: &mut Scanner,
    config: &Config,
    games: &[Game],
) -> Option<GameEvidence> {
    let all = guard.scan(games).ok()?;
    if guard.uncertain_games {
        return None;
    }
    let trigger_scanner = Scanner::new(config.clone()).ok()?;
    let evidence = evidence_from(all, &trigger_scanner, config, false);
    evidence.reliable().then_some(evidence)
}
pub(super) fn recovery_scanner(config: &Config) -> Result<Scanner> {
    let mut guard = config.clone();
    guard.excluded_paths.clear();
    guard.excluded_executables.clear();
    Scanner::new(guard)
}
#[derive(Clone)]
pub(super) struct DetectionInput {
    pub(super) power: Arc<crate::power::Signal>,
    pub(super) power_generation: u64,
    pub(super) config: Config,
    pub(super) games: Vec<Game>,
    pub(super) guard_games: Vec<Game>,
    pub(super) steam_roots: Vec<String>,
    pub(super) guarded: bool,
    pub(super) ready: bool,
    /// The user answered the current "ask" games with Pause.
    pub(super) ask_approved: bool,
}
#[derive(Clone)]
pub(super) struct DetectionFrame {
    pub(super) power_generation: u64,
    pub(super) config: Config,
    /// Running "ask" games, answered or not.
    pub(super) asking: Vec<String>,
    /// A fullscreen program nothing recognises, once it has stayed in front.
    pub(super) suggestion: Option<String>,
    pub(super) active: Vec<ActiveGame>,
    pub(super) all: Vec<ActiveGame>,
    pub(super) evidence: Option<GameEvidence>,
    pub(super) inaccessible: usize,
    pub(super) candidate: u64,
    pub(super) lm_running: bool,
    pub(super) ollama_running: bool,
    pub(super) running_apps: Vec<RunningApp>,
}
/// Scans a fullscreen program must lead before it is suggested.
const SUGGEST_AFTER_SCANS: u32 = 5;
pub(super) struct NativeDetection {
    power_generation: u64,
    config: Config,
    scanner: Scanner,
    guard: Scanner,
    candidate: u64,
    /// Unrecognised fullscreen executable and how many scans it has led.
    fullscreen: (String, u32),
}
impl NativeDetection {
    pub(super) fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            power_generation: 0,
            config: config.clone(),
            scanner: Scanner::new(config.clone())?,
            guard: recovery_scanner(config)?,
            candidate: 0,
            fullscreen: (String::new(), 0),
        })
    }
    pub(super) fn scan(&mut self, input: &DetectionInput) -> Result<DetectionFrame> {
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
pub(super) type BackgroundDetection = crate::detection_worker::DetectionWorker<
    DetectionInput,
    std::result::Result<DetectionFrame, String>,
>;
const FRESH_DETECTION_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) fn publish_detection(
    state: &SharedState,
    result: &std::result::Result<DetectionFrame, String>,
) {
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
pub(super) fn fresh_detection(
    worker: &BackgroundDetection,
    input: DetectionInput,
) -> Result<DetectionFrame> {
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
pub(super) fn action_evidence(
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
