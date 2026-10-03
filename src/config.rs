use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub poll_seconds: f64,
    pub discovery_seconds: f64,
    pub restore_delay_seconds: f64,
    pub retry_seconds: f64,
    pub mode: String,
    pub stop_server_during_gaming: bool,
    pub lms_path: String,
    pub api_host: String,
    pub steam_roots: Vec<String>,
    pub epic_manifest_dirs: Vec<String>,
    pub game_roots: Vec<String>,
    pub extra_games: Vec<ExtraGame>,
    pub excluded_executables: Vec<String>,
    pub excluded_paths: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtraGame {
    pub name: String,
    pub path: String,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            poll_seconds: 2.,
            discovery_seconds: 30.,
            restore_delay_seconds: 30.,
            retry_seconds: 30.,
            mode: "observe".into(),
            stop_server_during_gaming: true,
            lms_path: String::new(),
            api_host: "127.0.0.1:1234".into(),
            steam_roots: vec![],
            epic_manifest_dirs: vec![],
            game_roots: vec![],
            extra_games: vec![],
            excluded_executables: vec![],
            excluded_paths: vec![],
        }
    }
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            write_json(path, &Self::default())?;
        }
        let text = fs::read_to_string(path)?;
        let config: Self = serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .context("Invalid configuration")?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        for (name, value, min) in [
            ("poll_seconds", self.poll_seconds, 0.5),
            ("discovery_seconds", self.discovery_seconds, 10.),
            ("restore_delay_seconds", self.restore_delay_seconds, 0.),
            ("retry_seconds", self.retry_seconds, 5.),
        ] {
            if !value.is_finite() || value < min {
                bail!("{name} must be finite and >= {min}");
            }
        }
        if !["active", "observe"].contains(&self.mode.as_str()) {
            bail!("mode must be active or observe");
        }
        let (host, port) = self
            .api_host
            .rsplit_once(':')
            .context("api_host must be local host:port")?;
        if !["127.0.0.1", "localhost"].contains(&host) || port.parse::<u16>().unwrap_or(0) == 0 {
            bail!("api_host must be localhost or 127.0.0.1 with a valid port");
        }
        for game in &self.extra_games {
            if game.name.is_empty() || game.path.is_empty() {
                bail!("extra_games requires name and path");
            }
        }
        Ok(())
    }
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
mod tests {
    use super::*;
    #[test]
    fn config_validation() {
        let mut c = Config::default();
        assert!(c.validate().is_ok());
        c.api_host = "example.com:1234".into();
        assert!(c.validate().is_err());
        c = Config::default();
        c.poll_seconds = f64::NAN;
        assert!(c.validate().is_err());
    }
    #[test]
    fn unknown_settings_refused() {
        assert!(serde_json::from_str::<Config>(r#"{"poll_secnds":2}"#).is_err());
    }
    #[test]
    fn atomic_replace() {
        let p = std::env::temp_dir()
            .join(format!("gamepause-test-{}", std::process::id()))
            .join("state.json");
        write_json(&p, &serde_json::json!({"first":1})).unwrap();
        write_json(&p, &serde_json::json!({"second":2})).unwrap();
        assert!(fs::read_to_string(&p).unwrap().contains("second"));
        fs::remove_file(p).unwrap();
    }
}
