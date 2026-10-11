use super::*;
use crate::process_session::{Launch, Os, Windows};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ffi::c_void};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, LocalFree},
    System::{Diagnostics::Debug::ReadProcessMemory, Threading::*},
    UI::Shell::CommandLineToArgvW,
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
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Startup {
    pub launch: Launch,
    pub environment: Vec<u16>,
    pub server_configuration: BTreeMap<String, String>,
}
impl Startup {
    pub fn capture(instance: &Instance, executable: &str) -> Result<Self> {
        let process = Windows
            .running(&[executable.into()])?
            .into_iter()
            .find(|process| process.pid == instance.app.pid)
            .context("Desktop startup identity is unavailable")?;
        let launch = Windows.launch_details(&process)?;
        verify_command(&launch.command_line, executable)?;
        let environment = environment(instance.app.pid)?;
        let server_configuration = configuration(&self::environment(
            instance.server.as_ref().context("No managed server")?.pid,
        )?)?;
        if self::environment(instance.app.pid)? != environment {
            bail!("Desktop environment changed during capture");
        }
        let startup = Self {
            launch,
            environment,
            server_configuration,
        };
        startup.validate()?;
        Ok(startup)
    }
    pub fn validate(&self) -> Result<()> {
        if self.environment.len() > 128 * 1024
            || self.environment.len() < 2
            || !self.environment.ends_with(&[0, 0])
            || self.launch.directory.is_empty()
            || self.launch.directory.contains('\0')
            || self.launch.directory.len() > 32768
            || self.server_configuration.len() > 1024
        {
            bail!("Unsupported Desktop startup recovery; source retained");
        }
        configuration(&self.environment)?;
        Ok(())
    }
    pub fn spawn(&self, executable: &str) -> Result<Process> {
        verify_command(&self.launch.command_line, executable)?;
        let mut command = crate::wide(&format!("\"{executable}\" hidden --fast-startup"));
        let application = crate::wide(executable);
        let directory = crate::wide(&self.launch.directory);
        let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe {
            CreateProcessW(
                application.as_ptr(),
                command.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
                self.environment.as_ptr().cast(),
                directory.as_ptr(),
                &startup,
                &mut process,
            )
        } == 0
        {
            bail!("Cannot restart Ollama Desktop; recovery retained");
        }
        let _handle = Handle(process.hProcess);
        unsafe { CloseHandle(process.hThread) };
        Ok(Held::open(process.dwProcessId)?.identity)
    }
}
pub(super) fn configuration(environment: &[u16]) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for entry in environment
        .split(|unit| *unit == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry = String::from_utf16(entry).context("Invalid Desktop environment")?;
        let Some((key, value)) = entry.split_once('=') else {
            bail!("Invalid Desktop environment entry");
        };
        let key = key.to_ascii_uppercase();
        if (key.starts_with("OLLAMA_")
            || key.starts_with("CUDA_")
            || key.starts_with("HIP_")
            || key.starts_with("GGML_")
            || key.starts_with("VK_")
            || key.starts_with("ROCR_")
            || key.starts_with("HSA_")
            || matches!(
                key.as_str(),
                "LOCALAPPDATA" | "APPDATA" | "USERPROFILE" | "HOME" | "PATH" | "GPU_DEVICE_ORDINAL"
            ))
            && result.insert(key, value.to_owned()).is_some()
        {
            bail!("Ambiguous Desktop environment");
        }
    }
    Ok(result)
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
        bail!("Cannot read Desktop startup environment");
    }
    Ok(value)
}
pub(super) fn environment(pid: u32) -> Result<Vec<u16>> {
    let raw = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid) };
    if raw.is_null() {
        bail!("Cannot read Desktop startup environment");
    }
    let handle = Handle(raw);
    let mut basic = [0usize; 6];
    if unsafe {
        NtQueryInformationProcess(
            raw,
            0,
            basic.as_mut_ptr().cast(),
            size_of_val(&basic) as u32,
            std::ptr::null_mut(),
        )
    } != 0
    {
        bail!("Cannot query Desktop process parameters");
    }
    let parameters = pointer(handle.0, basic[1] + 0x20)?;
    let address = pointer(handle.0, parameters + 0x80)?;
    let mut result = Vec::new();
    while result.len() < 128 * 1024 {
        let next = address + result.len() * 2;
        let units = ((4096 - (next % 4096)) / 2).min(256);
        let mut chunk = vec![0u16; units];
        let mut done = 0;
        if unsafe {
            ReadProcessMemory(
                raw,
                next as *const c_void,
                chunk.as_mut_ptr().cast(),
                units * 2,
                &mut done,
            )
        } == 0
            || done != units * 2
        {
            bail!("Desktop environment is incomplete; shutdown refused");
        }
        for unit in chunk {
            let finished = unit == 0 && result.last() == Some(&0);
            result.push(unit);
            if finished {
                return Ok(result);
            }
        }
    }
    bail!("Desktop environment exceeds capture bounds")
}

