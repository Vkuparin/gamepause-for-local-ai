//! What the dashboard shows, derived from a `Shared` snapshot: the table rows
//! for each page, the hero summary and the provider status lines. No drawing.

use super::Page;
use crate::{
    app::Shared,
    config::Config,
    dashboard_theme::{Icon, Look},
    discovery::canonical,
};
use std::collections::HashSet;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Row {
    pub(super) name: String,
    pub(super) path: String,
    pub(super) platform: String,
    pub(super) custom: bool,
    pub(super) ignored: bool,
    pub(super) ask: bool,
    pub(super) pid: Option<u32>,
}
pub(super) fn ignored(config: &Config, path: &str) -> bool {
    config
        .ignored_games
        .iter()
        .chain(&config.excluded_paths)
        .any(|p| canonical(p) == canonical(path))
}
pub(super) fn rows(shared: &Shared, page: Page, query: &str) -> Vec<Row> {
    let status = |path: &str| {
        shared.active_games.iter().find(|g| {
            canonical(&g.path) == canonical(path) || canonical(&g.executable) == canonical(path)
        })
    };
    let mut rows: Vec<Row> = match page {
        Page::Games => shared
            .games
            .iter()
            .map(|g| Row {
                name: g.name.clone(),
                path: g.path.clone(),
                platform: g.launcher.clone(),
                custom: g.launcher == "Custom",
                ignored: ignored(&shared.config, &g.path),
                ask: shared.config.asks(&g.path),
                pid: status(&g.path).map(|g| g.pid),
            })
            .collect(),
        Page::Running => shared
            .running_apps
            .iter()
            .map(|a| {
                let recognized = shared
                    .games
                    .iter()
                    .find(|g| crate::discovery::inside(&a.path, &g.path));
                Row {
                    name: a.name.clone(),
                    path: a.path.clone(),
                    platform: recognized
                        .map_or("Unrecognized", |g| g.launcher.as_str())
                        .into(),
                    custom: false,
                    ignored: ignored(&shared.config, &a.path),
                    ask: shared.config.asks(&a.path),
                    pid: status(&a.path).map(|g| g.pid),
                }
            })
            .collect(),
        Page::Ignored => shared
            .config
            .ignored_games
            .iter()
            .chain(&shared.config.excluded_paths)
            .map(|path| {
                let game = shared
                    .games
                    .iter()
                    .find(|g| canonical(&g.path) == canonical(path));
                Row {
                    name: game.map_or_else(
                        || path.rsplit(['\\', '/']).next().unwrap_or(path).into(),
                        |g| g.name.clone(),
                    ),
                    path: path.clone(),
                    platform: game
                        .map_or("Custom exclusion", |g| g.launcher.as_str())
                        .into(),
                    custom: game.is_some_and(|g| g.launcher == "Custom"),
                    ignored: true,
                    ask: false,
                    pid: status(path).map(|g| g.pid),
                }
            })
            .collect(),
        Page::Activity => vec![],
    };
    let query = query.to_lowercase();
    let mut seen = HashSet::new();
    rows.retain(|r| {
        (format!("{} {} {}", r.name, r.path, r.platform)
            .to_lowercase()
            .contains(&query))
            && seen.insert(canonical(&r.path))
    });
    rows.sort_by_cached_key(|r| r.name.to_lowercase());
    rows
}
/// `ascending` keeps the name order from `rows` within equal keys.
pub(super) fn sort_rows(rows: &mut [Row], (column, ascending): (usize, bool), by_pid: bool) {
    match column {
        1 => rows.sort_by_cached_key(|r| r.platform.to_lowercase()),
        2 if by_pid => rows.sort_by_key(|r| r.pid),
        2 => rows.sort_by_key(|r| r.ignored),
        _ => (),
    }
    if !ascending {
        rows.reverse();
    }
}
pub(super) fn running_executable<'a>(s: &'a Shared, path: &str) -> Option<&'a str> {
    s.active_games
        .iter()
        .find(|g| canonical(&g.path) == canonical(path))
        .map(|g| g.executable.as_str())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    Neutral,
    Busy,
    Success,
    Error,
}
pub(super) struct Hero {
    pub(super) title: &'static str,
    pub(super) game: Option<String>,
    pub(super) hint: &'static str,
    pub(super) look: Look,
    pub(super) glyph: Icon,
}
/// The shared short status plus the dashboard's glyph for its look.
pub(super) fn hero(s: &Shared) -> Hero {
    let crate::presentation::Status {
        title,
        game,
        hint,
        look,
    } = crate::presentation::status(s);
    Hero {
        title,
        game,
        hint,
        look,
        glyph: match look {
            Look::Running => Icon::Play,
            Look::Paused => Icon::Pause,
            Look::Loading => Icon::Dots,
            Look::Attention => Icon::Alert,
        },
    }
}
/// Whether the provider's program was seen by the last process scan. Other
/// AI apps are checked only when a game starts, so they count as present.
fn app_running(s: &Shared, kind: crate::provider::Kind) -> bool {
    match kind {
        crate::provider::Kind::LMStudio => s.lm_running,
        crate::provider::Kind::Ollama => s.ollama_running,
        crate::provider::Kind::Process => true,
    }
}
/// The status card names only AI apps this PC has: installed or running, or
/// holding work from the current session. Someone who uses one app is not
/// shown a line for the other.
fn listed(s: &Shared, provider: &crate::config::Provider) -> bool {
    use crate::provider::Kind;
    match provider.kind() {
        Kind::LMStudio => !s.lm_missing,
        Kind::Ollama => {
            s.ollama_installed
                || s.ollama_running
                || s.provider_statuses.iter().any(|report| {
                    report.kind == Kind::Ollama
                        && report.id == provider.id()
                        && report.note != crate::ollama_session::NOT_RUNNING
                })
        }
        Kind::Process => true,
    }
}
/// One line per listed provider: name, short outcome, dot tone.
pub(super) fn provider_status(s: &Shared) -> Vec<(String, &'static str, Tone)> {
    use crate::control::Activity;
    use crate::coordinator::State;
    let activity = if s.verifying {
        Activity::Verifying
    } else {
        s.activity
    };
    s.config
        .providers
        .iter()
        .filter(|p| p.enabled() && listed(s, p))
        .map(|p| {
            let report = s
                .provider_statuses
                .iter()
                .find(|r| r.id == p.id() && r.kind == p.kind());
            let state = report.map(|r| r.state);
            let absent = report.is_some_and(|r| r.note == crate::ollama_session::NOT_RUNNING);
            let (text, tone) = match (state, activity) {
                (Some(State::Paused | State::Restored), _) if absent => {
                    ("Not running", Tone::Neutral)
                }
                (Some(State::Failed), _) => ("Something went wrong", Tone::Error),
                (Some(State::Deferred), _) => ("Waiting", Tone::Busy),
                (Some(State::Pausing), _) => ("Pausing AI", Tone::Busy),
                (Some(State::Restoring), _) => ("Resuming AI", Tone::Busy),
                // A report from the previous session can outlive the start of new work.
                (
                    None | Some(State::Restored),
                    Activity::Capturing | Activity::WaitingForInference | Activity::Unloading,
                ) => ("Pausing AI", Tone::Busy),
                (None | Some(State::Paused), Activity::Restoring) => ("Resuming AI", Tone::Busy),
                (_, Activity::Verifying) => ("Testing", Tone::Busy),
                (Some(State::Paused), _) => ("Models unloaded successfully", Tone::Success),
                (Some(State::Restored), _) => ("Models restored", Tone::Success),
                (_, Activity::Unknown | Activity::DetectionUnavailable) => {
                    ("Waiting for game detection", Tone::Neutral)
                }
                // "Ready" claims the app is there to be paused.
                _ if !app_running(s, p.kind()) => ("Not running", Tone::Neutral),
                _ => ("Ready", Tone::Success),
            };
            (format!("{}:", p.kind().name()), text, tone)
        })
        .collect()
}
