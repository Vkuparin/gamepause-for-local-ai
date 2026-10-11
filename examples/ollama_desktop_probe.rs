//! Explicit live experiment, excluded from default builds. Temporarily closes
//! an empty original Desktop, with sealed startup recovery, then tests a private
//! profile and restores the original. Never reads a GamePause recovery journal.
#[path = "ollama_desktop_probe/existing.rs"]
mod existing;
use anyhow::{Context, Result, bail};
use gamepause::ollama_session::{Http, Transport};
use std::{
    fs,
    net::TcpListener,
    os::windows::{ffi::OsStrExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        Threading::{
            CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, OpenProcess,
            PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            QueryFullProcessImageNameW, STARTUPINFOW, WaitForSingleObject,
        },
    },
    UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId, PostMessageW, WM_COMMAND},
};

const VERSION: &str = "0.40.3";
const QUIT: usize = 9;
const WAIT: Duration = Duration::from_secs(20);

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn tray() -> windows_sys::Win32::Foundation::HWND {
    unsafe { FindWindowW(gamepause::wide("OllamaClass").as_ptr(), std::ptr::null()) }
}

struct Child {
    handle: Handle,
    pid: u32,
}
impl Child {
    fn id(&self) -> u32 {
        self.pid
    }
    fn try_wait(&self) -> Result<Option<()>> {
        match unsafe { WaitForSingleObject(self.handle.0, 0) } {
            WAIT_OBJECT_0 => Ok(Some(())),
            windows_sys::Win32::Foundation::WAIT_TIMEOUT => Ok(None),
            _ => Err(std::io::Error::last_os_error()).context("Cannot inspect test child"),
        }
    }
}

fn spawn(exe: &Path, profile: &Path, endpoint: &str) -> Result<Child> {
    let mut variables = std::env::vars_os()
        .map(|(key, value)| (key.to_string_lossy().to_uppercase(), value))
        .collect::<std::collections::BTreeMap<_, _>>();
    for (key, value) in [
        ("LOCALAPPDATA", profile.as_os_str().to_owned()),
        ("APPDATA", profile.join("roaming").into_os_string()),
        ("USERPROFILE", profile.join("home").into_os_string()),
        (
            "OLLAMA_MODELS",
            std::env::var_os("GAMEPAUSE_FIXTURE_MODELS")
                .unwrap_or_else(|| profile.join("models").into_os_string()),
        ),
        ("OLLAMA_HOST", endpoint.into()),
        ("OLLAMA_NO_CLOUD", "1".into()),
    ] {
        variables.insert(key.into(), value);
    }
    let mut environment = Vec::<u16>::new();
    for (key, value) in variables {
        environment.extend(key.encode_utf16());
        environment.push('=' as u16);
        environment.extend(value.encode_wide());
        environment.push(0);
    }
    environment.push(0);
    spawn_raw(
        exe,
        &format!("\"{}\" hidden --fast-startup", exe.display()),
        &environment,
        None,
    )
}

fn spawn_raw(
    exe: &Path,
    command: &str,
    environment: &[u16],
    directory: Option<&str>,
) -> Result<Child> {
    let mut command = gamepause::wide(command);
    let application = exe
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = size_of::<STARTUPINFOW>() as u32;
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let directory = directory.map(gamepause::wide);
    if unsafe {
        CreateProcessW(
            application.as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            directory.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()),
            &startup,
            &mut process,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("Cannot start private Desktop");
    }
    unsafe { CloseHandle(process.hThread) };
    Ok(Child {
        handle: Handle(process.hProcess),
        pid: process.dwProcessId,
    })
}

fn quit(child: &mut Child) -> Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    let window = tray();
    let mut owner = 0;
    unsafe { GetWindowThreadProcessId(window, &mut owner) };
    if window.is_null() || owner != child.id() {
        bail!("The test child's tray window is unavailable; no command sent");
    }
    if unsafe { PostMessageW(window, WM_COMMAND, QUIT, 0) } == 0 {
        bail!("Windows refused the test child's Quit command");
    }
    let deadline = Instant::now() + WAIT;
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            bail!("Test Desktop did not exit; no forced termination attempted");
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

