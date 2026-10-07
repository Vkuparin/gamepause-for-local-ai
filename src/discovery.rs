//! Finds installed games. `Discovery` owns the refresh cadence, per-launcher
//! errors and the last good inventory of each launcher; the launcher adapters,
//! metadata parsers, registry reads and path rules are in the child modules.

mod launchers;
mod metadata;
mod paths;
mod registry;

pub use metadata::{parse_vdf, protobuf_fields};
pub use paths::{canonical, inside, same_path};

use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::panic::AssertUnwindSafe;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Game {
    pub launcher: String,
    pub identity: String,
    pub name: String,
    pub path: String,
}
impl Game {
    pub fn new(launcher: &str, id: &str, name: &str, path: impl AsRef<Path>) -> Self {
        Self {
            launcher: launcher.into(),
            identity: id.into(),
            name: name.into(),
            path: path.as_ref().to_string_lossy().into_owned(),
        }
    }
}
fn background_utility(game: &Game) -> bool {
    [
        "wallpaper engine",
        "lossless scaling",
        "steamworks common redistributables",
        "gog galaxy",
        "fps monitor",
    ]
    .contains(&game.name.to_lowercase().as_str())
}
#[derive(Default)]
pub struct Discovery {
    pub steam_libraries: Vec<String>,
    pub errors: BTreeMap<String, String>,
    retained: BTreeMap<String, Vec<Game>>,
    packages: Vec<Game>,
    package_at: Option<f64>,
    /// Xbox game folders found on fixed drives, and when they were looked for.
    xbox_roots: Vec<PathBuf>,
    xbox_roots_at: Option<f64>,
}
impl Discovery {
    pub fn refresh(
        &mut self,
        c: &Config,
        now: f64,
        force: bool,
        defer_packages: bool,
    ) -> Vec<Game> {
        self.errors.clear();
        // Each adapter replaces only its own last successful result.
        let adapters = [
            ("Steam", self.steam(c)),
            ("Epic", self.epic(c)),
            ("EA", self.ea()),
            ("Ubisoft", self.ubisoft()),
            ("GOG", self.gog()),
            ("Battle.net", self.battlenet()),
            ("Xbox", self.xbox(now, force, defer_packages)),
            ("Custom", Ok(self.custom(c))),
        ];
        for (name, result) in adapters {
            match result {
                Ok(games) => {
                    self.retained.insert(name.into(), games);
                }
                Err(e) => {
                    self.errors.insert(name.into(), format!("{e:#}"));
                }
            }
        }
        self.finalize()
    }
    /// The final inventory for this refresh: deduplicated, existing paths only,
    /// background utilities excluded. Read from `retained`, so it is stable
    /// across a refresh and can be reused by the panic-recovery path.
    fn finalize(&self) -> Vec<Game> {
        let mut seen = BTreeSet::new();
        self.retained
            .values()
            .flatten()
            .filter(|g| {
                !background_utility(g)
                    && Path::new(&g.path).exists()
                    && seen.insert(canonical(&g.path))
            })
            .cloned()
            .collect()
    }
    /// Run a scan body while containing any panic it raises. Returns
    /// `(inventory, panic_payload)`. A panic in an adapter — e.g. a future
    /// adapter added with an unwrapping bug, or a filesystem race in the Xbox
    /// `.GamingRoot` reader — must not kill the discovery worker and orphan the
    /// last good inventory: the payload is logged and recorded, and the last
    /// successful inventory is retained (best-available: each adapter keeps
    /// its own last good result).
    fn run_catchable(&mut self, body: impl FnOnce(&mut Self) -> Vec<Game>) -> (Vec<Game>, String) {
        match std::panic::catch_unwind(AssertUnwindSafe(|| body(self))) {
            Ok(games) => (games, String::new()),
            Err(panic) => {
                let payload = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_string()))
                    .unwrap_or_else(|| "unknown panic".into());
                self.errors.clear();
                self.errors.insert(
                    "Discovery".into(),
                    format!("Inventory scan panicked: {payload}"),
                );
                (self.finalize(), payload)
            }
        }
    }
    /// Production entry point: run a full refresh under panic containment.
    pub fn recover_refresh(
        &mut self,
        c: &Config,
        now: f64,
        force: bool,
        defer_packages: bool,
    ) -> (Vec<Game>, String) {
        self.run_catchable(move |d| d.refresh(c, now, force, defer_packages))
    }
    /// Only compiled for the production-profile regression example, never in default artifacts.
    #[cfg(feature = "resilience-test")]
    pub fn resilience_probe(&mut self) -> anyhow::Result<()> {
        let good = Game::new("Probe", "probe", "Retained", std::env::temp_dir());
        self.retained.insert("Probe".into(), vec![good]);
        let (games, panic) = self.run_catchable(|_| panic!("injected discovery panic"));
        if games.len() != 1 || panic.is_empty() {
            anyhow::bail!("Panic recovery failed");
        }
        let (games, panic) = self.run_catchable(|d| d.finalize());
        if games.len() != 1 || !panic.is_empty() {
            anyhow::bail!("Refresh after panic failed");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
