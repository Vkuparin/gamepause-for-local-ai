//! Generic process provider: stop chosen executables for gaming and relaunch
//! them afterwards. Untested with the real tools its presets name; the
//! process mechanics themselves are exercised against stand-in executables.
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

/// Encrypts with Windows per-user data protection and hex-encodes the result.
/// Another account or another PC cannot read it.
pub fn seal(launch: &Launch) -> Result<String> {
    let plain = serde_json::to_vec(launch)?;
    Ok(protect(&plain, true)?
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
pub fn open(sealed: &str) -> Result<Launch> {
    if sealed.is_empty() || !sealed.len().is_multiple_of(2) || !sealed.is_ascii() {
        bail!("Saved start command is missing or malformed");
    }
    let bytes = (0..sealed.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&sealed[index..index + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Saved start command is malformed")?;
    serde_json::from_slice(&protect(&bytes, false)?).context("Saved start command is malformed")
}
fn protect(input: &[u8], seal: bool) -> Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    let source = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(input.len()).context("Start command is too large")?,
        pbData: input.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        if seal {
            CryptProtectData(
                &source,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &source,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 || output.pbData.is_null() {
        bail!("Windows data protection refused the start command");
    }
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        LocalFree(output.pbData.cast());
    }
    Ok(bytes)
}

/// Win32 process control. Runs on the control thread only.
#[derive(Default)]
pub struct Windows;
mod native {
    use super::{Found, Launch};
    use anyhow::{Context, Result, bail};
    use std::ffi::c_void;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, WAIT_OBJECT_0},
        System::{
            Diagnostics::{
                Debug::ReadProcessMemory,
                ToolHelp::{
                    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                    TH32CS_SNAPPROCESS,
                },
            },
            Threading::{
                CREATE_NEW_CONSOLE, CreateProcessW, OpenProcess, PROCESS_INFORMATION,
                PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
                PROCESS_TERMINATE, PROCESS_VM_READ, QueryFullProcessImageNameW,
                STARTF_USESHOWWINDOW, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
            },
        },
        UI::WindowsAndMessaging::{
            EnumWindows, GetWindowThreadProcessId, PostMessageW, SW_SHOWMINNOACTIVE, WM_CLOSE,
        },
    };
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryInformationProcess(
            process: HANDLE,
            class: u32,
            information: *mut c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
    }
    const MAX_COMMAND_BYTES: usize = 64 * 1024;
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    fn open(pid: u32, access: u32) -> Result<Handle> {
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() {
            bail!("Windows refused access to the process");
        }
        Ok(Handle(handle))
    }
    fn image(pid: u32) -> Option<String> {
        let handle = open(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
        let mut buffer = vec![0u16; 32_768];
        let mut length = buffer.len() as u32;
        (unsafe { QueryFullProcessImageNameW(handle.0, 0, buffer.as_mut_ptr(), &mut length) } != 0)
            .then(|| String::from_utf16_lossy(&buffer[..length as usize]))
    }
    pub fn running(paths: &[String]) -> Result<Vec<Found>> {
        let wanted = paths
            .iter()
            .map(|path| crate::discovery::canonical(path))
            .collect::<Vec<_>>();
        let names = paths
            .iter()
            .map(|path| super::file_name(path).to_ascii_lowercase())
            .collect::<Vec<_>>();
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            bail!("Could not list running processes");
        }
        let snapshot = Handle(snapshot);
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut found = Vec::new();
        let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
        while more {
            let length = entry
                .szExeFile
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..length]).to_ascii_lowercase();
            // Open only processes whose file name matches a configured app.
            if names.contains(&name)
                && let Some(path) = image(entry.th32ProcessID)
                && wanted.contains(&crate::discovery::canonical(&path))
            {
                found.push(Found {
                    pid: entry.th32ProcessID,
                    parent: entry.th32ParentProcessID,
                    path,
                });
            }
            more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
        }
        Ok(found)
    }
    fn read<T: Copy>(process: HANDLE, address: usize) -> Result<T> {
        let mut value = std::mem::MaybeUninit::<T>::zeroed();
        let mut done = 0usize;
        if address == 0
            || unsafe {
                ReadProcessMemory(
                    process,
                    address as *const c_void,
                    value.as_mut_ptr().cast(),
                    size_of::<T>(),
                    &mut done,
                )
            } == 0
            || done != size_of::<T>()
        {
            bail!("Could not read the process start information");
        }
        Ok(unsafe { value.assume_init() })
    }
    /// A UNICODE_STRING in the target: length in bytes, then a buffer pointer.
    fn text(process: HANDLE, address: usize) -> Result<String> {
        let length = read::<u16>(process, address)? as usize;
        let buffer = read::<usize>(process, address + 8)?;
        if length == 0 {
            return Ok(String::new());
        }
        if length > MAX_COMMAND_BYTES || !length.is_multiple_of(2) {
            bail!("Process start information is too large");
        }
        let mut units = vec![0u16; length / 2];
        let mut done = 0usize;
        if unsafe {
            ReadProcessMemory(
                process,
                buffer as *const c_void,
                units.as_mut_ptr().cast(),
                length,
                &mut done,
            )
        } == 0
            || done != length
        {
            bail!("Could not read the process start information");
        }
        Ok(String::from_utf16_lossy(&units))
    }
    /// Command line and working directory from the 64-bit process parameters.
    #[cfg(target_pointer_width = "64")]
    pub fn launch_details(pid: u32) -> Result<Launch> {
        let handle = open(pid, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)?;
        let mut basic = [0usize; 6];
        if unsafe {
            NtQueryInformationProcess(
                handle.0,
                0,
                basic.as_mut_ptr().cast(),
                size_of_val(&basic) as u32,
                std::ptr::null_mut(),
            )
        } != 0
        {
            bail!("Could not query the process start information");
        }
        let parameters = read::<usize>(handle.0, basic[1] + 0x20)?;
        let command_line = text(handle.0, parameters + 0x70)?;
        if command_line.trim().is_empty() {
            bail!("The process has no readable start command");
        }
        Ok(Launch {
            command_line,
            directory: text(handle.0, parameters + 0x38)?
                .trim_end_matches('\\')
                .to_string(),
        })
    }
    unsafe extern "system" fn close_window(window: HWND, pid: LPARAM) -> i32 {
        let mut owner = 0u32;
        unsafe {
            GetWindowThreadProcessId(window, &mut owner);
            if owner == pid as u32 {
                PostMessageW(window, WM_CLOSE, 0, 0);
            }
        }
        1
    }
    pub fn stop(pid: u32) -> Result<()> {
        let Ok(handle) = open(pid, PROCESS_SYNCHRONIZE | PROCESS_TERMINATE) else {
            // Already gone, or not ours to stop; the absence check decides.
            return Ok(());
        };
        unsafe {
            EnumWindows(Some(close_window), pid as LPARAM);
            if WaitForSingleObject(handle.0, 3_000) == WAIT_OBJECT_0 {
                return Ok(());
            }
            TerminateProcess(handle.0, 1);
            if WaitForSingleObject(handle.0, 5_000) != WAIT_OBJECT_0 {
                bail!("The process did not exit");
            }
        }
        Ok(())
    }
    pub fn launch(launch: &Launch) -> Result<()> {
        let mut command = crate::wide(&launch.command_line);
        let directory = crate::wide(&launch.directory);
        let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        // Come back minimised and without taking focus from the user.
        startup.dwFlags = STARTF_USESHOWWINDOW;
        startup.wShowWindow = SW_SHOWMINNOACTIVE as u16;
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe {
            CreateProcessW(
                std::ptr::null(),
                command.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_NEW_CONSOLE,
                std::ptr::null(),
                if launch.directory.is_empty() {
                    std::ptr::null()
                } else {
                    directory.as_ptr()
                },
                &startup,
                &mut process,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error()).context("Windows could not start it");
        }
        unsafe {
            CloseHandle(process.hThread);
            CloseHandle(process.hProcess);
        }
        Ok(())
    }
}
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
pub(crate) mod tests {
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
        assert!(!saved.contains("fixture-secret") && !saved.contains("8080"));
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
}