struct Desktop(Child);
impl Drop for Desktop {
    fn drop(&mut self) {
        if let Err(error) = quit(&mut self.0) {
            eprintln!("Test instance cleanup needs attention: {error}");
        }
    }
}

fn server(child: &Child, profile: &Path, expected: &Path) -> Result<Handle> {
    let pid = fs::read_to_string(profile.join("Ollama/ollama.pid"))?
        .trim()
        .parse::<u32>()?;
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        bail!("Cannot inspect the test server's parent");
    }
    let snapshot = Handle(snapshot);
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
    let mut ours = false;
    while more {
        if entry.th32ProcessID == pid {
            ours = entry.th32ParentProcessID == child.id();
            break;
        }
        more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    if !ours {
        bail!("Server PID is not a child of this test's Desktop");
    }
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if handle.is_null() {
        bail!("Cannot hold the test server's process identity");
    }
    let handle = Handle(handle);
    let mut path = vec![0u16; 32768];
    let mut length = path.len() as u32;
    if unsafe { QueryFullProcessImageNameW(handle.0, 0, path.as_mut_ptr(), &mut length) } == 0
        || !gamepause::discovery::same_path(
            &String::from_utf16_lossy(&path[..length as usize]),
            &expected.to_string_lossy(),
        )
    {
        bail!("Test server executable identity does not match");
    }
    Ok(handle)
}

fn cycle(exe: &Path, profile: &Path, endpoint: &str) -> Result<serde_json::Value> {
    if !tray().is_null() {
        bail!("Another Ollama Desktop is running; it was left alone");
    }
    let started = Instant::now();
    let mut desktop = Desktop(spawn(exe, profile, endpoint)?);
    let agent = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .redirects(0)
        .timeout(Duration::from_secs(1))
        .build();
    let url = format!("http://{endpoint}/api/version");
    loop {
        if let Ok(response) = agent.get(&url).call() {
            let value: serde_json::Value = response.into_json()?;
            if value["version"] != VERSION {
                bail!("The endpoint is not the expected Ollama version");
            }
            if !tray().is_null() {
                break;
            }
        }
        if desktop.0.try_wait()?.is_some() || started.elapsed() >= WAIT {
            bail!("Test Desktop/server did not become ready");
        }
        thread::sleep(Duration::from_millis(100));
    }
    let server = server(&desktop.0, profile, &exe.with_file_name("ollama.exe"))?;
    let mut http = Http::new(endpoint)?;
    let inventory: serde_json::Value = serde_json::from_slice(&http.request("/api/ps", None)?)?;
    let catalog: serde_json::Value = serde_json::from_slice(&http.request("/api/tags", None)?)?;
    if inventory["models"].as_array().is_none_or(|m| !m.is_empty())
        || catalog["models"].as_array().is_none_or(|m| !m.is_empty())
    {
        bail!("Test profile is not empty; no model operations attempted");
    }
    let startup_ms = started.elapsed().as_millis();
    let stopping = Instant::now();
    quit(&mut desktop.0)?;
    if unsafe { WaitForSingleObject(server.0, WAIT.as_millis() as u32) } != WAIT_OBJECT_0 {
        bail!("Desktop exited but its managed server did not");
    }
    if agent.get(&url).call().is_ok() {
        bail!("Endpoint still responds after shutdown; protection unverified");
    }
    Ok(serde_json::json!({"startup_ms":startup_ms,
        "shutdown_ms":stopping.elapsed().as_millis(),
        "desktop_exited":true,"managed_server_exited":true,"endpoint_stopped":true}))
}

