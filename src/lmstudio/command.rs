//! Runs the LM Studio CLI with a deadline and an output cap, and finishes
//! quietly when a descendant keeps the pipes open after the CLI itself exits.

use anyhow::{Context, Result, bail};
use std::{
    os::windows::process::CommandExt,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
pub fn run_command(program: &str, args: &[&str], timeout: Duration) -> Result<Vec<u8>> {
    use std::{io::Read, os::windows::io::AsRawHandle};
    use windows_sys::Win32::{Foundation::ERROR_BROKEN_PIPE, System::Pipes::PeekNamedPipe};
    const CAP: usize = 4 * 1024 * 1024;
    // Poll pipe availability on this thread: no unbounded reader joins or detached readers.
    fn drain(
        pipe: &mut (impl Read + AsRawHandle),
        bytes: &mut Vec<u8>,
        truncated: &mut bool,
        progress: &mut bool,
    ) -> Result<bool> {
        let mut available = 0;
        if unsafe {
            PeekNamedPipe(
                pipe.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        } == 0
        {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                return Ok(true);
            }
            return Err(error.into());
        }
        if available != 0 {
            let mut buffer = [0; 8192];
            let count = pipe.read(&mut buffer[..(available as usize).min(8192)])?;
            let retained = count.min(CAP.saturating_sub(bytes.len()));
            bytes.extend_from_slice(&buffer[..retained]);
            *truncated |= retained < count;
            *progress |= count != 0;
        }
        Ok(false)
    }
    let mut child = Command::new(program)
        .args(args)
        .creation_flags(0x08000000)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Unable to launch local command")?;
    let mut stdout = child.stdout.take().context("Command stdout unavailable")?;
    let mut stderr = child.stderr.take().context("Command stderr unavailable")?;
    let deadline = Instant::now() + timeout;
    let result = (|| {
        let (mut output, mut errors) = (Vec::new(), Vec::new());
        let (mut out_done, mut err_done, mut truncated) = (false, false, false);
        // Set when the command itself has exited and its pipes were last busy.
        let mut exited_quiet: Option<Instant> = None;
        loop {
            if Instant::now() >= deadline {
                bail!("Local command timed out (including pipe completion)");
            }
            let mut progress = false;
            if !out_done {
                out_done = drain(&mut stdout, &mut output, &mut truncated, &mut progress)?;
            }
            if !err_done {
                err_done = drain(&mut stderr, &mut errors, &mut truncated, &mut progress)?;
            }
            let status = child.try_wait()?;
            // `lms` can start LM Studio, which inherits these pipes and never
            // closes them. Everything the command wrote is already buffered,
            // so once it has exited and the pipes stay quiet they count as done.
            if status.is_some() {
                if progress {
                    exited_quiet = None;
                } else if exited_quiet.get_or_insert_with(Instant::now).elapsed()
                    >= Duration::from_millis(250)
                {
                    out_done = true;
                    err_done = true;
                }
            }
            if let Some(status) = status
                && out_done
                && err_done
            {
                if truncated {
                    bail!("Local command output exceeded 4 MiB and was truncated");
                }
                if !status.success() {
                    bail!("Local command failed: {}", String::from_utf8_lossy(&errors));
                }
                return Ok(output);
            }
            // Keep draining while output flows; idle waits are coarse so a
            // slow command is not polled hundreds of times a second.
            if !progress {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}
