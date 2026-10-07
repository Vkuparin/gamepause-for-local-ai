//! Win32 process enumeration, graceful and forced stop, and relaunch for the
//! process provider. Runs on the control thread only.

use super::{Found, Launch};
use anyhow::{Context, Result, bail};
use std::ffi::c_void;
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_NO_MORE_FILES, FILETIME, GetLastError, HANDLE, HWND,
        INVALID_HANDLE_VALUE, LPARAM, WAIT_OBJECT_0,
    },
    System::{
        Diagnostics::{
            Debug::ReadProcessMemory,
            ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
        },
        Threading::{
            CREATE_NEW_CONSOLE, CreateProcessW, GetProcessTimes, OpenProcess, PROCESS_INFORMATION,
            PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            PROCESS_TERMINATE, PROCESS_VM_READ, QueryFullProcessImageNameW, STARTF_USESHOWWINDOW,
            STARTUPINFOW, TerminateProcess, WaitForSingleObject,
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
fn image(handle: &Handle) -> Result<String> {
    let mut buffer = vec![0u16; 32_768];
    let mut length = buffer.len() as u32;
    if unsafe { QueryFullProcessImageNameW(handle.0, 0, buffer.as_mut_ptr(), &mut length) } == 0 {
        bail!("Process executable unavailable; residency is unknown");
    }
    Ok(String::from_utf16_lossy(&buffer[..length as usize]))
}
fn created_at(handle: &Handle) -> Result<u64> {
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exit = created;
    let mut kernel = created;
    let mut user = created;
    if unsafe { GetProcessTimes(handle.0, &mut created, &mut exit, &mut kernel, &mut user) } == 0 {
        bail!("Process identity unavailable; residency is unknown");
    }
    Ok(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
}
fn verify_identity(handle: &Handle, process: &Found) -> Result<()> {
    if created_at(handle)? != process.created_at
        || !crate::discovery::same_path(&image(handle)?, &process.path)
    {
        bail!("Process identity changed; it was left alone");
    }
    Ok(())
}
pub(super) fn enumeration_finished(error: u32) -> Result<()> {
    if error != ERROR_NO_MORE_FILES {
        bail!("Process enumeration incomplete; residency is unknown");
    }
    Ok(())
}
pub fn running(paths: &[String]) -> Result<Vec<Found>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
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
        if names.contains(&name) {
            let handle = open(entry.th32ProcessID, PROCESS_QUERY_LIMITED_INFORMATION)
                .context("Matching process unavailable; residency is unknown")?;
            let path = image(&handle)?;
            if wanted.contains(&crate::discovery::canonical(&path)) {
                if found.len() >= super::MAX_PROCESSES {
                    bail!("Too many matching processes; residency is unknown");
                }
                found.push(Found {
                    pid: entry.th32ProcessID,
                    created_at: created_at(&handle)?,
                    parent: entry.th32ParentProcessID,
                    path,
                });
            }
        }
        more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    enumeration_finished(unsafe { GetLastError() })?;
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
pub fn launch_details(process: &Found) -> Result<Launch> {
    let handle = open(process.pid, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)?;
    verify_identity(&handle, process)?;
    launch_from_handle(&handle)
}
fn launch_from_handle(handle: &Handle) -> Result<Launch> {
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
        directory: text(handle.0, parameters + 0x38)?,
    })
}
/// The process to close and how many of its windows were asked.
struct Closing {
    pid: u32,
    asked: u32,
}
unsafe extern "system" fn close_window(window: HWND, closing: LPARAM) -> i32 {
    let mut owner = 0u32;
    unsafe {
        // Valid for the synchronous EnumWindows call that passed it.
        let closing = &mut *(closing as *mut Closing);
        GetWindowThreadProcessId(window, &mut owner);
        if owner == closing.pid && PostMessageW(window, WM_CLOSE, 0, 0) != 0 {
            closing.asked += 1;
        }
    }
    1
}
pub fn stop(process: &Found, expected: Option<&Launch>) -> Result<()> {
    let access = PROCESS_SYNCHRONIZE
        | PROCESS_TERMINATE
        | PROCESS_QUERY_LIMITED_INFORMATION
        | if expected.is_some() {
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ
        } else {
            0
        };
    let handle = open(process.pid, access)?;
    verify_identity(&handle, process)?;
    if let Some(expected) = expected
        && !super::same_launch(&launch_from_handle(&handle)?, expected)
    {
        bail!("Process start details changed; it was left running");
    }
    if unsafe { WaitForSingleObject(handle.0, 0) } == WAIT_OBJECT_0 {
        return Ok(());
    }
    let mut closing = Closing {
        pid: process.pid,
        asked: 0,
    };
    unsafe {
        EnumWindows(Some(close_window), &raw mut closing as LPARAM);
        // A console server owns no window (its console belongs to the
        // host), so no close request can reach it: do not wait for one.
        if closing.asked != 0 && WaitForSingleObject(handle.0, 3_000) == WAIT_OBJECT_0 {
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