fn production_cycle(exe: &Path, profile: &Path, endpoint: &str) -> Result<serde_json::Value> {
    use gamepause::{
        coordinator::{Coordinator, Store},
        ollama_session::{Adapter, Snapshot, SystemClock},
        provider::{Guarantee, Kind},
        recovery::{Binding, Intent, Journal},
    };
    struct Disk(PathBuf);
    impl Store<Snapshot> for Disk {
        fn save(&mut self, journal: Option<&Journal<Snapshot>>) -> Result<()> {
            match journal {
                Some(journal) => gamepause::config::write_json(&self.0, journal),
                None => {
                    fs::remove_file(&self.0)?;
                    Ok(())
                }
            }
        }
    }
    let started = Instant::now();
    let mut desktop = Desktop(spawn(exe, profile, endpoint)?);
    let mut http = Http::new(endpoint)?;
    while http.request("/api/version", None).is_err() || tray().is_null() {
        if started.elapsed() > WAIT || desktop.0.try_wait()?.is_some() {
            bail!("Private Desktop failed to start");
        }
        thread::sleep(Duration::from_millis(100));
    }
    let startup_ms = started.elapsed().as_millis();
    let with_model = std::env::args().any(|arg| arg == "--with-model");
    let race = std::env::args().any(|arg| arg == "--race-request");
    let mut inference = None;
    if with_model {
        http.request(
            "/api/generate",
            Some(
                serde_json::json!({"model":"qwen2.5:0.5b", "prompt":"", "stream":false,
            "keep_alive":-1, "options":{"num_ctx":4096}}),
            ),
        )?;
        let endpoint = endpoint.to_owned();
        if !race {
            inference = Some(thread::spawn(move || -> Result<()> {
                let agent = ureq::AgentBuilder::new()
                    .try_proxy_from_env(false)
                    .timeout(Duration::from_secs(90))
                    .build();
                agent.post(&format!("http://{endpoint}/api/generate")).send_json(serde_json::json!({"model":"qwen2.5:0.5b",
                "prompt":"List the numbers from one to five hundred, spelling each number out.", "stream":false,
                "keep_alive":-1,"options":{"num_ctx":4096,"num_predict":512}}))?;
                Ok(())
            }));
        }
        thread::sleep(Duration::from_millis(200));
    }
    let binding = Binding {
        id: "ollama-live-fixture".into(),
        kind: Kind::Ollama,
        endpoint: endpoint.into(),
        configured_endpoint: endpoint.into(),
        payload_version: 1,
        guarantee: Guarantee::SupportedFields,
    };
    let mut adapter = Adapter::new(http, SystemClock::default(), binding.clone())?;
    adapter.prevent_reload(true);
    let path = profile.join("production-recovery.json");
    let mut c = Coordinator::new(
        adapter,
        Disk(path.clone()),
        vec![binding.clone()],
        None,
        Duration::from_secs(1),
    )?;
    let pause = Instant::now();
    let mut late_request = None;
    while !c.pause_complete() {
        if race
            && late_request.is_none()
            && c.journal().is_some_and(|journal| {
                journal.providers[0]
                    .payload
                    .desktop
                    .as_ref()
                    .is_some_and(|d| format!("{:?}", d.phase) == "Stopping")
            })
        {
            let endpoint = endpoint.to_owned();
            late_request = Some(thread::spawn(move || {
                thread::sleep(Duration::from_millis(20));
                let agent = ureq::AgentBuilder::new()
                    .try_proxy_from_env(false)
                    .timeout(Duration::from_secs(30))
                    .build();
                match agent.post(&format!("http://{endpoint}/api/generate")).send_json(serde_json::json!({"model":"qwen2.5:0.5b",
                    "prompt":"List the numbers from one to five hundred.","stream":false,"keep_alive":-1,"options":{"num_ctx":4096,"num_predict":512}})) {
                    Ok(_) => "completed",
                    Err(ureq::Error::Transport(error)) if error.kind() == ureq::ErrorKind::ConnectionFailed => "connection_refused",
                    Err(_) => "request_failed_during_shutdown",
                }
            }));
        }
        c.advance(Intent::Pause, &[], pause.elapsed(), &mut || false)?;
        if let Some(saved) = c.journal()
            && saved.providers[0].payload.desktop.is_none()
        {
            bail!(
                "Production capture used fallback: {}",
                saved.providers[0].payload.note()
            );
        }
        if pause.elapsed() > Duration::from_secs(120) {
            bail!("Production pause failed: {:?}", c.reports(pause.elapsed()));
        }
        thread::sleep(Duration::from_millis(50));
    }
    let pause_ms = pause.elapsed().as_millis();
    let active_request_completed = inference.map(|worker| worker.join().unwrap().is_ok());
    let late_request_result = late_request.map(|worker| worker.join().unwrap());
    let saved: Journal<Snapshot> = serde_json::from_slice(&fs::read(&path)?)?;
    drop(c); // A fresh adapter recovers only from persisted, encrypted startup evidence.
    let external_restart = std::env::args().any(|arg| arg == "--external-restart");
    let external = if external_restart {
        let child = spawn(exe, profile, endpoint)?;
        let ready = Instant::now();
        while Http::new(endpoint)?.request("/api/version", None).is_err() || tray().is_null() {
            if ready.elapsed() > WAIT {
                bail!("External fixture restart did not become ready");
            }
            thread::sleep(Duration::from_millis(100));
        }
        Some(child)
    } else {
        None
    };
    let adapter = Adapter::new(
        Http::new(endpoint)?,
        SystemClock::default(),
        binding.clone(),
    )?;
    let mut c = Coordinator::new(
        adapter,
        Disk(path.clone()),
        vec![binding],
        Some(saved),
        Duration::from_secs(1),
    )?;
    let restore = Instant::now();
    while c.journal().is_some() {
        c.advance(Intent::Restore, &[], restore.elapsed(), &mut || false)?;
        if restore.elapsed() > Duration::from_secs(40) {
            bail!(
                "Production restore failed: {:?}",
                c.reports(restore.elapsed())
            );
        }
        thread::sleep(Duration::from_millis(100));
    }
    let restore_ms = restore.elapsed().as_millis();
    if let Some(child) = &external {
        let mut owner = 0;
        unsafe { GetWindowThreadProcessId(tray(), &mut owner) };
        if owner != child.id() {
            bail!("Production recovery duplicated the external restart");
        }
    }
    let inventory: serde_json::Value =
        serde_json::from_slice(&c.runtime.transport.request("/api/ps", None)?)?;
    if with_model {
        if inventory["models"]
            .as_array()
            .is_none_or(|models| models.len() != 1)
            || inventory["models"][0]["context_length"] != 4096
        {
            bail!("Production model restoration not verified");
        }
        c.runtime.transport.request("/api/generate", Some(serde_json::json!({"model":"qwen2.5:0.5b","prompt":"", "stream":false,"keep_alive":0})))?;
    }
    // Recovered instance belongs to this private fixture, whose profile is kept separate.
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(tray(), &mut pid) };
    desktop.0 = Child {
        handle: Handle(unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        }),
        pid,
    };
    quit(&mut desktop.0)?;
    Ok(
        serde_json::json!({"startup_ms":startup_ms,"pause_ms":pause_ms,"restore_ms":restore_ms,"durable_restart":true,
            "model_restore_verified":with_model,"active_request_completed":active_request_completed,
            "late_request_result":late_request_result,
            "external_restart_preserved":external_restart}),
    )
}

