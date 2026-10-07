//! Shell integration behind the tray commands: start-with-Windows registration
//! and opening the data folder.

use crate::{app::SharedState, ui_commands::Command, wide};
use anyhow::Result;
use std::{
    path::Path,
    ptr::{null, null_mut},
};
use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
use winreg::{RegKey, enums::HKEY_CURRENT_USER};
pub(super) const STARTUP_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub fn startup_enabled() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(STARTUP_KEY)
        .and_then(|k| k.get_value::<String, _>("GamePause"))
        .is_ok()
}
pub fn startup_command(executable: &Path, folder: &Path) -> String {
    format!(
        "\"{}\" --background --data-dir \"{}\"",
        executable.display(),
        folder.display()
    )
}
pub fn set_startup(enabled: bool, folder: &Path) -> Result<()> {
    let (root, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(STARTUP_KEY)?;
    if enabled {
        root.set_value(
            "GamePause",
            &startup_command(&std::env::current_exe()?, folder),
        )?;
    } else {
        match root.delete_value("GamePause") {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
/// Pure: classify a `ShellExecuteW` return value (an `HINSTANCE`,
/// `*mut c_void`). Per the Win32 docs a successful call returns a handle whose
/// value is greater than 32; `0` and the range `1..=32` are documented failure
/// codes (S_OK, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ...). This is the
/// testable core of "open_path surfaces a visible error on failure" (P1-7).
#[must_use]
pub fn shell_execute_failed(return_value: *mut core::ffi::c_void) -> bool {
    return_value as isize <= 32
}
/// Open a folder in the default file manager. A `ShellExecuteW` failure is
/// surfaced as a visible error instead of being silently swallowed (P1-7).
pub fn open_path(path: &Path) -> Result<()> {
    let operation = wide("open");
    let display = path.to_string_lossy();
    let path_w = wide(&display);
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            operation.as_ptr(),
            path_w.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    if shell_execute_failed(result) {
        anyhow::bail!(
            "Could not open this folder: {display} (shell code {})",
            result as isize
        );
    }
    Ok(())
}
pub fn request_startup(state: &SharedState, enabled: bool, folder: &Path) {
    if !crate::ui_commands::allowed(state, Command::Startup) {
        return;
    }
    use crate::commands::Outcome;
    if startup_enabled() == enabled {
        crate::app::local_result(
            state,
            Outcome::NoChange,
            "Windows startup preference unchanged.",
        );
        return;
    }
    match set_startup(enabled, folder) {
        Ok(()) => crate::app::local_result(
            state,
            Outcome::Completed,
            if enabled {
                "Start with Windows enabled."
            } else {
                "Start with Windows disabled."
            },
        ),
        Err(error) => crate::app::local_result(
            state,
            Outcome::Failed,
            format!("Could not save Windows startup preference: {error:#}"),
        ),
    }
}
pub fn request_folder(state: &SharedState, folder: &Path) {
    if !crate::ui_commands::allowed(state, Command::OpenFolder) {
        return;
    }
    match open_path(folder) {
        Ok(()) => crate::app::local_result(
            state,
            crate::commands::Outcome::Completed,
            "Logs and status folder opened.",
        ),
        Err(error) => crate::app::local_result(
            state,
            crate::commands::Outcome::Failed,
            format!("Could not open logs folder: {error:#}"),
        ),
    }
}
