//! Non-waiting, process-lifetime control claims, independent of data directories.
use crate::{config::normalized_endpoint, provider::Kind};
use anyhow::{Context, Result, bail};
use std::{
    collections::{BTreeMap, BTreeSet},
    os::windows::io::{FromRawHandle, OwnedHandle},
    sync::{Arc, Mutex},
};
use windows_sys::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, GetLastError, SetLastError},
    System::Threading::CreateMutexW,
};

pub type SharedClaims = Arc<Mutex<Claims>>;

pub struct Claims {
    held: BTreeMap<String, (String, OwnedHandle)>,
    namespace: String,
}
impl Default for Claims {
    fn default() -> Self {
        Self {
            held: BTreeMap::new(),
            namespace: "GamePause.Control.v1".into(),
        }
    }
}
impl Claims {
    #[cfg(test)]
    pub(crate) fn isolated() -> Self {
        Self {
            namespace: format!(
                "GamePause.Control.Fixture.{}.{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ),
            ..Default::default()
        }
    }
    /// Acquire the complete new set before releasing obsolete routes. Failure
    /// preserves previous claims and releases only newly acquired handles.
    pub fn claim(&mut self, owner: &str, kind: Kind, endpoints: &[&str]) -> Result<()> {
        let namespace = self.namespace.clone();
        self.claim_in(&namespace, owner, kind, endpoints)
    }
    fn claim_in(
        &mut self,
        namespace: &str,
        owner: &str,
        kind: Kind,
        endpoints: &[&str],
    ) -> Result<()> {
        if owner.is_empty() || endpoints.is_empty() {
            bail!("Provider control requires an identity and route");
        }
        let mut keys = BTreeSet::new();
        for endpoint in endpoints {
            keys.insert(format!("endpoint.{}", normalized_endpoint(endpoint)?));
        }
        // lms CLI selects its service independently of the configured API port.
        if kind == Kind::LMStudio {
            keys.insert("lmstudio-service".into());
        }
        let mut acquired = Vec::new();
        for key in &keys {
            if let Some((existing, _)) = self.held.get(key) {
                if existing != owner {
                    bail!(
                        "Provider route is already owned by another configured provider; control refused"
                    );
                }
            } else {
                acquired.push((
                    key.clone(),
                    create_claim(&format!("Global\\{namespace}.{key}"))?,
                ));
            }
        }
        for (key, handle) in acquired {
            self.held.insert(key, (owner.into(), handle));
        }
        self.held
            .retain(|key, (existing, _)| existing != owner || keys.contains(key));
        Ok(())
    }
}
fn create_claim(name: &str) -> Result<OwnedHandle> {
    let name = crate::wide(name);
    // Use named-object existence, not thread ownership. This lets the handle
    // move with the bounded worker; there is no wait or ReleaseMutex call.
    let raw = unsafe {
        SetLastError(0);
        CreateMutexW(std::ptr::null(), 0, name.as_ptr())
    };
    let error = unsafe { GetLastError() };
    if raw.is_null() {
        return Err(std::io::Error::from_raw_os_error(error as i32))
            .context("Provider ownership could not be established; control refused");
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    if error == ERROR_ALREADY_EXISTS {
        bail!(
            "Another GamePause instance owns this provider route; control refused. Use its data directory or quit it first"
        );
    }
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    fn namespace() -> String {
        format!(
            "GamePause.Control.Fixture.{}.{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        )
    }
    #[test]
    fn aliases_and_provider_ids_share_endpoint_ownership() {
        let ns = namespace();
        let mut first = Claims::default();
        first
            .claim_in(&ns, "a", Kind::Ollama, &["localhost:01234"])
            .unwrap();
        first
            .claim_in(&ns, "a", Kind::Ollama, &["127.0.0.1:1234"])
            .unwrap();
        assert!(
            first
                .claim_in(&ns, "b", Kind::Ollama, &["127.0.0.1:1234"])
                .is_err()
        );
        let mut second = Claims::default();
        assert!(
            second
                .claim_in(&ns, "c", Kind::LMStudio, &["127.0.0.1:1234"])
                .is_err()
        );
        drop(first);
        second
            .claim_in(&ns, "c", Kind::LMStudio, &["127.0.0.1:1234"])
            .unwrap();
    }
    #[test]
    fn failed_multi_route_claim_rolls_back_and_preserves_previous_routes() {
        let ns = namespace();
        let mut first = Claims::default();
        let mut second = Claims::default();
        first
            .claim_in(&ns, "a", Kind::Ollama, &["localhost:1234"])
            .unwrap();
        second
            .claim_in(&ns, "b", Kind::Ollama, &["localhost:3456"])
            .unwrap();
        assert!(
            first
                .claim_in(
                    &ns,
                    "a",
                    Kind::Ollama,
                    &["localhost:2345", "localhost:3456"]
                )
                .is_err()
        );
        assert!(
            second
                .claim_in(&ns, "b", Kind::Ollama, &["localhost:1234"])
                .is_err()
        );
        second
            .claim_in(&ns, "c", Kind::Ollama, &["localhost:2345"])
            .unwrap();
        first
            .claim_in(&ns, "a", Kind::Ollama, &["localhost:4567"])
            .unwrap();
        second
            .claim_in(&ns, "d", Kind::Ollama, &["localhost:1234"])
            .unwrap();
    }
    #[test]
    fn lm_cli_service_is_exclusive_even_with_different_ports() {
        let ns = namespace();
        let mut first = Claims::default();
        first
            .claim_in(
                &ns,
                "lm",
                Kind::LMStudio,
                &["localhost:1234", "localhost:4321"],
            )
            .unwrap();
        let mut second = Claims::default();
        assert!(
            second
                .claim_in(&ns, "other-lm", Kind::LMStudio, &["localhost:5555"])
                .is_err()
        );
        second
            .claim_in(&ns, "ollama", Kind::Ollama, &["localhost:5555"])
            .unwrap();
        assert!(
            second
                .claim_in(&ns, "other", Kind::Ollama, &["localhost:4321"])
                .is_err()
        );
    }
    #[test]
    fn simultaneous_claims_admit_one_owner_and_handles_move_between_threads() {
        let ns = namespace();
        let start = Arc::new(std::sync::Barrier::new(3));
        let finish = Arc::new(std::sync::Barrier::new(3));
        let workers: Vec<_> = (0..2)
            .map(|id| {
                let ns = ns.clone();
                let start = start.clone();
                let finish = finish.clone();
                std::thread::spawn(move || {
                    let mut claim = Claims::default();
                    start.wait();
                    let won = claim
                        .claim_in(&ns, &id.to_string(), Kind::Ollama, &["localhost:1234"])
                        .is_ok();
                    finish.wait();
                    (won, claim)
                })
            })
            .collect();
        start.wait();
        finish.wait();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|(won, _)| *won).count(), 1);
        drop(results);
        Claims::default()
            .claim_in(&ns, "after", Kind::Ollama, &["localhost:1234"])
            .unwrap();
    }
    #[test]
    fn child_claim_fixture() {
        use std::io::Write;
        let Ok(token) = std::env::var("GAMEPAUSE_CLAIM_FIXTURE") else {
            return;
        };
        assert!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'-')
        );
        let ns = format!("GamePause.Control.Fixture.Process.{token}");
        let mut claim = Claims::default();
        let result = claim.claim_in(&ns, "child", Kind::Ollama, &["localhost:1234"]);
        if std::env::var("GAMEPAUSE_CLAIM_EXPECT").unwrap() == "blocked" {
            assert!(result.is_err());
            return;
        }
        result.unwrap();
        println!("CLAIMED");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
    }
    #[test]
    fn process_exit_releases_claim_and_another_process_cannot_take_it() {
        use std::{
            io::Read,
            os::windows::io::AsRawHandle,
            os::windows::process::CommandExt,
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        let token = format!(
            "{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        );
        let ns = format!("GamePause.Control.Fixture.Process.{token}");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .creation_flags(0x08000000)
            .args([
                "--exact",
                "ownership::tests::child_claim_fixture",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("GAMEPAUSE_CLAIM_FIXTURE", &token)
            .env("GAMEPAUSE_CLAIM_EXPECT", "held")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let started = Instant::now();
        let mut bytes = Vec::new();
        let mut ready = false;
        while started.elapsed() < Duration::from_secs(3) && bytes.len() < 4096 {
            let mut available = 0;
            if unsafe {
                windows_sys::Win32::System::Pipes::PeekNamedPipe(
                    stdout.as_raw_handle(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                break;
            }
            if available > 0 {
                let mut chunk = [0u8; 512];
                let limit = chunk.len().min(available as usize);
                match stdout.read(&mut chunk[..limit]) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => bytes.extend_from_slice(&chunk[..count]),
                }
                if String::from_utf8_lossy(&bytes).contains("CLAIMED") {
                    ready = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut parent = Claims::default();
        let refused = ready
            && parent
                .claim_in(&ns, "parent", Kind::Ollama, &["127.0.0.1:1234"])
                .is_err();
        let _ = child.kill();
        child.wait().unwrap();
        assert!(ready && refused, "{}", String::from_utf8_lossy(&bytes));
        parent
            .claim_in(&ns, "parent", Kind::Ollama, &["127.0.0.1:1234"])
            .unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .creation_flags(0x08000000)
            .args([
                "--exact",
                "ownership::tests::child_claim_fixture",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("GAMEPAUSE_CLAIM_FIXTURE", &token)
            .env("GAMEPAUSE_CLAIM_EXPECT", "blocked")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