fn main() -> Result<()> {
    if std::env::args().any(|arg| arg == "--harmless-game") {
        thread::sleep(Duration::from_secs(20));
        return Ok(());
    }
    if !std::env::args().any(|argument| argument == "--run") {
        bail!(
            "Live lifecycle test requires --run; it may temporarily close an empty original Desktop"
        );
    }
    let exe = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("Pass the Desktop executable path")?,
    );
    let arguments = std::env::args_os().collect::<Vec<_>>();
    if let Some(index) = arguments.iter().position(|arg| arg == "--recover-test") {
        let profile = arguments
            .get(index + 1)
            .context("Pass this test's private profile")?;
        return existing::Original::recover_test(&exe, &PathBuf::from(profile));
    }
    let output = Command::new(&exe)
        .arg("--version")
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !output.status.success() || String::from_utf8(output.stdout)?.trim() != VERSION {
        bail!("This probe supports only Ollama Desktop 0.40.3");
    }
    let profile = std::env::current_dir()?
        .join("scratch")
        .join(format!("ollama-desktop-probe-{}", std::process::id()));
    fs::create_dir_all(profile.parent().unwrap())?;
    fs::create_dir(&profile).context("Test directory must not already exist")?;
    for folder in ["home", "roaming", "models"] {
        fs::create_dir(profile.join(folder))?;
    }
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = listener.local_addr()?.to_string();
    drop(listener);
    let mut original = existing::Original::capture(&exe, &profile)?;
    let initial = if std::env::args().any(|arg| arg == "--with-model") {
        serde_json::Value::Null
    } else {
        cycle(&exe, &profile, &endpoint)?
    };
    let restart = if std::env::args().any(|arg| arg == "--engine-game") {
        engine_game_cycle(&exe, &profile, &endpoint)?
    } else {
        production_cycle(&exe, &profile, &endpoint)?
    };
    original.restore()?;
    println!(
        "{}",
        serde_json::json!({"version":VERSION,"private_profile":true,
        "initial":initial,"restart":restart,"inference_tested":std::env::args().any(|arg| arg == "--with-model")})
    );
    Ok(())
}

