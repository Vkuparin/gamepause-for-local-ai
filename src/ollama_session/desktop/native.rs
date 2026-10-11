//! Bounded, control-thread-only native operations. Handles never enter the UI.
mod evidence;
mod startup;
use super::*;
use anyhow::Context;
use evidence::*;
use startup::Startup;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowThreadProcessId, PostMessageW, WM_COMMAND,
};

pub(in crate::ollama_session) struct Native;
impl Native {
    fn identify(&self, endpoint: &str) -> Result<(Instance, String)> {
        let window = tray()?;
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(window, &mut pid) };
        let app = Held::open(pid)?;
        let executable = app.path()?;
        if !crate::process_session::file_name(&executable).eq_ignore_ascii_case("ollama app.exe") {
            bail!("Desktop executable is not recognized");
        }
        let owners = listeners(endpoint)?;
        if owners.len() != 1 {
            bail!("Ollama endpoint ownership is ambiguous");
        }
        let server = Held::open(*owners.iter().next().unwrap())?;
        if !crate::discovery::same_path(&server.path()?, &server_path(&executable))
            || parent(server.identity.pid)? != app.identity.pid
            || server.identity.created_at < app.identity.created_at
        {
            bail!("Endpoint is not managed by this Ollama Desktop");
        }
        app.same_user()?;
        server.same_user()?;
        Ok((
            Instance {
                app: app.identity.clone(),
                server: Some(server.identity.clone()),
            },
            executable,
        ))
    }
    fn inspect(&self, snapshot: &Snapshot, endpoint: &str) -> Result<Started> {
        let pins = pin(&snapshot.original.executable)?;
        verify_hashes(&pins, &snapshot.original)?;
        let (instance, executable) = self.identify(endpoint)?;
        if !crate::discovery::same_path(&executable, &snapshot.original.executable) {
            bail!("Ollama Desktop executable changed; recovery retained");
        }
        verify_version(endpoint, &executable)?;
        let original: Startup =
            crate::process_session::open_value(&snapshot.original.sealed_startup)?;
        original.validate()?;
        startup::verify_command(&original.launch.command_line, &snapshot.original.executable)?;
        if startup::configuration(&startup::environment(
            instance.server.as_ref().unwrap().pid,
        )?)? != original.server_configuration
        {
            bail!("Ollama server configuration changed; recovery retained");
        }
        Ok(Started {
            external: snapshot.external_restart || instance.app != snapshot.current.app,
            instance,
        })
    }
}
impl Control for Native {
    fn capture(&mut self, endpoint: &str, version: &str) -> Result<Eligibility> {
        if version != SUPPORTED_VERSION {
            return Ok(Eligibility::Fallback(
                "Desktop protection unavailable for this Ollama version; models are unloaded only."
                    .into(),
            ));
        }
        let mut stage = "process and endpoint ownership";
        let capture = (|| -> Result<Capture> {
            let (instance, executable) = self.identify(endpoint)?;
            stage = "executable access";
            let pins = pin(&executable)?;
            stage = "Desktop and server version";
            verify_version(endpoint, &executable)?;
            stage = "startup environment and arguments";
            let startup = Startup::capture(&instance, &executable)?;
            let sealed_startup = crate::process_session::seal_value(&startup)?;
            let (again, path) = self.identify(endpoint)?;
            if again != instance || !crate::discovery::same_path(&path, &executable) {
                bail!("Desktop changed during capture");
            }
            Ok(Capture {
                version: SUPPORTED_VERSION.into(),
                source_revision: SOURCE_REVISION.into(),
                executable,
                executable_sha256: hash(&pins[0])?,
                server_sha256: hash(&pins[1])?,
                sealed_startup,
                instance,
            })
        })();
        // Evidence failures before any lifecycle obligation retain ordinary unloading.
        Ok(match capture {
            Ok(capture) => Eligibility::Supported(capture),
            Err(_) => Eligibility::Fallback(format!(
                "Desktop protection unavailable: {stage} could not be verified; models are unloaded only."
            )),
        })
    }
    fn request_stop(&mut self, snapshot: &Snapshot, endpoint: &str) -> Result<()> {
        if snapshot.external_restart {
            bail!("Externally restarted Desktop was left running; protection incomplete");
        }
        if gone(&snapshot.current.app)? {
            return self.verify_stopped(snapshot, endpoint);
        }
        let observed = self.inspect(snapshot, endpoint)?;
        if observed.instance != snapshot.current || observed.external {
            bail!("Desktop instance changed; no shutdown requested");
        }
        // Keep both original process handles and both locked files until dispatch.
        let app = Held::matching(&snapshot.current.app)?;
        let server = Held::matching(
            snapshot
                .current
                .server
                .as_ref()
                .context("No managed server")?,
        )?;
        let pins = pin(&snapshot.original.executable)?;
        verify_hashes(&pins, &snapshot.original)?;
        let window = tray()?;
        let mut owner = 0;
        unsafe { GetWindowThreadProcessId(window, &mut owner) };
        if owner != app.identity.pid || gone(&server.identity)? {
            bail!("Desktop ownership changed before shutdown");
        }
        if unsafe { PostMessageW(window, WM_COMMAND, 9, 0) } == 0 {
            bail!("Ollama Desktop refused its native Quit command");
        }
        app.wait(10_000)?;
        server.wait(5_000)?;
        Ok(())
    }
    fn verify_stopped(&mut self, snapshot: &Snapshot, endpoint: &str) -> Result<()> {
        if !gone(&snapshot.current.app)?
            || !gone(
                snapshot
                    .current
                    .server
                    .as_ref()
                    .context("No managed server")?,
            )?
            || !listeners(endpoint)?.is_empty()
        {
            bail!("Desktop or its managed endpoint remains running; protection incomplete");
        }
        Ok(())
    }
    fn start(&mut self, snapshot: &Snapshot, endpoint: &str) -> Result<Started> {
        let pins = pin(&snapshot.original.executable)?;
        verify_hashes(&pins, &snapshot.original)?;
        if !listeners(endpoint)?.is_empty() {
            return self.inspect(snapshot, endpoint);
        }
        // A live matching process may still be starting. Never duplicate it.
        if !crate::process_session::Os::running(
            &mut crate::process_session::Windows,
            std::slice::from_ref(&snapshot.original.executable),
        )?
        .is_empty()
            || !no_tray()?
            || !no_servers()?
        {
            bail!(
                "Ollama Desktop is already running but its endpoint is not ready; recovery retained"
            );
        }
        let startup: Startup =
            crate::process_session::open_value(&snapshot.original.sealed_startup)?;
        startup.validate()?;
        let app = startup.spawn(&snapshot.original.executable)?;
        Ok(Started {
            instance: Instance { app, server: None },
            external: false,
        })
    }
    fn verify_ready(&mut self, snapshot: &Snapshot, endpoint: &str) -> Result<Started> {
        self.inspect(snapshot, endpoint)
    }
}
fn verify_version(endpoint: &str, executable: &str) -> Result<()> {
    use crate::ollama_session::Transport;
    let version: serde_json::Value = serde_json::from_slice(
        &crate::ollama_session::Http::new(endpoint)?.request("/api/version", None)?,
    )?;
    let output = crate::lmstudio::run_command(
        executable,
        &["--version"],
        std::time::Duration::from_secs(3),
    )?;
    if version["version"] != SUPPORTED_VERSION
        || String::from_utf8_lossy(&output).trim() != SUPPORTED_VERSION
    {
        bail!("Ollama Desktop version is not verified");
    }
    Ok(())
}
