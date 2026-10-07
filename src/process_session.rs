//! Generic process provider: stop chosen executables for gaming and relaunch
//! them afterwards. Untested with the real tools its presets name; the
//! process mechanics themselves are exercised against stand-in executables.
mod native;
mod sealing;

pub use sealing::{open, seal};

use crate::{
    config::ProcessApp,
    coordinator::{Outcome, Planned, Runtime},
    discovery::{Game, canonical},
    provider::{Guarantee, Kind},
    recovery::{Binding, Entry, Intent},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Fixed route identity: this provider controls local processes, not a port.
pub const ROUTE: &str = "local-processes";
const MAX_APPS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub pid: u32,
    pub parent: u32,
    pub path: String,
}
/// What is needed to start a process again. Sealed before it is persisted
/// because a command line can carry an API key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    pub command_line: String,
    pub directory: String,
}
pub trait Os {
    /// Every running process whose image is one of `paths`.
    fn running(&mut self, paths: &[String]) -> Result<Vec<Found>>;
    fn launch_details(&mut self, pid: u32) -> Result<Launch>;
    /// Ask the process to close, then end it. Returns once it has exited.
    fn stop(&mut self, pid: u32) -> Result<()>;
    fn launch(&mut self, launch: &Launch) -> Result<()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Captured,
    Stopping,
    Stopped,
    Starting,
    Started,
}
/// One top-level instance that was running at capture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    pub name: String,
    pub path: String,
    pub relaunch: bool,
    /// Per-user encrypted `Launch`; empty when the app is not relaunched.
    pub sealed: String,
    pub stage: Stage,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    pub apps: Vec<App>,
    pub pause_complete: bool,
}
impl Snapshot {
    pub fn begin(&mut self) {
        self.pause_complete = false;
        for app in &mut self.apps {
            app.stage = Stage::Captured;
        }
    }
    pub fn units(&self) -> usize {
        self.apps.len()
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.apps.len() > MAX_APPS
            || self.apps.iter().any(|app| {
                app.name.is_empty()
                    || app.path.is_empty()
                    || app.relaunch == app.sealed.is_empty()
                    || !app.sealed.bytes().all(|c| c.is_ascii_hexdigit())
                    || (self.pause_complete && app.stage != Stage::Stopped)
            })
        {
            bail!("Unsupported or inconsistent process recovery; source retained");
        }
        Ok(())
    }
    pub fn validate_transition(&self, new: &Self) -> Result<()> {
        if self.version != new.version
            || self.apps.len() != new.apps.len()
            || self.apps.iter().zip(&new.apps).any(|(old, new)| {
                old.name != new.name
                    || old.path != new.path
                    || old.relaunch != new.relaunch
                    || old.sealed != new.sealed
            })
        {
            bail!("Original process recovery evidence changed");
        }
        Ok(())
    }
    /// Names the apps this session stops without a relaunch obligation.
    pub fn note(&self) -> String {
        let names = self
            .apps
            .iter()
            .filter(|app| !app.relaunch)
            .map(|app| app.name.as_str())
            .collect::<Vec<_>>();
        if names.is_empty() {
            String::new()
        } else {
            format!("Stopped without relaunch: {}.", names.join(", "))
        }
    }
    pub fn restore_finished(&self) -> bool {
        self.apps
            .iter()
            .all(|app| !app.relaunch || app.stage == Stage::Started)
    }
}

