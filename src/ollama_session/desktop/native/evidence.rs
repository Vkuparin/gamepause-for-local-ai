use super::*;
use std::{collections::BTreeSet, fs::File, io::Read, os::windows::fs::OpenOptionsExt};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, FILETIME, GetLastError, HANDLE,
        HWND, INVALID_HANDLE_VALUE, LPARAM, WAIT_OBJECT_0,
    },
    NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
        TCP_TABLE_OWNER_PID_LISTENER,
    },
    Networking::WinSock::{AF_INET, AF_INET6},
    Security::{
        Cryptography::*, EqualSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::FILE_SHARE_READ,
    System::{Diagnostics::ToolHelp::*, RemoteDesktop::ProcessIdToSessionId, Threading::*},
    UI::WindowsAndMessaging::{EnumWindows, GetClassNameW},
};

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
    pub fn open(pid: u32) -> Result<Self> {
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if raw.is_null() {
            bail!("Cannot verify Ollama process identity");
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
            bail!("Cannot read Ollama process creation time");
        }
        Ok(Self {
            handle,
            identity: Process {
                pid,
                created_at: (u64::from(created.dwHighDateTime) << 32)
                    | u64::from(created.dwLowDateTime),
            },
        })
    }
    pub fn matching(identity: &Process) -> Result<Self> {
        let held = Self::open(identity.pid)?;
        if &held.identity != identity {
            bail!("Ollama process identity changed; recovery retained");
        }
        Ok(held)
    }
    pub fn path(&self) -> Result<String> {
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        if unsafe { QueryFullProcessImageNameW(self.handle.0, 0, path.as_mut_ptr(), &mut length) }
            == 0
        {
            bail!("Cannot verify Ollama executable path");
        }
        String::from_utf16(&path[..length as usize]).context("Invalid Ollama executable path")
    }
    pub fn wait(&self, milliseconds: u32) -> Result<()> {
        if unsafe { WaitForSingleObject(self.handle.0, milliseconds) } != WAIT_OBJECT_0 {
            bail!("Ollama process did not stop within its shutdown allowance");
        }
        Ok(())
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
            bail!("Ollama is owned by another user or desktop session");
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
        bail!("Cannot establish whether Ollama has exited");
    }
    let held = Handle(raw);
    if unsafe { WaitForSingleObject(held.0, 0) } == WAIT_OBJECT_0 {
        return Ok(true);
    }
    // A reused PID proves the original exited, but never conveys authority over its replacement.
    Ok(Held::open(identity.pid)?.identity != *identity)
}
pub(super) fn parent(pid: u32) -> Result<u32> {
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE {
        bail!("Cannot inspect managed server parent");
    }
    let held = Handle(raw);
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut more = unsafe { Process32FirstW(held.0, &mut entry) } != 0;
    for _ in 0..65536 {
        if !more {
            break;
        }
        if entry.th32ProcessID == pid {
            return Ok(entry.th32ParentProcessID);
        }
        more = unsafe { Process32NextW(held.0, &mut entry) } != 0;
    }
    bail!("Managed server parent is unverified")
}
pub(super) fn tray() -> Result<HWND> {
    let windows = tray_windows()?;
    if windows.len() != 1 {
        bail!("Ollama tray ownership is unavailable or ambiguous");
    }
    Ok(windows[0])
}
pub(super) fn no_tray() -> Result<bool> {
    Ok(tray_windows()?.is_empty())
}
fn tray_windows() -> Result<Vec<HWND>> {
    struct Windows {
        windows: Vec<HWND>,
        count: usize,
    }
    unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> i32 {
        let state = unsafe { &mut *(data as *mut Windows) };
        state.count += 1;
        if state.count > 65536 {
            return 0;
        }
        let mut class = [0u16; 64];
        let length = unsafe { GetClassNameW(window, class.as_mut_ptr(), class.len() as i32) };
        if length > 0
            && class[..length as usize] == "OllamaClass".encode_utf16().collect::<Vec<_>>()
        {
            state.windows.push(window);
        }
        1
    }
    let mut state = Windows {
        windows: vec![],
        count: 0,
    };
    if unsafe { EnumWindows(Some(visit), (&raw mut state) as LPARAM) } == 0 {
        bail!("Ollama tray ownership is unavailable or ambiguous");
    }
    Ok(state.windows)
}
/// Desktop's own startup can reap standalone servers. Refuse startup while any
/// Ollama executable exists, including one at another path/port/account.
pub(super) fn no_servers() -> Result<bool> {
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE {
        bail!("Cannot rule out independently managed Ollama servers");
    }
    let held = Handle(raw);
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut more = unsafe { Process32FirstW(held.0, &mut entry) } != 0;
    for _ in 0..65536 {
        if !more {
            if unsafe { GetLastError() } == ERROR_NO_MORE_FILES {
                return Ok(true);
            }
            bail!("Ollama process inventory is incomplete; restart refused");
        }
        let length = entry
            .szExeFile
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(entry.szExeFile.len());
        if String::from_utf16_lossy(&entry.szExeFile[..length]).eq_ignore_ascii_case("ollama.exe") {
            return Ok(false);
        }
        more = unsafe { Process32NextW(held.0, &mut entry) } != 0;
    }
    bail!("Ollama process inventory exceeds bounds")
}
/// Includes all IPv4/IPv6 interfaces at the port: an unrelated listener refuses control.
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
                bail!("Cannot verify Ollama endpoint ownership");
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
pub(super) fn server_path(executable: &str) -> String {
    std::path::Path::new(executable)
        .with_file_name("ollama.exe")
        .to_string_lossy()
        .into_owned()
}
pub(super) fn pin(executable: &str) -> Result<Vec<File>> {
    [executable.to_owned(), server_path(executable)]
        .iter()
        .map(|path| {
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(path)
                .context("Cannot lock Ollama executable identity")
        })
        .collect()
}
pub(super) fn hash(file: &File) -> Result<String> {
    let length = file.metadata()?.len();
    if length == 0 || length > 256 * 1024 * 1024 {
        bail!("Ollama executable exceeds verification bounds");
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
        bail!("Cannot create executable identity hash");
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
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if unsafe { BCryptHashData(handle.0, buffer.as_ptr(), count as u32, 0) } < 0 {
            bail!("Cannot hash Ollama executable");
        }
    }
    let mut digest = [0u8; 32];
    if unsafe { BCryptFinishHash(handle.0, digest.as_mut_ptr(), 32, 0) } < 0 {
        bail!("Cannot finish executable identity hash");
    }
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}
pub(super) fn verify_hashes(pins: &[File], original: &Capture) -> Result<()> {
    if hash(&pins[0])? != original.executable_sha256 || hash(&pins[1])? != original.server_sha256 {
        bail!("Ollama executable was replaced; restart refused and recovery retained");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_hash_rewinds_and_changed_content_is_refused() {
        let path = std::env::temp_dir().join(format!(
            "gamepause-hash-fixture-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"abc").unwrap();
        let file = File::open(&path).unwrap();
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(hash(&file).unwrap(), expected);
        assert_eq!(hash(&file).unwrap(), expected);
        drop(file);
        std::fs::write(&path, b"changed").unwrap();
        let file = File::open(&path).unwrap();
        let mut original = crate::ollama_session::tests::desktop_capture();
        original.executable_sha256 = expected.into();
        original.server_sha256 = expected.into();
        let pins = vec![file.try_clone().unwrap(), file];
        assert!(verify_hashes(&pins, &original).is_err());
        drop(pins);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn creation_time_guard_refuses_reused_pid_without_mutation() {
        let held = Held::open(std::process::id()).unwrap();
        held.same_user().unwrap();
        let mut wrong = held.identity.clone();
        wrong.created_at += 1;
        assert!(Held::matching(&wrong).is_err());
        assert!(gone(&wrong).unwrap());
        assert!(!gone(&held.identity).unwrap());
    }
}
