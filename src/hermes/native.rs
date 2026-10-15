use super::Process;
use anyhow::{Context, Result, bail};
use std::{collections::BTreeSet, fs::File, io::Read};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, GetLastError, HANDLE, WAIT_OBJECT_0,
    },
    NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
        TCP_TABLE_OWNER_PID_LISTENER,
    },
    Networking::WinSock::{AF_INET, AF_INET6},
    Security::{
        Cryptography::*, EqualSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    System::{RemoteDesktop::ProcessIdToSessionId, Threading::*},
};
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationProcess(
        process: HANDLE,
        class: u32,
        information: *mut std::ffi::c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}
/// Reads only the data-directory variable from a bounded native environment.
/// Neither the environment nor other variables are returned, logged or saved.
pub(super) fn environment_home(identity: &Process) -> Result<Option<String>> {
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    let raw = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, identity.pid) };
    if raw.is_null() {
        bail!("Cannot verify Hermes backend data directory");
    }
    let handle = Handle(raw);
    if Held::open(identity.pid)?.identity != *identity {
        bail!("Hermes backend identity changed");
    }
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
        bail!("Cannot read Hermes backend identity");
    }
    fn pointer(handle: HANDLE, address: usize) -> Result<usize> {
        let mut value = 0usize;
        let mut read = 0usize;
        if unsafe {
            ReadProcessMemory(
                handle,
                address as *const _,
                (&raw mut value).cast(),
                size_of::<usize>(),
                &mut read,
            )
        } == 0
            || read != size_of::<usize>()
        {
            bail!("Cannot read Hermes backend data directory");
        }
        Ok(value)
    }
    let parameters = pointer(handle.0, basic[1] + 0x20)?;
    let environment = pointer(handle.0, parameters + 0x80)?;
    let mut entry = Vec::new();
    let mut found = None;
    for index in 0..65536 {
        let mut unit = 0u16;
        let mut read = 0usize;
        if unsafe {
            ReadProcessMemory(
                handle.0,
                (environment + index * 2) as *const _,
                (&raw mut unit).cast(),
                2,
                &mut read,
            )
        } == 0
            || read != 2
        {
            bail!("Hermes environment cannot be verified");
        }
        if unit == 0 {
            if entry.is_empty() {
                return Ok(found);
            }
            let text = String::from_utf16(&entry).context("Invalid Hermes environment encoding")?;
            if let Some((name, value)) = text.split_once('=')
                && name.eq_ignore_ascii_case("HERMES_HOME")
            {
                if found.is_some() {
                    bail!("Ambiguous Hermes data directory");
                }
                found = Some(value.to_owned());
            }
            entry.clear();
        } else {
            entry.push(unit);
        }
    }
    bail!("Hermes environment exceeds verification bounds")
}
pub(super) struct Handle(pub HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}
pub(super) struct Held {
    pub handle: Handle,
    pub identity: Process,
}
impl Held {
    /// Windows may retain an exited process object while another handle is open.
    /// Its PID can still be opened, but its executable path is no longer readable.
    pub fn live_recorded(pid: u32, start_time: u64) -> Result<Option<Self>> {
        match Self::open(pid) {
            Ok(held) => {
                if unsafe { WaitForSingleObject(held.handle.0, 0) } == WAIT_OBJECT_0 {
                    return Ok(None);
                }
                Self::recorded(pid, start_time).map(Some)
            }
            Err(error) => {
                if gone(&Process { pid, start_time: 0 })? {
                    Ok(None)
                } else {
                    Err(error)
                }
            }
        }
    }
    pub fn recorded(pid: u32, start_time: u64) -> Result<Self> {
        let held = Self::open(pid)?;
        let unix = held
            .identity
            .start_time
            .checked_sub(116_444_736_000_000_000)
            .context("Invalid Hermes creation time")?;
        // Hermes quantizes psutil's epoch time to centiseconds.
        let centiseconds = (unix + 50_000) / 100_000;
        if start_time == 0 || centiseconds.abs_diff(start_time) > 1 {
            bail!("Hermes owner record is stale; control refused");
        }
        held.same_user()?;
        Ok(held)
    }
    pub fn open(pid: u32) -> Result<Self> {
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if raw.is_null() {
            bail!("Cannot verify Hermes process identity");
        }
        let handle = Handle(raw);
        let mut created: FILETIME = unsafe { std::mem::zeroed() };
        let mut other = [created; 3];
        if unsafe {
            GetProcessTimes(
                raw,
                &mut created,
                &mut other[0],
                &mut other[1],
                &mut other[2],
            )
        } == 0
        {
            bail!("Cannot read Hermes process creation time");
        }
        Ok(Self {
            handle,
            identity: Process {
                pid,
                start_time: (u64::from(created.dwHighDateTime) << 32)
                    | u64::from(created.dwLowDateTime),
            },
        })
    }
    pub fn matching(identity: &Process) -> Result<Self> {
        let held = Self::open(identity.pid)?;
        if &held.identity != identity {
            bail!("Hermes process identity changed; recovery retained");
        }
        Ok(held)
    }
    pub fn path(&self) -> Result<String> {
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        if unsafe { QueryFullProcessImageNameW(self.handle.0, 0, path.as_mut_ptr(), &mut length) }
            == 0
        {
            bail!("Cannot verify Hermes executable path");
        }
        String::from_utf16(&path[..length as usize]).context("Invalid Hermes executable path")
    }
    pub fn same_user(&self) -> Result<()> {
        fn token(process: HANDLE) -> Result<Handle> {
            let mut handle = std::ptr::null_mut();
            if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut handle) } == 0 {
                bail!("Cannot verify process account");
            }
            Ok(Handle(handle))
        }
        fn user(token: &Handle) -> Result<Vec<usize>> {
            let mut bytes = 0;
            unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut bytes) };
            if bytes == 0 || bytes > 65536 {
                bail!("Invalid process account evidence");
            }
            let mut data = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
            if unsafe {
                GetTokenInformation(
                    token.0,
                    TokenUser,
                    data.as_mut_ptr().cast(),
                    bytes,
                    &mut bytes,
                )
            } == 0
            {
                bail!("Cannot read process account");
            }
            Ok(data)
        }
        let ours = user(&token(unsafe { GetCurrentProcess() })?)?;
        let theirs = user(&token(self.handle.0)?)?;
        let ours = unsafe { &*ours.as_ptr().cast::<TOKEN_USER>() };
        let theirs = unsafe { &*theirs.as_ptr().cast::<TOKEN_USER>() };
        let mut our_session = 0;
        let mut their_session = 0;
        if unsafe { EqualSid(ours.User.Sid, theirs.User.Sid) } == 0
            || unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut our_session) } == 0
            || unsafe { ProcessIdToSessionId(self.identity.pid, &mut their_session) } == 0
            || our_session != their_session
        {
            bail!("Hermes is owned by another user or desktop session");
        }
        Ok(())
    }
}
pub(super) fn gone(identity: &Process) -> Result<bool> {
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            identity.pid,
        )
    };
    if raw.is_null() {
        if unsafe { GetLastError() } == ERROR_INVALID_PARAMETER {
            return Ok(true);
        }
        bail!("Cannot establish whether Hermes has exited");
    }
    let held = Handle(raw);
    if unsafe { WaitForSingleObject(held.0, 0) } == WAIT_OBJECT_0 {
        return Ok(true);
    }
    // A reused PID proves the original exited, but never conveys authority over its replacement.
    Ok(Held::open(identity.pid)?.identity != *identity)
}
pub(super) fn tree(router: &Process, executable: &str) -> Result<Vec<Process>> {
    use crate::process_session::Os;
    let found = crate::process_session::Windows.running(&[executable.into()])?;
    if found.len() > super::MAX_MODELS {
        bail!("Hermes process tree exceeds bounds");
    }
    let mut members = BTreeSet::from([router.pid]);
    for _ in 0..found.len() {
        for child in &found {
            if members.contains(&child.parent) {
                members.insert(child.pid);
            }
        }
    }
    if found.iter().any(|p| !members.contains(&p.pid)) {
        bail!("An independently managed server shares the Hermes engine; control refused");
    }
    found
        .iter()
        .filter(|p| p.pid != router.pid)
        .map(|p| {
            let held = Held::open(p.pid)?;
            held.same_user()?;
            Ok(held.identity)
        })
        .collect()
}
pub(super) fn no_engine(executable: &str) -> Result<bool> {
    use crate::process_session::Os;
    Ok(crate::process_session::Windows
        .running(&[executable.into()])?
        .is_empty())
}
pub(super) fn hash_bytes(bytes: &[u8]) -> Result<String> {
    let mut output = [0u8; 32];
    if unsafe {
        BCryptHash(
            BCRYPT_SHA256_ALG_HANDLE,
            std::ptr::null(),
            0,
            bytes.as_ptr(),
            bytes.len() as u32,
            output.as_mut_ptr(),
            32,
        )
    } < 0
    {
        bail!("Cannot verify Hermes metadata digest");
    }
    Ok(output.iter().map(|b| format!("{b:02x}")).collect())
}
pub(super) fn listeners(endpoint: &str) -> Result<BTreeSet<u32>> {
    let endpoint = crate::config::normalized_endpoint(endpoint)?;
    let port: u16 = endpoint
        .rsplit_once(':')
        .context("Invalid endpoint")?
        .1
        .parse()?;
    let mut owners = BTreeSet::new();
    for family in [AF_INET, AF_INET6] {
        let mut bytes = 0;
        unsafe {
            GetExtendedTcpTable(
                std::ptr::null_mut(),
                &mut bytes,
                0,
                family as u32,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        let row_size = if family == AF_INET {
            size_of::<MIB_TCPROW_OWNER_PID>()
        } else {
            size_of::<MIB_TCP6ROW_OWNER_PID>()
        };
        for attempt in 0..2 {
            if !(4..=4 * 1024 * 1024).contains(&bytes) {
                bail!("TCP ownership evidence exceeds bounds");
            }
            let mut data = vec![0u32; (bytes as usize).div_ceil(4)];
            let status = unsafe {
                GetExtendedTcpTable(
                    data.as_mut_ptr().cast(),
                    &mut bytes,
                    0,
                    family as u32,
                    TCP_TABLE_OWNER_PID_LISTENER,
                    0,
                )
            };
            if status != 0 {
                if status == 122 && attempt == 0 {
                    continue;
                }
                bail!("Cannot verify Hermes endpoint ownership");
            }
            let count = data[0] as usize;
            if count > (bytes as usize).saturating_sub(4) / row_size {
                bail!("Invalid TCP ownership evidence");
            }
            for index in 0..count {
                let row = unsafe { data.as_ptr().cast::<u8>().add(4 + index * row_size) };
                let (local, pid) = if family == AF_INET {
                    let row =
                        unsafe { std::ptr::read_unaligned(row.cast::<MIB_TCPROW_OWNER_PID>()) };
                    (row.dwLocalPort, row.dwOwningPid)
                } else {
                    let row =
                        unsafe { std::ptr::read_unaligned(row.cast::<MIB_TCP6ROW_OWNER_PID>()) };
                    (row.dwLocalPort, row.dwOwningPid)
                };
                if (local as u16).to_be() == port {
                    owners.insert(pid);
                }
            }
            break;
        }
    }
    Ok(owners)
}
pub(super) fn hash(file: &File) -> Result<String> {
    let length = file.metadata()?.len();
    if length == 0 || length > 128 * 1024 * 1024 * 1024 {
        bail!("Hermes content exceeds verification bounds");
    }
    let mut handle = std::ptr::null_mut();
    if unsafe {
        BCryptCreateHash(
            BCRYPT_SHA256_ALG_HANDLE,
            &mut handle,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            0,
            0,
        )
    } < 0
    {
        bail!("Cannot create content identity hash");
    }
    struct Hash(BCRYPT_HASH_HANDLE);
    impl Drop for Hash {
        fn drop(&mut self) {
            unsafe { BCryptDestroyHash(self.0) };
        }
    }
    let handle = Hash(handle);
    // A cloned file shares its cursor; rewind before every bounded verification.
    use std::io::{Seek, SeekFrom};
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let mut buffer = vec![0u8; 1024 * 1024];
    let began = std::time::Instant::now();
    loop {
        if began.elapsed() > std::time::Duration::from_secs(90) {
            bail!("Hermes content verification timed out");
        }
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if unsafe { BCryptHashData(handle.0, buffer.as_ptr(), count as u32, 0) } < 0 {
            bail!("Cannot hash Hermes content");
        }
    }
    let mut digest = [0u8; 32];
    if unsafe { BCryptFinishHash(handle.0, digest.as_mut_ptr(), 32, 0) } < 0 {
        bail!("Cannot finish content identity hash");
    }
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}
