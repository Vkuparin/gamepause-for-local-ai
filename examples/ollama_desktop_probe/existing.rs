//! Live-test-only original-instance preservation. Startup evidence is sealed
//! before Quit; it is never printed, and the user profile is never replaced.
use super::*;
use gamepause::process_session::{Launch, Os, Windows};
use std::{ffi::c_void, io::Write, os::windows::fs::OpenOptionsExt};
use windows_sys::Win32::{
    Storage::FileSystem::FILE_SHARE_READ,
    System::{
        Diagnostics::Debug::ReadProcessMemory,
        Threading::{PROCESS_QUERY_INFORMATION, PROCESS_VM_READ},
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

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Startup {
    launch: Launch,
    environment: Vec<u16>,
}

fn pointer(handle: HANDLE, address: usize) -> Result<usize> {
    let mut value = 0usize;
    let mut done = 0;
    if unsafe {
        ReadProcessMemory(
            handle,
            address as *const c_void,
            (&raw mut value).cast(),
            size_of::<usize>(),
            &mut done,
        )
    } == 0
        || done != size_of::<usize>()
    {
        bail!("Cannot read original startup environment");
    }
    Ok(value)
}

fn environment(pid: u32) -> Result<Vec<u16>> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid) };
    if handle.is_null() {
        bail!("Cannot read original startup environment");
    }
    let handle = Handle(handle);
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
        bail!("Cannot query original process parameters");
    }
    let parameters = pointer(handle.0, basic[1] + 0x20)?;
    let address = pointer(handle.0, parameters + 0x80)?;
    let mut result = Vec::new();
    // Read one page fragment at a time; never read beyond a page boundary.
    while result.len() < 128 * 1024 {
        let next = address + result.len() * 2;
        let units = ((4096 - (next % 4096)) / 2).min(256);
        let mut chunk = vec![0u16; units];
        let mut done = 0;
        if unsafe {
            ReadProcessMemory(
                handle.0,
                next as *const c_void,
                chunk.as_mut_ptr().cast(),
                units * 2,
                &mut done,
            )
        } == 0
            || done != units * 2
        {
            bail!("Original environment is incomplete; no shutdown allowed");
        }
        for unit in chunk {
            let finished = unit == 0 && result.last() == Some(&0);
            result.push(unit);
            if finished {
                return Ok(result);
            }
        }
    }
    bail!("Original environment exceeds test capture limit");
}

