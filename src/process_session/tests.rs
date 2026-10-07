use super::*;
use crate::coordinator::{Coordinator, State, Store};
use crate::recovery::Journal;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Default)]
struct Disk(Arc<Mutex<Option<Vec<u8>>>>);
impl Disk {
    fn journal(&self) -> Option<Journal<Snapshot>> {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .map(|bytes| serde_json::from_slice(bytes).unwrap())
    }
}
impl Store<Snapshot> for Disk {
    fn save(&mut self, journal: Option<&Journal<Snapshot>>) -> Result<()> {
        *self.0.lock().unwrap() = journal.map(serde_json::to_vec).transpose()?;
        Ok(())
    }
}
#[derive(Clone, Default)]
struct Fake {
    processes: Vec<(Found, Launch)>,
    next_pid: u32,
    stopped: Vec<u32>,
    launched: Vec<Launch>,
    unreadable: bool,
    refuse_launch: bool,
    survives_stop: bool,
    disk: Disk,
}
impl Fake {
    fn add(&mut self, path: &str, parent: u32, command_line: &str) -> u32 {
        self.next_pid += 1;
        self.processes.push((
            Found {
                pid: self.next_pid,
                parent,
                path: path.into(),
            },
            Launch {
                command_line: command_line.into(),
                directory: r"D:\Fixture".into(),
            },
        ));
        self.next_pid
    }
}
impl Os for Fake {
    fn running(&mut self, paths: &[String]) -> Result<Vec<Found>> {
        Ok(self
            .processes
            .iter()
            .filter(|(found, _)| paths.iter().any(|p| canonical(p) == canonical(&found.path)))
            .map(|(found, _)| found.clone())
            .collect())
    }
    fn launch_details(&mut self, pid: u32) -> Result<Launch> {
        if self.unreadable {
            bail!("fixture access denied");
        }
        Ok(self
            .processes
            .iter()
            .find(|(found, _)| found.pid == pid)
            .unwrap()
            .1
            .clone())
    }
    fn stop(&mut self, pid: u32) -> Result<()> {
        let journal = self.disk.journal().expect("intent persisted before a stop");
        assert_eq!(journal.session.intent, Intent::Pause);
        assert!(
            journal.providers[0]
                .payload
                .apps
                .iter()
                .any(|app| app.stage == Stage::Stopping)
        );
        self.stopped.push(pid);
        if !self.survives_stop {
            self.processes.retain(|(found, _)| found.pid != pid);
        }
        Ok(())
    }
    fn launch(&mut self, launch: &Launch) -> Result<()> {
        let journal = self
            .disk
            .journal()
            .expect("intent persisted before a start");
        assert_eq!(journal.session.intent, Intent::Restore);
        if self.refuse_launch {
            bail!("fixture launch refused");
        }
        self.launched.push(launch.clone());
        let path = launch
            .command_line
            .split('"')
            .nth(1)
            .unwrap_or_default()
            .to_string();
        self.add(&path, 0, &launch.command_line);
        Ok(())
    }
}
const SERVER: &str = r"D:\Fixture\llama-server.exe";
const KOBOLD: &str = r"D:\Fixture\koboldcpp.exe";
pub(crate) fn binding() -> Binding {
    Binding {
        id: "apps-main".into(),
        kind: Kind::Process,
        endpoint: ROUTE.into(),
        configured_endpoint: ROUTE.into(),
        payload_version: 1,
        guarantee: Guarantee::ProcessRelaunch,
    }
}
fn app(path: &str, relaunch: bool) -> ProcessApp {
    ProcessApp {
        name: file_name(path).into(),
        path: path.into(),
        relaunch,
    }
}
type TestCoordinator = Coordinator<Adapter<Fake>, Disk>;
fn fixture(os: Fake, apps: Vec<ProcessApp>) -> TestCoordinator {
    let disk = os.disk.clone();
    Coordinator::new(
        Adapter::new(os, binding(), apps).unwrap(),
        disk,
        vec![binding()],
        None,
        Duration::from_secs(10),
    )
    .unwrap()
}
fn drive(c: &mut TestCoordinator, intent: Intent, now: u64) {
    for _ in 0..10 {
        c.advance(intent, &[], Duration::from_secs(now), &mut || false)
            .unwrap();
    }
}
#[test]
fn this_process_start_command_and_folder_are_readable() {
    let launch = Windows.launch_details(std::process::id()).unwrap();
    let exe = std::env::current_exe().unwrap();
    let name = exe.file_stem().unwrap().to_string_lossy().to_string();
    assert!(
        launch.command_line.contains(&name),
        "{}",
        launch.command_line
    );
    assert_eq!(
        canonical(&launch.directory),
        canonical(&std::env::current_dir().unwrap().to_string_lossy())
    );
    let me = Windows
        .running(&[exe.to_string_lossy().to_string()])
        .unwrap();
    assert!(me.iter().any(|found| found.pid == std::process::id()));
    assert!(Windows.launch_details(0).is_err());
}
#[test]
fn sealed_start_commands_round_trip_and_hide_their_text() {
    let launch = Launch {
        command_line: r#""D:\Fixture\llama-server.exe" --api-key fixture-secret -m model.gguf"#
            .into(),
        directory: r"D:\Fixture".into(),
    };
    let sealed = seal(&launch).unwrap();
    assert!(sealed.bytes().all(|c| c.is_ascii_hexdigit()));
    assert!(!sealed.contains("fixture-secret"));
    let hexed = "fixture-secret"
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    assert!(
        !sealed.contains(&hexed),
        "the secret must not be merely encoded"
    );
    assert_eq!(open(&sealed).unwrap(), launch);
    for broken in ["", "zz", "abc", &sealed[..sealed.len() - 2]] {
        assert!(open(broken).is_err());
    }
}
#[test]
fn stops_then_relaunches_each_instance_with_its_own_command() {
    let mut os = Fake::default();
    os.add(
        SERVER,
        0,
        &format!(r#""{SERVER}" --port 8080 --api-key fixture-secret"#),
    );
    os.add(SERVER, 0, &format!(r#""{SERVER}" --port 8081"#));
    os.add(r"D:\Other\llama-server.exe", 0, "bundled by another app");
    let mut c = fixture(os, vec![app(SERVER, true)]);
    drive(&mut c, Intent::Pause, 0);
    assert!(c.pause_complete());
    assert_eq!(c.runtime.os.stopped, [1, 2]);
    assert_eq!(c.runtime.os.processes.len(), 1, "another path is untouched");
    let saved = serde_json::to_string(&c.store.journal().unwrap()).unwrap();
    // Markers that hex ciphertext cannot spell by chance, unlike a bare "8080".
    assert!(!saved.contains("fixture-secret") && !saved.contains("--port"));
    drive(&mut c, Intent::Restore, 1);
    assert!(c.journal().is_none());
    assert_eq!(
        c.runtime
            .os
            .launched
            .iter()
            .map(|launch| launch.command_line.clone())
            .collect::<Vec<_>>(),
        [
            format!(r#""{SERVER}" --port 8080 --api-key fixture-secret"#),
            format!(r#""{SERVER}" --port 8081"#)
        ]
    );
    assert_eq!(c.reports(Duration::from_secs(1))[0].state, State::Restored);
}
#[test]
fn a_launcher_and_its_worker_are_one_instance_and_no_relaunch_is_reported() {
    let mut os = Fake::default();
    let parent = os.add(KOBOLD, 0, &format!(r#""{KOBOLD}" --model fixture.gguf"#));
    os.add(
        KOBOLD,
        parent,
        &format!(r#""{KOBOLD}" --model fixture.gguf"#),
    );
    os.add(SERVER, 0, &format!(r#""{SERVER}""#));
    let mut c = fixture(os, vec![app(KOBOLD, true), app(SERVER, false)]);
    drive(&mut c, Intent::Pause, 0);
    assert!(c.pause_complete());
    assert!(c.runtime.os.processes.is_empty());
    let saved = c.journal().unwrap().providers[0].payload.clone();
    assert_eq!(saved.apps.len(), 2);
    assert_eq!(saved.note(), "Stopped without relaunch: llama-server.exe.");
    assert!(saved.apps[1].sealed.is_empty());
    drive(&mut c, Intent::Restore, 1);
    assert!(c.journal().is_none());
    assert_eq!(c.runtime.os.launched.len(), 1);
    assert_eq!(
        c.reports(Duration::from_secs(1))[0].note,
        "Stopped without relaunch: llama-server.exe."
    );
}
#[test]
fn unreadable_start_command_leaves_the_app_running() {
    let mut os = Fake::default();
    os.add(SERVER, 0, "fixture");
    os.unreadable = true;
    let mut c = fixture(os, vec![app(SERVER, true)]);
    drive(&mut c, Intent::Pause, 0);
    assert_eq!(c.statuses()["apps-main"].state, State::Failed);
    assert!(c.runtime.os.stopped.is_empty());
    assert!(c.store.journal().is_none());
    assert!(c.statuses()["apps-main"].error.contains("left running"));
}
#[test]
fn failed_relaunch_retains_recovery_and_a_user_restart_is_not_duplicated() {
    let mut os = Fake::default();
    os.add(SERVER, 0, &format!(r#""{SERVER}" --port 8080"#));
    let mut c = fixture(os, vec![app(SERVER, true)]);
    drive(&mut c, Intent::Pause, 0);
    c.runtime.os.refuse_launch = true;
    drive(&mut c, Intent::Restore, 1);
    assert_eq!(c.statuses()["apps-main"].state, State::Failed);
    assert!(
        c.statuses()["apps-main"]
            .error
            .contains("could not be restarted")
    );
    let original = c.store.journal().unwrap().providers[0].payload.apps[0]
        .sealed
        .clone();
    // The user starts it by hand before the retry.
    c.runtime.os.add(SERVER, 0, "started by hand");
    drive(&mut c, Intent::Restore, 11);
    assert!(c.journal().is_none());
    assert!(c.runtime.os.launched.is_empty());
    assert!(!original.is_empty());
}
#[test]
fn surviving_process_keeps_pause_incomplete_and_repause_stops_relaunched_ones() {
    let mut os = Fake::default();
    os.add(SERVER, 0, &format!(r#""{SERVER}""#));
    os.survives_stop = true;
    let mut c = fixture(os, vec![app(SERVER, true)]);
    drive(&mut c, Intent::Pause, 0);
    assert_eq!(c.statuses()["apps-main"].state, State::Failed);
    assert!(!c.pause_complete());
    c.runtime.os.survives_stop = false;
    drive(&mut c, Intent::Pause, 10);
    assert!(c.pause_complete());
    // Restore starts it, a game interrupts, and the new instance is stopped.
    c.advance(Intent::Restore, &[], Duration::from_secs(11), &mut || false)
        .unwrap();
    c.advance(Intent::Restore, &[], Duration::from_secs(11), &mut || false)
        .unwrap();
    assert_eq!(c.runtime.os.processes.len(), 1);
    drive(&mut c, Intent::Pause, 12);
    assert!(c.pause_complete());
    assert!(c.runtime.os.processes.is_empty());
    drive(&mut c, Intent::Restore, 13);
    assert!(c.journal().is_none());
    assert_eq!(c.runtime.os.processes.len(), 1);
}
#[test]
fn nothing_running_is_an_empty_session_and_tampered_recovery_is_refused() {
    let mut c = fixture(Fake::default(), vec![app(SERVER, true)]);
    drive(&mut c, Intent::Pause, 0);
    assert!(c.pause_complete());
    assert_eq!(c.journal().unwrap().providers[0].payload.units(), 0);
    drive(&mut c, Intent::Restore, 1);
    assert!(c.journal().is_none());
    let good = Snapshot {
        version: 1,
        apps: vec![App {
            name: "fixture".into(),
            path: SERVER.into(),
            relaunch: true,
            sealed: "ab".into(),
            stage: Stage::Captured,
        }],
        pause_complete: false,
    };
    good.validate().unwrap();
    let mut changed = good.clone();
    changed.apps[0].sealed = "cd".into();
    assert!(good.validate_transition(&changed).is_err());
    for edit in 0..4 {
        let mut bad = good.clone();
        match edit {
            0 => bad.version = 2,
            1 => bad.apps[0].sealed.clear(),
            2 => bad.apps[0].sealed = "not hex".into(),
            _ => bad.pause_complete = true,
        }
        assert!(bad.validate().is_err(), "{edit}");
    }
}