#[derive(Clone, Copy)]
enum Operation {
    Stop(usize),
    Start(usize),
    VerifyPause,
    VerifyRestore,
}
pub struct Work {
    snapshot: Snapshot,
    operation: Operation,
}
pub struct Adapter<O> {
    pub os: O,
    binding: Binding,
    apps: Vec<ProcessApp>,
    /// A retry plans every stop or start again, each behind its own checkpoint.
    retry: std::cell::Cell<bool>,
}
/// Processes of a path that were not started by another process of that path.
/// A bundled launcher and its worker count as one instance.
fn roots<'a>(found: &'a [Found], path: &str) -> Vec<&'a Found> {
    let same = |candidate: &&Found| canonical(&candidate.path) == canonical(path);
    found
        .iter()
        .filter(same)
        .filter(|process| {
            !found
                .iter()
                .filter(same)
                .any(|other| other.pid == process.parent && other.pid != process.pid)
        })
        .collect()
}
impl<O: Os> Adapter<O> {
    pub fn new(os: O, binding: Binding, apps: Vec<ProcessApp>) -> Result<Self> {
        if binding.kind != Kind::Process
            || binding.guarantee != Guarantee::ProcessRelaunch
            || binding.payload_version != 1
            || binding.endpoint != ROUTE
            || binding.configured_endpoint != ROUTE
        {
            bail!("Invalid process adapter binding");
        }
        Ok(Self {
            os,
            binding,
            apps,
            retry: std::cell::Cell::new(false),
        })
    }
    fn guard(&self, binding: &Binding) -> Result<()> {
        if *binding != self.binding {
            bail!("Process provider binding changed; recovery retained");
        }
        Ok(())
    }
    fn running(&mut self, snapshot: &Snapshot) -> Result<Vec<Found>> {
        let mut paths = snapshot
            .apps
            .iter()
            .map(|app| app.path.clone())
            .collect::<Vec<_>>();
        paths.dedup();
        self.os.running(&paths)
    }
}
impl<O: Os> Runtime for Adapter<O> {
    type Payload = Snapshot;
    type Work = Work;
    fn capture(&mut self, binding: &Binding, _: &[Game]) -> Result<Entry<Snapshot>> {
        self.guard(binding)?;
        let paths = self
            .apps
            .iter()
            .map(|app| app.path.clone())
            .collect::<Vec<_>>();
        let found = self.os.running(&paths)?;
        let mut apps = Vec::new();
        for configured in &self.apps {
            for process in roots(&found, &configured.path) {
                // Never stop what could not be started again when asked to.
                let sealed = if configured.relaunch {
                    let launch = self.os.launch_details(process.pid).with_context(|| {
                        format!(
                            "Could not read how {} was started; it was left running",
                            configured.name
                        )
                    })?;
                    seal(&launch)?
                } else {
                    String::new()
                };
                apps.push(App {
                    name: configured.name.clone(),
                    path: configured.path.clone(),
                    relaunch: configured.relaunch,
                    sealed,
                    stage: Stage::Captured,
                });
            }
        }
        let snapshot = Snapshot {
            version: 1,
            apps,
            pause_complete: false,
        };
        snapshot.validate()?;
        Ok(Entry {
            binding: binding.clone(),
            payload: snapshot,
            restore_complete: false,
        })
    }
    fn validate(&self, entry: &Entry<Snapshot>, _: &[Game]) -> Result<()> {
        self.guard(&entry.binding)?;
        entry.payload.validate()?;
        if entry.restore_complete
            && (entry.payload.pause_complete || !entry.payload.restore_finished())
        {
            bail!("Process completion conflicts with recovery progress");
        }
        Ok(())
    }
    fn validate_transition(&self, old: &Snapshot, new: &Snapshot) -> Result<()> {
        old.validate_transition(new)
    }
    fn begin(&self, payload: &mut Snapshot, _: Intent) -> Result<()> {
        payload.begin();
        Ok(())
    }
    fn complete(&self, payload: &Snapshot, intent: Intent) -> bool {
        intent == Intent::Pause && payload.pause_complete
    }
    fn note(&self, payload: &Snapshot) -> String {
        payload.note()
    }
    fn retry(&self, _: &Binding) {
        self.retry.set(true);
    }
    fn plan(&mut self, entry: &Entry<Snapshot>, intent: Intent) -> Result<Planned<Snapshot, Work>> {
        self.validate(entry, &[])?;
        let mut snapshot = entry.payload.clone();
        if self.retry.replace(false) {
            snapshot.begin();
        }
        let operation = match intent {
            Intent::Pause => match snapshot
                .apps
                .iter()
                .position(|app| app.stage != Stage::Stopped)
            {
                Some(index) => {
                    snapshot.apps[index].stage = Stage::Stopping;
                    Operation::Stop(index)
                }
                None => Operation::VerifyPause,
            },
            Intent::Restore => match snapshot
                .apps
                .iter()
                .position(|app| app.relaunch && app.stage != Stage::Started)
            {
                Some(index) => {
                    snapshot.apps[index].stage = Stage::Starting;
                    Operation::Start(index)
                }
                None => Operation::VerifyRestore,
            },
            Intent::Reconcile => bail!("Process control requires a reconciled direction"),
        };
        Ok(Planned {
            checkpoint: snapshot.clone(),
            work: Work {
                snapshot,
                operation,
            },
        })
    }
    fn execute(&mut self, binding: &Binding, work: Work) -> Result<Outcome<Snapshot>> {
        self.guard(binding)?;
        let mut snapshot = work.snapshot;
        snapshot.validate()?;
        let mut complete = false;
        match work.operation {
            Operation::Stop(index) => {
                // Stop by path, not by the captured process: a re-pause after an
                // interrupted restore must also stop what was relaunched.
                let path = snapshot.apps[index].path.clone();
                for process in self.os.running(std::slice::from_ref(&path))? {
                    self.os.stop(process.pid)?;
                }
                snapshot.apps[index].stage = Stage::Stopped;
            }
            Operation::VerifyPause => {
                if let Some(left) = self.running(&snapshot)?.first() {
                    bail!(
                        "{} is still running; pause incomplete",
                        file_name(&left.path)
                    );
                }
                snapshot.pause_complete = true;
            }
            Operation::Start(index) => {
                let app = snapshot.apps[index].clone();
                // The n-th instance of a path is already present when at least n
                // are running: after a lost response, or if the user restarted it.
                let wanted = snapshot.apps[..=index]
                    .iter()
                    .filter(|other| other.relaunch && other.path == app.path)
                    .count();
                let found = self.os.running(std::slice::from_ref(&app.path))?;
                if roots(&found, &app.path).len() < wanted {
                    let launch = open(&app.sealed).with_context(|| {
                        format!(
                            "{} was stopped and could not be restarted: its saved start command cannot be read on this Windows account",
                            app.name
                        )
                    })?;
                    self.os.launch(&launch).with_context(|| {
                        format!("{} was stopped and could not be restarted", app.name)
                    })?;
                }
                snapshot.apps[index].stage = Stage::Started;
            }
            Operation::VerifyRestore => {
                let found = self.running(&snapshot)?;
                for app in snapshot.apps.iter().filter(|app| app.relaunch) {
                    let wanted = snapshot
                        .apps
                        .iter()
                        .filter(|other| other.relaunch && other.path == app.path)
                        .count();
                    if roots(&found, &app.path).len() < wanted {
                        bail!(
                            "{} is not running after its restart; recovery retained",
                            app.name
                        );
                    }
                }
                complete = true;
            }
        }
        Ok(Outcome {
            payload: snapshot,
            restore_complete: complete,
        })
    }
}

pub fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Win32 process control. Runs on the control thread only.
#[derive(Default)]
pub struct Windows;
impl Os for Windows {
    fn running(&mut self, paths: &[String]) -> Result<Vec<Found>> {
        native::running(paths)
    }
    fn launch_details(&mut self, pid: u32) -> Result<Launch> {
        native::launch_details(pid)
    }
    fn stop(&mut self, pid: u32) -> Result<()> {
        native::stop(pid)
    }
    fn launch(&mut self, launch: &Launch) -> Result<()> {
        native::launch(launch)
    }
}

#[cfg(test)]
pub(crate) mod tests;