pub(super) fn verify_command(command: &str, executable: &str) -> Result<()> {
    let mut count = 0;
    let args = unsafe { CommandLineToArgvW(crate::wide(command).as_ptr(), &mut count) };
    if args.is_null() {
        bail!("Desktop startup arguments are not recognized");
    }
    struct Arguments(*mut *mut u16);
    impl Drop for Arguments {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0.cast()) };
        }
    }
    let args = Arguments(args);
    if !(1..=8).contains(&count) {
        bail!("Desktop startup arguments exceed bounds");
    }
    let mut values = Vec::new();
    for index in 0..count as usize {
        let ptr = unsafe { *args.0.add(index) };
        let mut length = 0;
        while length < 32768 && unsafe { *ptr.add(length) } != 0 {
            length += 1;
        }
        if length == 32768 {
            bail!("Desktop startup argument exceeds bounds");
        }
        values.push(String::from_utf16(unsafe {
            std::slice::from_raw_parts(ptr, length)
        })?);
    }
    if !crate::discovery::same_path(&values[0], executable)
        || values[1..]
            .iter()
            .any(|arg| !matches!(arg.as_str(), "hidden" | "--hide" | "-j" | "--fast-startup"))
    {
        bail!("Desktop startup arguments are not supported");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sealed_command_cannot_be_redirected_to_another_executable() {
        let command = r#""C:\Fixture\Ollama\ollama app.exe" hidden --fast-startup"#;
        verify_command(command, r"C:\Fixture\Ollama\ollama app.exe").unwrap();
        assert!(verify_command(command, r"D:\Other\ollama app.exe").is_err());
        assert!(
            verify_command(
                r#""C:\Fixture\Ollama\ollama app.exe" --unreviewed"#,
                r"C:\Fixture\Ollama\ollama app.exe"
            )
            .is_err()
        );
    }
    #[test]
    fn server_configuration_normalizes_order_and_detects_drift() {
        let first = "OLLAMA_HOST=127.0.0.1:1234\0CUDA_VISIBLE_DEVICES=0\0IRRELEVANT=one\0\0"
            .encode_utf16()
            .collect::<Vec<_>>();
        let same = "irrelevant=two\0cuda_visible_devices=0\0ollama_host=127.0.0.1:1234\0\0"
            .encode_utf16()
            .collect::<Vec<_>>();
        assert_eq!(
            configuration(&first).unwrap(),
            configuration(&same).unwrap()
        );
        let drift = "OLLAMA_HOST=127.0.0.1:4321\0CUDA_VISIBLE_DEVICES=0\0\0"
            .encode_utf16()
            .collect::<Vec<_>>();
        assert_ne!(
            configuration(&first).unwrap(),
            configuration(&drift).unwrap()
        );
        let ambiguous = "OLLAMA_HOST=a\0ollama_host=b\0\0"
            .encode_utf16()
            .collect::<Vec<_>>();
        assert!(configuration(&ambiguous).is_err());
    }
}