pub(super) struct Original {
    exe: PathBuf,
    sealed: Option<String>,
    journal: PathBuf,
    // Refuse executable replacement while this experiment runs.
    _files: Vec<std::fs::File>,
}
impl Original {
    pub(super) fn recover_test(exe: &Path, profile: &Path) -> Result<()> {
        let journal = profile.join("original-desktop.sealed");
        let sealed = fs::read_to_string(&journal)?;
        if !tray().is_null() {
            let mut pid = 0;
            unsafe { GetWindowThreadProcessId(tray(), &mut pid) };
            let environment = environment(pid)?;
            let matches = environment
                .split(|unit| *unit == 0)
                .filter_map(|entry| String::from_utf16(entry).ok())
                .any(|entry| {
                    entry.strip_prefix("LOCALAPPDATA=").is_some_and(|path| {
                        gamepause::discovery::same_path(path, &profile.to_string_lossy())
                    })
                });
            if !matches {
                bail!("Current Desktop is not this private test profile; no cleanup attempted");
            }
            let mut child = Child {
                handle: Handle(unsafe {
                    OpenProcess(
                        PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                        0,
                        pid,
                    )
                }),
                pid,
            };
            let managed = server(&child, profile, &exe.with_file_name("ollama.exe"))?;
            quit(&mut child)?;
            if unsafe { WaitForSingleObject(managed.0, WAIT.as_millis() as u32) } != WAIT_OBJECT_0 {
                bail!("Private server cleanup unverified");
            }
        }
        let mut original = Self {
            exe: exe.to_owned(),
            sealed: Some(sealed),
            journal,
            _files: vec![],
        };
        for path in [exe.to_owned(), exe.with_file_name("ollama.exe")] {
            original._files.push(
                fs::OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ)
                    .open(path)?,
            );
        }
        original.restore()
    }
    pub(super) fn capture(exe: &Path, profile: &Path) -> Result<Self> {
        let mut original = Self {
            exe: exe.to_owned(),
            sealed: None,
            journal: profile.join("original-desktop.sealed"),
            _files: vec![],
        };
        for path in [exe.to_owned(), exe.with_file_name("ollama.exe")] {
            original._files.push(
                fs::OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ)
                    .open(path)?,
            );
        }
        if tray().is_null() {
            return Ok(original);
        }
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(tray(), &mut pid) };
        let process = Windows
            .running(&[exe.to_string_lossy().into_owned()])?
            .into_iter()
            .find(|p| p.pid == pid)
            .context("Original tray identity is ambiguous")?;
        let launch = Windows.launch_details(&process)?;
        let env = environment(pid)?;
        if environment(pid)? != env {
            bail!("Original environment changed during capture");
        }
        let mut http = Http::new("127.0.0.1:11434")?;
        let version: serde_json::Value =
            serde_json::from_slice(&http.request("/api/version", None)?)?;
        let resident: serde_json::Value = serde_json::from_slice(&http.request("/api/ps", None)?)?;
        if version["version"] != VERSION
            || resident["models"].as_array().is_none_or(|m| !m.is_empty())
        {
            bail!("Original instance must be 0.40.3 with no resident models");
        }
        let managed = server(
            &Child {
                handle: Handle(unsafe {
                    OpenProcess(
                        PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                        0,
                        pid,
                    )
                }),
                pid,
            },
            &PathBuf::from(std::env::var_os("LOCALAPPDATA").context("No local profile")?),
            &exe.with_file_name("ollama.exe"),
        )?;
        // The seal API encrypts a Launch-shaped container. This command_line
        // contains serialized test startup evidence and is never executed.
        let sealed = gamepause::process_session::seal(&Launch {
            command_line: serde_json::to_string(&Startup {
                launch,
                environment: env,
            })?,
            directory: String::new(),
        })?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&original.journal)?;
        file.write_all(sealed.as_bytes())?;
        file.sync_all()?;
        original.sealed = Some(sealed);
        let mut child = Child {
            handle: Handle(unsafe {
                OpenProcess(
                    PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    pid,
                )
            }),
            pid,
        };
        quit(&mut child)?;
        if unsafe { WaitForSingleObject(managed.0, WAIT.as_millis() as u32) } != WAIT_OBJECT_0 {
            bail!("Original managed server did not exit");
        }
        if http.request("/api/version", None).is_ok() {
            bail!("Original endpoint still responds");
        }
        Ok(original)
    }

    pub(super) fn restore(&mut self) -> Result<()> {
        let Some(sealed) = &self.sealed else {
            return Ok(());
        };
        if !tray().is_null() {
            bail!("Another Desktop appeared; original recovery file retained");
        }
        if Http::new("127.0.0.1:11434")?
            .request("/api/version", None)
            .is_ok()
        {
            bail!("Original endpoint is occupied; no restart attempted");
        }
        let container = gamepause::process_session::open(sealed)?;
        let startup: Startup = serde_json::from_str(&container.command_line)?;
        let child = spawn_raw(
            &self.exe,
            &format!("{} hidden --fast-startup", startup.launch.command_line),
            &startup.environment,
            Some(&startup.launch.directory),
        )?;
        let started = Instant::now();
        loop {
            if let Ok(bytes) = Http::new("127.0.0.1:11434")?.request("/api/version", None) {
                let version: serde_json::Value = serde_json::from_slice(&bytes)?;
                let mut owner = 0;
                unsafe { GetWindowThreadProcessId(tray(), &mut owner) };
                if version["version"] == VERSION && owner == child.id() {
                    break;
                }
            }
            if child.try_wait()?.is_some() || started.elapsed() >= WAIT {
                bail!("Original restart unverified; encrypted startup file retained");
            }
            thread::sleep(Duration::from_millis(100));
        }
        let resident: serde_json::Value =
            serde_json::from_slice(&Http::new("127.0.0.1:11434")?.request("/api/ps", None)?)?;
        if resident["models"].as_array().is_none_or(|m| !m.is_empty()) {
            bail!("Original empty residency changed during test");
        }
        self.sealed = None;
        fs::remove_file(&self.journal)?;
        Ok(())
    }
}
impl Drop for Original {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            eprintln!("Original Desktop recovery needs attention: {error}");
        }
    }
}
