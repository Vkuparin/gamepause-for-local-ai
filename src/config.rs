use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}
impl Appearance {
    pub fn index(self) -> usize {
        match self {
            Self::System => 0,
            Self::Light => 1,
            Self::Dark => 2,
        }
    }
    pub fn from_index(index: isize) -> Option<Self> {
        match index {
            0 => Some(Self::System),
            1 => Some(Self::Light),
            2 => Some(Self::Dark),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub poll_seconds: f64,
    pub discovery_seconds: f64,
    pub restore_delay_seconds: f64,
    pub retry_seconds: f64,
    pub mode: String,
    pub settings_version: u32,
    pub automation_enabled: bool,
    pub ignored_games: Vec<String>,
    /// Games that never pause AI on their own: GamePause asks instead, and
    /// AI keeps running until the user answers.
    pub ask_games: Vec<String>,
    pub providers: Vec<Provider>,
    pub advanced_settings_visible: bool,
    pub appearance: Appearance,
    pub notifications_enabled: bool,
    pub sound_enabled: bool,
    pub steam_roots: Vec<String>,
    pub epic_manifest_dirs: Vec<String>,
    pub game_roots: Vec<String>,
    pub extra_games: Vec<ExtraGame>,
    pub excluded_executables: Vec<String>,
    pub excluded_paths: Vec<String>,
    /// Point out fullscreen programs no launcher knows, so they can be added.
    /// GamePause never pauses AI for them on its own.
    pub suggest_unknown_games: bool,
    /// Executables the user said are not games; never suggested again.
    pub dismissed_suggestions: Vec<String>,
    /// System-wide Pause AI / Resume AI shortcut such as `Ctrl+Alt+P`.
    /// Empty registers nothing.
    pub pause_hotkey: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Current,
    VersionThree,
    Legacy,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtraGame {
    pub name: String,
    pub path: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Provider {
    LMStudio {
        id: String,
        enabled: bool,
        connection: LMConnection,
    },
    Ollama {
        id: String,
        enabled: bool,
        endpoint: String,
    },
    /// Executables to stop for gaming. Not tied to a port or protocol.
    Process {
        id: String,
        enabled: bool,
        apps: Vec<ProcessApp>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessApp {
    pub name: String,
    /// Full path of the executable; only this exact file is ever stopped.
    pub path: String,
    /// Start it again after gaming with its recorded command line.
    pub relaunch: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LMConnection {
    pub endpoint: String,
    pub lms_path: String,
    pub stop_server_during_gaming: bool,
}
impl Provider {
    pub fn id(&self) -> &str {
        match self {
            Self::LMStudio { id, .. } | Self::Ollama { id, .. } | Self::Process { id, .. } => id,
        }
    }
    pub fn enabled(&self) -> bool {
        match self {
            Self::LMStudio { enabled, .. } | Self::Ollama { enabled, .. } => *enabled,
            // With no app chosen there is nothing to control.
            Self::Process { enabled, apps, .. } => *enabled && !apps.is_empty(),
        }
    }
    pub fn kind(&self) -> crate::provider::Kind {
        match self {
            Self::LMStudio { .. } => crate::provider::Kind::LMStudio,
            Self::Ollama { .. } => crate::provider::Kind::Ollama,
            Self::Process { .. } => crate::provider::Kind::Process,
        }
    }
    pub fn endpoint(&self) -> &str {
        match self {
            Self::LMStudio { connection, .. } => &connection.endpoint,
            Self::Ollama { endpoint, .. } => endpoint,
            Self::Process { .. } => crate::process_session::ROUTE,
        }
    }
}
fn default_providers() -> Vec<Provider> {
    vec![
        Provider::LMStudio {
            id: "lmstudio-main".into(),
            enabled: true,
            connection: LMConnection {
                endpoint: "127.0.0.1:1234".into(),
                lms_path: String::new(),
                stop_server_during_gaming: true,
            },
        },
        Provider::Ollama {
            id: "ollama-main".into(),
            // Unit tests must not reach a developer's real Ollama through
            // default settings; fixtures enable it against private endpoints.
            enabled: !cfg!(test),
            endpoint: "127.0.0.1:11434".into(),
        },
        Provider::Process {
            id: "apps-main".into(),
            enabled: true,
            apps: vec![],
        },
    ]
}
impl Default for Config {
    fn default() -> Self {
        Self {
            poll_seconds: 2.,
            discovery_seconds: 30.,
            restore_delay_seconds: 30.,
            retry_seconds: 30.,
            mode: "active".into(),
            settings_version: 4,
            automation_enabled: true,
            ignored_games: vec![],
            ask_games: vec![],
            providers: default_providers(),
            advanced_settings_visible: false,
            appearance: Appearance::System,
            notifications_enabled: true,
            sound_enabled: true,
            steam_roots: vec![],
            epic_manifest_dirs: vec![],
            game_roots: vec![],
            extra_games: vec![],
            excluded_executables: vec![],
            excluded_paths: vec![],
            suggest_unknown_games: true,
            dismissed_suggestions: vec![],
            pause_hotkey: String::new(),
        }
    }
}
/// Parses `Ctrl+Alt+P` style text into Win32 modifier flags and a virtual
/// key. Requires Ctrl, Alt or Win so plain typing can never trigger it.
pub fn parse_hotkey(text: &str) -> Result<Option<(u32, u32)>> {
    const ALT: u32 = 1;
    const CONTROL: u32 = 2;
    const SHIFT: u32 = 4;
    const WIN: u32 = 8;
    if text.trim().is_empty() {
        return Ok(None);
    }
    let (mut modifiers, mut key) = (0, None);
    for part in text.split('+').map(str::trim) {
        let upper = part.to_ascii_uppercase();
        let modifier = match upper.as_str() {
            "CTRL" | "CONTROL" => CONTROL,
            "ALT" => ALT,
            "SHIFT" => SHIFT,
            "WIN" | "WINDOWS" => WIN,
            _ => 0,
        };
        if modifier != 0 {
            modifiers |= modifier;
            continue;
        }
        let bytes = upper.as_bytes();
        let code = match bytes {
            [c] if c.is_ascii_uppercase() || c.is_ascii_digit() => u32::from(*c),
            [b'F', digits @ ..] if !digits.is_empty() && digits.len() <= 2 => {
                match upper[1..].parse::<u32>() {
                    Ok(n @ 1..=24) => 0x6F + n,
                    _ => 0,
                }
            }
            _ => match upper.as_str() {
                "PAUSE" => 0x13,
                "SPACE" => 0x20,
                "HOME" => 0x24,
                "END" => 0x23,
                "INSERT" => 0x2D,
                "DELETE" => 0x2E,
                "PAGEUP" => 0x21,
                "PAGEDOWN" => 0x22,
                _ => 0,
            },
        };
        if code == 0 || key.replace(code).is_some() {
            bail!("pause_hotkey must be modifiers plus one key, such as Ctrl+Alt+P");
        }
    }
    match key {
        Some(key) if modifiers & (CONTROL | ALT | WIN) != 0 => Ok(Some((modifiers, key))),
        _ => bail!("pause_hotkey needs Ctrl, Alt or Win plus one key, such as Ctrl+Alt+P"),
    }
}
impl Config {
    /// The "ask" rule for a game folder or executable path.
    pub fn asks(&self, path: &str) -> bool {
        self.ask_games
            .iter()
            .any(|ask| crate::discovery::same_path(ask, path))
    }
    pub fn any_provider_enabled(&self) -> bool {
        self.providers.iter().any(Provider::enabled)
    }
    pub fn ollama_enabled(&self) -> bool {
        self.providers
            .iter()
            .any(|provider| provider.kind() == crate::provider::Kind::Ollama && provider.enabled())
    }
    /// Validated in-memory migration for read-only diagnostics. No files change.
    pub fn parse(text: &str) -> Result<Self> {
        Ok(Self::decode(text)?.0)
    }
    /// Returns the settings and the format they were read from. Version-2 and
    /// unversioned files pass through the version-3 shape on their way to 4.
    fn decode(text: &str) -> Result<(Self, Source)> {
        let mut raw: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .context("Invalid configuration")?;
        let legacy = raw.get("settings_version").is_none() || raw["settings_version"] == 2;
        if legacy {
            let unversioned = raw.get("settings_version").is_none();
            let map = raw
                .as_object_mut()
                .context("Configuration must be an object")?;
            if map
                .get("mode")
                .is_some_and(|mode| mode != "active" && mode != "observe")
            {
                bail!("Invalid legacy mode; source retained");
            }
            for key in [
                "providers",
                "advanced_settings_visible",
                "appearance",
                "notifications_enabled",
                "sound_enabled",
            ] {
                if map.contains_key(key) {
                    bail!("Version-3 field in legacy configuration; source retained");
                }
            }
            let mut providers = default_providers();
            if let Provider::LMStudio { connection, .. } = &mut providers[0] {
                if let Some(value) = map.remove("api_host") {
                    connection.endpoint = serde_json::from_value(value)?;
                }
                if let Some(value) = map.remove("lms_path") {
                    connection.lms_path = serde_json::from_value(value)?;
                }
                if let Some(value) = map.remove("stop_server_during_gaming") {
                    connection.stop_server_during_gaming = serde_json::from_value(value)?;
                }
            }
            map.insert("providers".into(), serde_json::to_value(providers)?);
            map.insert("settings_version".into(), 3.into());
            if unversioned {
                map.insert("mode".into(), "active".into());
            }
        }
        let version_three = raw["settings_version"] == 3;
        if version_three {
            let map = raw
                .as_object_mut()
                .context("Configuration must be an object")?;
            // Version 3 shipped with Ollama off by default. A file without the
            // interim marker never chose that, so it gets the current default;
            // a file with the marker keeps whatever its owner saved.
            let chosen = map.remove("ollama_default_applied").is_some();
            if !legacy
                && !chosen
                && let Some(providers) = map.get_mut("providers").and_then(|v| v.as_array_mut())
            {
                for provider in providers.iter_mut().filter(|p| p["kind"] == "ollama") {
                    provider["enabled"] = true.into();
                }
            }
            // Version 4 adds the entry for other AI apps, with none chosen.
            if let Some(providers) = map.get_mut("providers").and_then(|v| v.as_array_mut())
                && !providers.iter().any(|p| p["kind"] == "process")
                && providers.len() < 3
            {
                providers.push(serde_json::to_value(&default_providers()[2])?);
            }
            map.insert("settings_version".into(), 4.into());
        }
        let config: Self = serde_json::from_value(raw).context("Invalid configuration")?;
        config.validate()?;
        Ok((
            config,
            if legacy {
                Source::Legacy
            } else if version_three {
                Source::VersionThree
            } else {
                Source::Current
            },
        ))
    }
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            write_json(path, &Self::default())?;
        }
        let text = fs::read_to_string(path)?;
        let (config, source) = Self::decode(&text)?;
        let backup = match source {
            Source::Current => return Ok(config),
            Source::Legacy => path.with_extension("v2.backup.json"),
            Source::VersionThree => path.with_extension("v3.backup.json"),
        };
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)
        {
            Ok(mut file) => {
                file.write_all(text.as_bytes())?;
                file.sync_all()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if fs::read(&backup)? != text.as_bytes() {
                    bail!("Settings backup already contains different data; source retained");
                }
            }
            Err(e) => return Err(e.into()),
        }
        write_json(path, &config).context("Settings migration could not save; source retained")?;
        Ok(config)
    }
    pub fn lm(&self) -> Option<&LMConnection> {
        self.providers.iter().find_map(|provider| match provider {
            Provider::LMStudio { connection, .. } => Some(connection),
            _ => None,
        })
    }
    pub fn lm_mut(&mut self) -> Result<&mut LMConnection> {
        self.providers
            .iter_mut()
            .find_map(|provider| match provider {
                Provider::LMStudio { connection, .. } => Some(connection),
                _ => None,
            })
            .context("LM Studio provider is not configured")
    }
    pub fn lm_endpoint(&self) -> &str {
        self.lm().map_or("", |lm| lm.endpoint.as_str())
    }
    pub fn lms_path(&self) -> &str {
        self.lm().map_or("", |lm| lm.lms_path.as_str())
    }
    pub fn lm_enabled(&self) -> bool {
        self.providers.iter().any(|provider| {
            provider.kind() == crate::provider::Kind::LMStudio && provider.enabled()
        })
    }
    pub fn stop_server_during_gaming(&self) -> bool {
        self.lm().is_some_and(|lm| lm.stop_server_during_gaming)
    }
    pub fn validate_recovery_edit(&self, updated: &Self) -> Result<()> {
        // Loaded recovery has already validated this configured LM binding.
        // Recovery uses the journal's original route, not a newly selected port.
        if let Some(original) = self
            .providers
            .iter()
            .find(|p| p.kind() == crate::provider::Kind::LMStudio)
        {
            let replacement = updated.providers.iter().find(|p| p.id() == original.id());
            if replacement.is_none_or(|p| {
                !p.enabled()
                    || p.kind() != original.kind()
                    || normalized_endpoint(p.endpoint()).ok()
                        != normalized_endpoint(original.endpoint()).ok()
            }) {
                bail!(
                    "LM Studio recovery is pending; restore it before disabling, removing or reassigning its endpoint/ID"
                );
            }
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        for (name, value, min) in [
            ("poll_seconds", self.poll_seconds, 0.5),
            ("discovery_seconds", self.discovery_seconds, 10.),
            ("restore_delay_seconds", self.restore_delay_seconds, 0.),
            ("retry_seconds", self.retry_seconds, 5.),
        ] {
            if !value.is_finite() || value < min || value > 86400. {
                bail!("{name} must be finite and between {min} and 86400 seconds");
            }
        }
        if !["active", "observe"].contains(&self.mode.as_str()) {
            bail!("mode must be active or observe");
        }
        parse_hotkey(&self.pause_hotkey)?;
        if self.settings_version != 4 {
            bail!("Unsupported settings version; configuration retained");
        }
        let mut ids = std::collections::HashSet::new();
        let mut kinds = std::collections::HashSet::new();
        let mut endpoints = std::collections::HashSet::new();
        if self.providers.len() > 3 {
            bail!("Only one entry per supported provider kind is allowed");
        }
        for provider in &self.providers {
            let id = provider.id();
            if id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
                || !ids.insert(id)
            {
                bail!("Invalid or duplicate provider ID");
            }
            if !kinds.insert(provider.kind()) {
                bail!("Duplicate provider kind");
            }
            if let Provider::Process { apps, .. } = provider {
                let mut paths = std::collections::HashSet::new();
                if apps.len() > 32
                    || apps.iter().any(|app| {
                        app.name.trim().is_empty()
                            || !std::path::Path::new(&app.path).is_absolute()
                            || !app.path.to_ascii_lowercase().ends_with(".exe")
                            || !paths.insert(crate::discovery::canonical(&app.path))
                    })
                {
                    bail!(
                        "Other AI apps need a name and a distinct full path to an .exe file, at most 32"
                    );
                }
            } else if !endpoints.insert(normalized_endpoint(provider.endpoint())?) {
                bail!("Duplicate provider endpoint");
            }
        }
        for game in &self.extra_games {
            if game.name.is_empty() || game.path.is_empty() {
                bail!("extra_games requires name and path");
            }
        }
        Ok(())
    }
}
pub fn normalized_endpoint(endpoint: &str) -> Result<String> {
    let (host, port) = endpoint
        .rsplit_once(':')
        .context("Provider endpoint must be local host:port")?;
    if !["127.0.0.1", "localhost"].contains(&host)
        || !port.bytes().all(|c| c.is_ascii_digit())
        || port.parse::<u16>().unwrap_or(0) == 0
    {
        bail!("Provider endpoint must be localhost or 127.0.0.1 with a valid port");
    }
    Ok(format!("127.0.0.1:{}", port.parse::<u16>()?))
}
pub fn data_directory() -> PathBuf {
    PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("GamePause")
}
static SERIAL: AtomicU64 = AtomicU64::new(0);
pub fn write_json(path: &Path, value: &(impl Serialize + ?Sized)) -> Result<()> {
    let parent = path.parent().context("File has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".gamepause-{}-{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        let source = crate::wide(&temp.to_string_lossy());
        let target = crate::wide(&path.to_string_lossy());
        unsafe {
            if windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                source.as_ptr(),
                target.as_ptr(),
                windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                    | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        Ok(())
    })();
    let _ = fs::remove_file(temp);
    result
}
pub fn lock(folder: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(folder.join("instance.lock"))
        .context("GamePause is already running with this data directory")
}

#[cfg(test)]
mod tests;
