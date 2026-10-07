//! Pure edits to a `Config` behind the dashboard's game and settings controls,
//! and the text of a round-trip report. Nothing here draws or sends an action.

use crate::{
    app::Shared,
    config::{Config, ExtraGame},
    discovery::canonical,
};
/// Locations of the running games whose rule is "ask".
pub(super) fn asking_paths(s: &Shared) -> Vec<String> {
    let mut paths: Vec<String> = s
        .active_games
        .iter()
        .filter(|game| s.config.asks(&game.path))
        .map(|game| game.path.clone())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}
/// Settings with the ask rule of these games replaced by On (`pause`) or Off.
pub(super) fn answer_ask(config: &Config, paths: &[String], pause: bool) -> Config {
    let mut updated = config.clone();
    for path in paths {
        set_ask(&mut updated, path, false);
        set_ignored(&mut updated, path, !pause);
    }
    updated
}
/// "Ask" and "ignore" are exclusive rules for one game.
pub(super) fn set_ask(config: &mut Config, path: &str, ask: bool) {
    config.ask_games.retain(|p| canonical(p) != canonical(path));
    if ask {
        set_ignored(config, path, false);
        config.ask_games.push(path.into());
    }
}
/// A running executable is ignored through `excluded_paths`, which detection
/// matches against process paths. `ignored_games` names game locations and
/// never matches an executable inside a launcher's game folder.
pub(super) fn exclude_executable(config: &mut Config, path: &str) {
    set_ignored(config, path, false);
    config.excluded_paths.push(path.into());
}
pub(super) fn set_ignored(config: &mut Config, path: &str, off: bool) {
    if off {
        config.ask_games.retain(|p| canonical(p) != canonical(path));
    }
    config
        .ignored_games
        .retain(|p| canonical(p) != canonical(path));
    config
        .excluded_paths
        .retain(|p| canonical(p) != canonical(path));
    if off {
        config.ignored_games.push(path.into());
    }
}
pub fn render_verify_report(report: &crate::engine::VerifyReport) -> String {
    let mut lines = report
        .steps
        .iter()
        .map(|s| {
            format!(
                "{}: {} ({})",
                s.name,
                s.detail,
                if s.ok { "ok" } else { "FAIL" }
            )
        })
        .collect::<Vec<_>>()
        .join("  |  ");
    lines.push_str("  —  ");
    lines.push_str(&report.summary);
    lines
}
pub fn apply_save(config: &Config, delay_text: &str, host_text: &str) -> Result<Config, String> {
    let mut out = config.clone();
    let delay = delay_text
        .trim()
        .parse::<f64>()
        .map_err(|_| "Restore delay must be a number of seconds.".to_string())?;
    if !(0.0..=3600.0).contains(&delay) {
        return Err("Restore delay must be between 0 and 3600 seconds.".to_string());
    }
    out.restore_delay_seconds = delay;
    let host = host_text.trim();
    if host.is_empty() {
        return Err("Local API address cannot be empty.".to_string());
    }
    out.lm_mut().map_err(|e| e.to_string())?.endpoint = host.into();
    out.validate()
        .map_err(|e| format!("Check these settings: {e:#}"))?;
    Ok(out)
}
/// Pure: apply the RENAME action to a config, returning the new config on
/// success or a human-readable error string for the FEEDBACK line.
/// Only rows that are custom (user-added) are renameable; discovered rows
/// keep their launcher-provided name.
pub fn apply_rename(config: &Config, row_path: &str, new_name: &str) -> Result<Config, String> {
    let name = new_name.trim();
    if name.is_empty() {
        return Err("Game name cannot be empty.".to_string());
    }
    let mut out = config.clone();
    let target = canonical(row_path);
    if let Some(game) = out
        .extra_games
        .iter_mut()
        .find(|g| canonical(&g.path) == target)
    {
        game.name = name.into();
        Ok(out)
    } else {
        Err("This game is not a custom entry — only games you added can be renamed.".to_string())
    }
}
pub(super) fn add_game(config: &mut Config, path: String, name: String) {
    config
        .excluded_paths
        .retain(|p| canonical(p) != canonical(&path));
    config
        .ignored_games
        .retain(|p| canonical(p) != canonical(&path));
    if !config
        .extra_games
        .iter()
        .any(|g| canonical(&g.path) == canonical(&path))
    {
        config.extra_games.push(ExtraGame { name, path });
    }
}
