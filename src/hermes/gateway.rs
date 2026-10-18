//! Gateway-owner fallback. Only the verified gateway incarnation is stopped;
//! its full restart data remains encrypted for the current Windows account.
use super::{Gateway, native};
use crate::process_session::{Found, Launch, Os, Windows, open_value, seal_value};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Restart {
    launch: Launch,
    environment: Vec<u16>,
}
pub(super) fn owner(
    home: &Path,
    directory: &Path,
    expected: Option<(u32, u64)>,
) -> Result<Option<native::Held>> {
    let path = directory.join("host-gateway.json");
    if !path.exists() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        bail!("Hermes gateway record exceeds bounds");
    }
    let record: serde_json::Value =
        serde_json::from_slice(&bytes).context("Invalid Hermes gateway record")?;
    let pid = record["pid"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .context("Invalid Hermes gateway PID")?;
    let birth = record["startTime"]
        .as_u64()
        .context("Invalid Hermes gateway birth")?;
    if expected.is_some_and(|identity| identity != (pid, birth)) {
        return Ok(None);
    }
    let Some(held) = native::Held::live_recorded(pid, birth)? else {
        return Ok(None);
    };
    if record["role"] != "gateway"
        || record["protocolVersion"] != 1
        || record["profiles"] != serde_json::json!(["default"])
    {
        bail!("Hermes gateway requires a verified default-profile owner");
    }
    let recorded_home = record["home"]
        .as_str()
        .context("Hermes gateway data directory missing")?;
    if !crate::discovery::same_path(recorded_home, &home.to_string_lossy()) {
        return Ok(None);
    }
    let actual_home = native::environment_home(&held.identity)?
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("hermes")
        });
    if !crate::discovery::same_path(&actual_home.to_string_lossy(), &home.to_string_lossy()) {
        bail!("Hermes gateway data directory changed");
    }
    Ok(Some(held))
}
fn found(held: &native::Held) -> Result<Found> {
    Ok(Found {
        pid: held.identity.pid,
        created_at: held.identity.start_time,
        parent: 0,
        path: held.path()?,
    })
}
fn restart(held: &native::Held) -> Result<Restart> {
    let launch = Windows.launch_details(&found(held)?)?;
    // Parse with Windows' own quoting rules. Never log or persist the plain argv.
    let command = crate::wide(&launch.command_line);
    let mut count = 0;
    let argv =
        unsafe { windows_sys::Win32::UI::Shell::CommandLineToArgvW(command.as_ptr(), &mut count) };
    if argv.is_null() {
        bail!("Hermes gateway command cannot be verified");
    }
    let mut args = Vec::new();
    for index in 0..count {
        let value = unsafe { *argv.add(index as usize) };
        let mut length = 0;
        while unsafe { *value.add(length) } != 0 {
            length += 1;
        }
        args.push(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(value, length)
        }));
    }
    unsafe {
        windows_sys::Win32::Foundation::LocalFree(argv.cast());
    }
    if !args
        .windows(4)
        .any(|w| w == ["-m", "hermes_cli.main", "gateway", "run"])
        && !(args.first().is_some_and(|a| {
            crate::process_session::file_name(a).eq_ignore_ascii_case("hermes.exe")
        }) && args.get(1).is_some_and(|a| a == "gateway")
            && args.get(2).is_some_and(|a| a == "run"))
    {
        bail!("Hermes gateway launch is unsupported; left running");
    }
    Ok(Restart {
        launch,
        environment: native::environment_block(&held.identity)?,
    })
}
pub(super) fn verify_restart(home: &Path, directory: &Path, identity: (u32, u64)) -> Result<()> {
    let held = owner(home, directory, Some(identity))?.context("Hermes gateway owner changed")?;
    restart(&held)?;
    Ok(())
}
pub(super) fn capture(held: &native::Held, image: super::FileIdentity) -> Result<Gateway> {
    Ok(Gateway {
        process: held.identity.clone(),
        image,
        sealed: seal_value(&restart(held)?)?,
    })
}
fn matching(home: &Path, directory: &Path, saved: &Gateway) -> Result<Option<native::Held>> {
    let Some(held) = owner(home, directory, None)? else {
        return Ok(None);
    };
    if !crate::discovery::same_path(&held.path()?, &saved.image.path)
        || !same_launch(&open_value::<Restart>(&saved.sealed)?, &restart(&held)?)
    {
        bail!("Hermes gateway restart details changed; recovery retained");
    }
    Ok(Some(held))
}
fn same_launch(saved: &Restart, current: &Restart) -> bool {
    // Startup may amend PYTHONPATH and other process-local variables. We replay
    // the captured environment, but bind a running gateway to its verified home,
    // image, command and working directory rather than its mutable environment.
    saved.launch == current.launch
}
pub(super) fn stop(home: &Path, directory: &Path, saved: &Gateway) -> Result<()> {
    if let Some(held) = matching(home, directory, saved)? {
        let details: Restart = open_value(&saved.sealed)?;
        Windows.stop(&found(&held)?, Some(&details.launch))?;
    } else if !native::gone(&saved.process)? {
        bail!("Hermes gateway owner cannot be verified; left running");
    }
    Ok(())
}
pub(super) fn paused(home: &Path, directory: &Path, saved: &Gateway) -> Result<()> {
    if !native::gone(&saved.process)? || owner(home, directory, None)?.is_some() {
        bail!("Hermes gateway restarted during pause; recovery retained");
    }
    Ok(())
}
pub(super) fn start(home: &Path, directory: &Path, saved: &Gateway) -> Result<()> {
    if matching(home, directory, saved)?.is_some() {
        return Ok(());
    }
    let details: Restart = open_value(&saved.sealed)?;
    let candidates = Windows.running(std::slice::from_ref(&saved.image.path))?;
    if candidates.len() > 256 {
        bail!("Hermes gateway process inventory exceeds bounds");
    }
    for candidate in candidates {
        let launch = Windows.launch_details(&candidate)?;
        if launch == details.launch {
            bail!("Hermes gateway is starting without verified ownership; retry recovery shortly");
        }
    }
    launch(&saved.image.path, &details)?;
    let began = Instant::now();
    while began.elapsed() < Duration::from_secs(30) {
        if matching(home, directory, saved)?.is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    bail!("Hermes gateway startup is unverified; recovery retained")
}
pub(super) fn restored(home: &Path, directory: &Path, saved: &Gateway) -> Result<()> {
    matching(home, directory, saved)?.context("Hermes gateway has not returned")?;
    Ok(())
}
fn launch(image: &str, saved: &Restart) -> Result<()> {
    use windows_sys::Win32::System::Threading::*;
    if saved.environment.len() > 65536 || !saved.environment.ends_with(&[0, 0]) {
        bail!("Invalid saved Hermes gateway environment");
    }
    let image = crate::wide(image);
    let mut command = crate::wide(&saved.launch.command_line);
    let directory = crate::wide(&saved.launch.directory);
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = size_of::<STARTUPINFOW>() as u32;
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe {
        CreateProcessW(
            image.as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            saved.environment.as_ptr().cast(),
            directory.as_ptr(),
            &startup,
            &mut process,
        )
    } == 0
    {
        bail!("Windows could not restart Hermes gateway");
    }
    drop(native::Handle(process.hThread));
    drop(native::Handle(process.hProcess));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_environment_and_command_are_sealed_together() {
        let saved = Restart {
            launch:Launch {command_line:r#""D:\Fixture\python.exe" -m hermes_cli.main gateway run --fixture-secret=private-value"#.into(),directory:r"D:\Fixture\".into()},
            environment:"HERMES_HOME=D:\\Fixture\0FIXTURE_SECRET=private-value\0\0".encode_utf16().collect(),
        };
        let sealed = seal_value(&saved).unwrap();
        assert!(!sealed.contains("private-value"));
        assert!(open_value::<Restart>(&sealed).unwrap() == saved);
    }
    #[test]
    fn startup_environment_changes_do_not_hide_a_valid_gateway_but_launch_changes_do() {
        let before = Restart {
            launch: Launch {
                command_line: r#""D:\Fixture\python.exe" -m hermes_cli.main gateway run"#.into(),
                directory: r"D:\Fixture\".into(),
            },
            environment: "HERMES_HOME=D:\\Fixture\0PYTHONPATH=before\0\0"
                .encode_utf16()
                .collect(),
        };
        let mut after = before.clone();
        after.environment = "HERMES_HOME=D:\\Fixture\0PYTHONPATH=after\0\0"
            .encode_utf16()
            .collect();
        assert!(same_launch(&before, &after));
        after.launch.command_line.push_str(" --different-launch");
        assert!(!same_launch(&before, &after));
        after.launch = before.launch.clone();
        after.launch.directory = r"D:\OtherFixture\".into();
        assert!(!same_launch(&before, &after));
    }
}