fn engine_game_cycle(exe: &Path, profile: &Path, endpoint: &str) -> Result<serde_json::Value> {
    use gamepause::config::{Config, ExtraGame, Provider, write_json};
    let mut desktop = Desktop(spawn(exe, profile, endpoint)?);
    let started = Instant::now();
    let mut http = Http::new(endpoint)?;
    while http.request("/api/version", None).is_err() || tray().is_null() {
        if started.elapsed() > WAIT {
            bail!("Private Desktop failed to start");
        }
        thread::sleep(Duration::from_millis(100));
    }
    let with_model = std::env::args().any(|arg| arg == "--with-model");
    if with_model {
        http.request(
            "/api/generate",
            Some(
                serde_json::json!({"model":"qwen2.5:0.5b","prompt":"","stream":false,
        "keep_alive":-1,"options":{"num_ctx":4096}}),
            ),
        )?;
    }
    let data = profile.join("watcher-data");
    fs::create_dir(&data)?;
    let harmless = profile.join("FixtureGame.exe");
    fs::copy(std::env::current_exe()?, &harmless)?;
    let mut config = Config {
        poll_seconds: 0.5,
        restore_delay_seconds: 0.,
        retry_seconds: 5.,
        notifications_enabled: false,
        sound_enabled: false,
        ..Config::default()
    };
    // This fixture must not acquire gameplay policy from unrelated real games.
    config.ignored_games = gamepause::discovery::Discovery::default()
        .refresh(&config, 0., true, false)
        .into_iter()
        .map(|game| game.path)
        .collect();
    for provider in &mut config.providers {
        match provider {
            Provider::LMStudio { enabled, .. } | Provider::Process { enabled, .. } => {
                *enabled = false
            }
            Provider::Ollama {
                enabled,
                endpoint: route,
                prevent_reload,
                ..
            } => {
                *enabled = true;
                *route = endpoint.into();
                *prevent_reload = true;
            }
        }
    }
    config.extra_games.push(ExtraGame {
        name: "Harmless lifecycle fixture".into(),
        path: harmless.to_string_lossy().into_owned(),
    });
    write_json(&data.join("config.json"), &config)?;
    let watcher = std::env::var_os("GAMEPAUSE_FIXTURE_WATCHER")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap()
                .join("target/release/GamePauseCLI.exe")
        });
    let observe = Command::new(&watcher)
        .args(["--headless", "--observe", "--duration", "1", "--data-dir"])
        .arg(&data)
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !observe.status.success() || data.join("state.json").exists() {
        bail!("Observation preflight failed");
    }
    let mut game = Command::new(&harmless)
        .arg("--harmless-game")
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    let fixture_game = gamepause::discovery::Game::new(
        "Custom",
        "fixture",
        "Harmless lifecycle fixture",
        &harmless,
    );
    let games = vec![fixture_game];
    let mut scanner = gamepause::processes::Scanner::new(config.clone())?;
    struct DisabledLm;
    impl gamepause::lmstudio::Backend for DisabledLm {
        fn snapshot(&mut self) -> Result<gamepause::lmstudio::Snapshot> {
            panic!("LM Studio must remain disabled")
        }
        fn loaded(&mut self) -> Result<Vec<serde_json::Value>> {
            panic!("LM Studio must remain disabled")
        }
        fn stop_server(&mut self) -> Result<()> {
            panic!("LM Studio must remain disabled")
        }
        fn start_server(&mut self, _: u16) -> Result<()> {
            panic!("LM Studio must remain disabled")
        }
        fn unload(&mut self, _: &str) -> Result<()> {
            panic!("LM Studio must remain disabled")
        }
        fn restore(&mut self, _: &gamepause::lmstudio::Model) -> Result<()> {
            panic!("LM Studio must remain disabled")
        }
        fn read_config(&mut self, _: &gamepause::lmstudio::Model) -> Result<serde_json::Value> {
            panic!("LM Studio must remain disabled")
        }
        fn installed(&mut self) -> bool {
            false
        }
    }
    let mut engine = gamepause::engine::Engine::new(config, DisabledLm, data.join("state.json"))?;
    engine.remember_games(games.clone())?;
    let began = Instant::now();
    let mut pause_ms = None;
    let mut restore_ms = None;
    let mut exited_at = None;
    while began.elapsed() < Duration::from_secs(60) {
        if game.try_wait()?.is_some() && exited_at.is_none() {
            exited_at = Some(Instant::now());
        }
        let mut scan = || {
            scanner
                .scan(&games)
                .ok()
                .map(|active| gamepause::gameplay::GameEvidence {
                    all: active.clone(),
                    triggers: active,
                })
        };
        let evidence = scan();
        engine.step_games(evidence, began.elapsed().as_secs_f64(), &mut scan);
        if engine.pause_verified() && pause_ms.is_none() {
            if http.request("/api/version", None).is_ok() {
                bail!("Engine reported protection while endpoint remained available");
            }
            pause_ms = Some(began.elapsed().as_millis());
        }
        if pause_ms.is_some() && !engine.pending() && exited_at.is_some() {
            restore_ms = exited_at.map(|time| time.elapsed().as_millis());
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    if pause_ms.is_none() || restore_ms.is_none() || engine.pending() {
        bail!(
            "Engine custom-game lifecycle failed: {}; private journal retained",
            engine.last_error
        );
    }
    let inventory: serde_json::Value = serde_json::from_slice(&http.request("/api/ps", None)?)?;
    if with_model
        && (inventory["models"].as_array().is_none_or(|m| m.len() != 1)
            || inventory["models"][0]["context_length"] != 4096)
    {
        bail!("Engine did not restore model context and residency");
    }
    let mut owner = 0;
    unsafe { GetWindowThreadProcessId(tray(), &mut owner) };
    desktop.0 = Child {
        handle: Handle(unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                owner,
            )
        }),
        pid: owner,
    };
    quit(&mut desktop.0)?;
    Ok(
        serde_json::json!({"custom_game_pause_ms":pause_ms,"after_game_restore_ms":restore_ms,
        "model_restore_verified":with_model,"observation_preflight":true,"recovery_cleared":true}),
    )
}
