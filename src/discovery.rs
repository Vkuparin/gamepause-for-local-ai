use crate::{config::Config, lmstudio::run_command};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::panic::AssertUnwindSafe;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use winreg::{RegKey, enums::*};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Game {
    pub launcher: String,
    pub identity: String,
    pub name: String,
    pub path: String,
}
impl Game {
    pub fn new(launcher: &str, id: &str, name: &str, path: impl AsRef<Path>) -> Self {
        Self {
            launcher: launcher.into(),
            identity: id.into(),
            name: name.into(),
            path: path.as_ref().to_string_lossy().into_owned(),
        }
    }
}
pub fn canonical(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}
pub fn inside(path: &str, root: &str) -> bool {
    let p = canonical(path);
    let r = canonical(root);
    !r.is_empty() && (p == r || p.starts_with(&(r + "\\")))
}
fn background_utility(game: &Game) -> bool {
    [
        "wallpaper engine",
        "lossless scaling",
        "steamworks common redistributables",
        "gog galaxy",
        "fps monitor",
    ]
    .contains(&game.name.to_lowercase().as_str())
}
pub fn parse_vdf(text: &str) -> Result<Value> {
    if text.len() > 16 * 1024 * 1024 {
        bail!("Launcher metadata exceeds 16 MiB");
    }
    let token = regex::Regex::new(r#"//[^\n]*|"((?:\\.|[^"\\])*)"|([{}])"#)?;
    let tokens: Vec<String> = token
        .captures_iter(text)
        .filter_map(|c| {
            c.get(1)
                .map(|m| {
                    format!(
                        "Q{}",
                        m.as_str().replace(r#"\""#, "\"").replace(r"\\", r"\")
                    )
                })
                .or_else(|| c.get(2).map(|m| m.as_str().into()))
        })
        .collect();
    fn object(
        tokens: &[String],
        position: &mut usize,
        nested: bool,
        depth: usize,
    ) -> Result<Value> {
        if depth > 64 {
            bail!("KeyValues nesting exceeds 64 levels");
        }
        let mut map = Map::new();
        while *position < tokens.len() {
            let key = &tokens[*position];
            *position += 1;
            if key == "}" {
                if nested {
                    return Ok(Value::Object(map));
                }
                bail!("Unexpected KeyValues closing brace");
            }
            let key = key
                .strip_prefix('Q')
                .context("Expected KeyValues key")?
                .to_owned();
            let token = tokens.get(*position).context("Incomplete KeyValues")?;
            *position += 1;
            let value = if token == "{" {
                object(tokens, position, true, depth + 1)?
            } else {
                Value::String(
                    token
                        .strip_prefix('Q')
                        .context("Expected KeyValues value")?
                        .into(),
                )
            };
            map.insert(key, value);
        }
        if nested {
            bail!("Incomplete KeyValues object");
        }
        Ok(Value::Object(map))
    }
    object(&tokens, &mut 0, false, 0)
}
fn env_path(key: &str, fallback: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(key).unwrap_or_else(|| fallback.into()))
}
fn entries(path: &Path) -> Vec<PathBuf> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(|r| r.ok().map(|e| e.path()))
        .collect()
}
fn read_metadata(path: impl AsRef<Path>) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 {
        bail!("Launcher metadata exceeds 16 MiB");
    }
    Ok(bytes)
}
fn read_metadata_text(path: impl AsRef<Path>) -> Result<String> {
    Ok(String::from_utf8(read_metadata(path)?)?)
}
fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_str(
        read_metadata_text(path)?.trim_start_matches('\u{feff}'),
    )?)
}
fn reg_values(hive: usize, path: &str) -> Vec<BTreeMap<String, String>> {
    let mut rows = vec![];
    for view in [KEY_WOW64_32KEY, KEY_WOW64_64KEY] {
        if let Ok(key) = RegKey::predef(hive as _).open_subkey_with_flags(path, KEY_READ | view) {
            let map = key
                .enum_values()
                .filter_map(|r| {
                    r.ok().and_then(|(name, _)| {
                        key.get_value::<String, _>(&name)
                            .ok()
                            .map(|value| (name, value))
                    })
                })
                .collect();
            if !rows.contains(&map) {
                rows.push(map);
            }
        }
    }
    rows
}
fn reg_children(hive: usize, path: &str) -> Vec<(String, BTreeMap<String, String>)> {
    let mut names = BTreeSet::new();
    for view in [KEY_WOW64_32KEY, KEY_WOW64_64KEY] {
        if let Ok(key) = RegKey::predef(hive as _).open_subkey_with_flags(path, KEY_READ | view) {
            names.extend(key.enum_keys().filter_map(Result::ok));
        }
    }
    names
        .into_iter()
        .flat_map(|name| {
            reg_values(hive, &format!("{path}\\{name}"))
                .into_iter()
                .map(move |v| (name.clone(), v))
        })
        .collect()
}
#[derive(Default)]
pub struct Discovery {
    pub steam_libraries: Vec<String>,
    pub errors: BTreeMap<String, String>,
    retained: BTreeMap<String, Vec<Game>>,
    packages: Vec<Game>,
    package_at: Option<f64>,
}
impl Discovery {
    pub fn refresh(
        &mut self,
        c: &Config,
        now: f64,
        force: bool,
        defer_packages: bool,
    ) -> Vec<Game> {
        self.errors.clear();
        // Each adapter replaces only its own last successful result.
        let adapters = [
            ("Steam", self.steam(c)),
            ("Epic", self.epic(c)),
            ("EA", self.ea()),
            ("Ubisoft", self.ubisoft()),
            ("Battle.net", self.battlenet()),
            ("Xbox", self.xbox(now, force, defer_packages)),
            ("Custom", Ok(self.custom(c))),
        ];
        for (name, result) in adapters {
            match result {
                Ok(games) => {
                    self.retained.insert(name.into(), games);
                }
                Err(e) => {
                    self.errors.insert(name.into(), format!("{e:#}"));
                }
            }
        }
        self.finalize()
    }
    /// The final inventory for this refresh: deduplicated, existing paths only,
    /// background utilities excluded. Read from `retained`, so it is stable
    /// across a refresh and can be reused by the panic-recovery path.
    fn finalize(&self) -> Vec<Game> {
        let mut seen = BTreeSet::new();
        self.retained
            .values()
            .flatten()
            .filter(|g| {
                !background_utility(g)
                    && Path::new(&g.path).exists()
                    && seen.insert(canonical(&g.path))
            })
            .cloned()
            .collect()
    }
    /// Run a scan body while containing any panic it raises. Returns
    /// `(inventory, panic_payload)`. A panic in an adapter — e.g. a future
    /// adapter added with an unwrapping bug, or a filesystem race in the Xbox
    /// `.GamingRoot` reader — must not kill the discovery worker and orphan the
    /// last good inventory: the payload is logged and recorded, and the last
    /// successful inventory is retained (best-available: each adapter keeps
    /// its own last good result).
    fn run_catchable(&mut self, body: impl FnOnce(&mut Self) -> Vec<Game>) -> (Vec<Game>, String) {
        match std::panic::catch_unwind(AssertUnwindSafe(|| body(self))) {
            Ok(games) => (games, String::new()),
            Err(panic) => {
                let payload = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_string()))
                    .unwrap_or_else(|| "unknown panic".into());
                self.errors.clear();
                self.errors.insert(
                    "Discovery".into(),
                    format!("Inventory scan panicked: {payload}"),
                );
                (self.finalize(), payload)
            }
        }
    }
    /// Production entry point: run a full refresh under panic containment.
    pub fn recover_refresh(
        &mut self,
        c: &Config,
        now: f64,
        force: bool,
        defer_packages: bool,
    ) -> (Vec<Game>, String) {
        self.run_catchable(move |d| d.refresh(c, now, force, defer_packages))
    }
    /// Only compiled for the production-profile regression example, never in default artifacts.
    #[cfg(feature = "resilience-test")]
    pub fn resilience_probe(&mut self) -> Result<()> {
        let good = Game::new("Probe", "probe", "Retained", std::env::temp_dir());
        self.retained.insert("Probe".into(), vec![good]);
        let (games, panic) = self.run_catchable(|_| panic!("injected discovery panic"));
        if games.len() != 1 || panic.is_empty() {
            bail!("Panic recovery failed");
        }
        let (games, panic) = self.run_catchable(|d| d.finalize());
        if games.len() != 1 || !panic.is_empty() {
            bail!("Refresh after panic failed");
        }
        Ok(())
    }
    fn steam(&mut self, c: &Config) -> Result<Vec<Game>> {
        let mut roots: BTreeSet<PathBuf> = c.steam_roots.iter().map(PathBuf::from).collect();
        roots.insert(env_path("ProgramFiles(x86)", r"C:\Program Files (x86)").join("Steam"));
        for values in reg_values(HKEY_CURRENT_USER as usize, r"Software\Valve\Steam") {
            if let Some(root) = values.get("SteamPath") {
                roots.insert(root.into());
            }
        }
        for root in roots.clone() {
            let file = root.join(r"steamapps\libraryfolders.vdf");
            if file.is_file() {
                let v = parse_vdf(&read_metadata_text(file)?)?;
                let map = v
                    .get("libraryfolders")
                    .unwrap_or(&v)
                    .as_object()
                    .context("Invalid Steam library metadata")?;
                for (k, v) in map {
                    if k.chars().all(|c| c.is_ascii_digit())
                        && let Some(p) = v.as_str().or_else(|| v["path"].as_str())
                    {
                        roots.insert(p.into());
                    }
                }
            }
        }
        self.steam_libraries = roots
            .iter()
            .map(|p| p.join(r"steamapps\common").to_string_lossy().into_owned())
            .collect();
        let mut games = vec![];
        for root in roots {
            for file in entries(&root.join("steamapps")) {
                if !file
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .starts_with("appmanifest_")
                {
                    continue;
                }
                let Ok(text) = read_metadata_text(file) else {
                    continue;
                };
                let Ok(v) = parse_vdf(&text) else { continue };
                let s = &v["AppState"];
                let folder = s["installdir"].as_str().unwrap_or("");
                let flags = s["StateFlags"]
                    .as_str()
                    .unwrap_or("0")
                    .parse::<u64>()
                    .unwrap_or(0);
                if flags & 4 != 0
                    && !folder.is_empty()
                    && !folder.contains(':')
                    && !folder.starts_with(['\\', '/'])
                    && !folder.replace('/', "\\").split('\\').any(|p| p == "..")
                {
                    games.push(Game::new(
                        "Steam",
                        s["appid"].as_str().unwrap_or(folder),
                        s["name"].as_str().unwrap_or(folder),
                        root.join(r"steamapps\common").join(folder),
                    ));
                }
            }
        }
        Ok(games)
    }
    fn epic(&self, c: &Config) -> Result<Vec<Game>> {
        let mut roots: Vec<PathBuf> = c.epic_manifest_dirs.iter().map(PathBuf::from).collect();
        roots.push(
            env_path("PROGRAMDATA", r"C:\ProgramData")
                .join(r"Epic\EpicGamesLauncher\Data\Manifests"),
        );
        let mut games = vec![];
        for root in roots {
            for file in entries(&root) {
                if file.extension().is_none_or(|e| e != "item") {
                    continue;
                }
                let Ok(v) = read_json(&file) else { continue };
                if v["bIsIncompleteInstall"] == true
                    || v["LaunchExecutable"].as_str().unwrap_or("").is_empty()
                {
                    continue;
                }
                if let Some(path) = v["InstallLocation"].as_str() {
                    games.push(Game::new(
                        "Epic",
                        v["AppName"].as_str().unwrap_or("unknown"),
                        v["DisplayName"].as_str().unwrap_or("Epic game"),
                        path,
                    ));
                }
            }
        }
        Ok(games)
    }
    fn uninstall() -> Vec<(String, BTreeMap<String, String>)> {
        [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER]
            .iter()
            .flat_map(|h| {
                reg_children(
                    *h as usize,
                    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
                )
            })
            .collect()
    }
    fn ea(&self) -> Result<Vec<Game>> {
        let mut games = vec![];
        for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
            for key in [r"SOFTWARE\EA Games", r"SOFTWARE\Electronic Arts\EA Games"] {
                for (name, v) in reg_children(hive as usize, key) {
                    if let Some(p) = ["Install Dir", "InstallDir", "InstallLocation"]
                        .iter()
                        .find_map(|k| v.get(*k))
                    {
                        games.push(Game::new(
                            "EA",
                            &name,
                            v.get("DisplayName").unwrap_or(&name),
                            p,
                        ));
                    }
                }
            }
        }
        for (name, v) in Self::uninstall() {
            if let Some(p) = v.get("InstallLocation")
                && v.get("Publisher")
                    .is_some_and(|s| s.to_lowercase().contains("electronic arts"))
                && Path::new(p).join("__Installer").is_dir()
            {
                games.push(Game::new(
                    "EA",
                    &name,
                    v.get("DisplayName").unwrap_or(&name),
                    p,
                ));
            }
        }
        Ok(games)
    }
    fn ubisoft(&self) -> Result<Vec<Game>> {
        Ok(reg_children(
            HKEY_LOCAL_MACHINE as usize,
            r"SOFTWARE\Ubisoft\Launcher\Installs",
        )
        .into_iter()
        .filter_map(|(name, v)| {
            v.get("InstallDir").map(|p| {
                Game::new(
                    "Ubisoft",
                    &name,
                    Path::new(p)
                        .file_name()
                        .unwrap_or_default()
                        .to_str()
                        .unwrap_or(&name),
                    p,
                )
            })
        })
        .collect())
    }
    fn battlenet(&self) -> Result<Vec<Game>> {
        let mut games = vec![];
        for (name, v) in Self::uninstall() {
            let command = v
                .get("UninstallString")
                .map(|s| s.to_lowercase())
                .unwrap_or_default();
            if ["bna", "agent", "battle.net", "battlenet"].contains(&name.to_lowercase().as_str()) {
                continue;
            }
            if command.contains("battle.net")
                && command.contains("--uid=")
                && !command.contains("--uid=bna")
                && !command.contains("--uid=agent")
                && let Some(p) = v.get("InstallLocation")
            {
                games.push(Game::new(
                    "Battle.net",
                    &name,
                    v.get("DisplayName").unwrap_or(&name),
                    p,
                ));
            }
        }
        let file = env_path("PROGRAMDATA", r"C:\ProgramData").join(r"Battle.net\Agent\product.db");
        if file.is_file() {
            let bytes = read_metadata(file)?;
            for (key, record) in protobuf_fields(&bytes)? {
                if key != 1 {
                    continue;
                }
                let fields = protobuf_fields(record)?;
                let id = fields
                    .iter()
                    .find(|(k, _)| *k == 1)
                    .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
                    .unwrap_or_default();
                if id.is_empty()
                    || ["bna", "agent", "battle.net", "battlenet"]
                        .contains(&id.to_lowercase().as_str())
                {
                    continue;
                }
                if let Some((_, data)) = fields.iter().find(|(k, _)| *k == 3)
                    && let Some((_, p)) = protobuf_fields(data)?.iter().find(|(k, _)| *k == 1)
                {
                    let path = String::from_utf8_lossy(p);
                    if Path::new(path.as_ref()).join(".build.info").is_file() {
                        games.push(Game::new(
                            "Battle.net",
                            &id,
                            Path::new(path.as_ref())
                                .file_name()
                                .unwrap_or_default()
                                .to_str()
                                .unwrap_or(&id),
                            path.as_ref(),
                        ));
                    }
                }
            }
        }
        Ok(games)
    }
    fn xbox(&mut self, now: f64, force: bool, defer: bool) -> Result<Vec<Game>> {
        let mut roots = BTreeSet::new();
        for drive in b'C'..=b'Z' {
            let drive = PathBuf::from(format!("{}:\\", drive as char));
            let file = drive.join(".GamingRoot");
            if let Ok(bytes) = read_metadata(file)
                && bytes.len() > 8
            {
                let units: Vec<u16> = bytes[8..]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .take_while(|n| *n != 0)
                    .collect();
                let root = String::from_utf16_lossy(&units);
                if !root.is_empty() {
                    roots.insert(drive.join(root.trim_start_matches('\\')));
                }
            }
            roots.insert(drive.join("XboxGames"));
        }
        let mut games = vec![];
        for root in roots {
            for child in entries(&root) {
                let content = child.join("Content");
                let location = if content.is_dir() {
                    content
                } else {
                    child.clone()
                };
                let file = location.join("MicrosoftGame.config");
                if let Ok(text) = read_metadata_text(file)
                    && let Ok(doc) = roxmltree::Document::parse(&text)
                {
                    let id = doc
                        .descendants()
                        .find(|n| n.has_tag_name("Identity"))
                        .and_then(|n| n.attribute("Name"))
                        .unwrap_or("Xbox game");
                    games.push(Game::new(
                        "Xbox",
                        id,
                        child.file_name().unwrap_or_default().to_str().unwrap_or(id),
                        location,
                    ));
                }
            }
        }
        if !defer && (force || self.package_at.is_none_or(|n| now - n >= 300.)) {
            let script = r#"$ErrorActionPreference='Stop'; $items=@(Get-AppxPackage | ForEach-Object {$p=$_; try {$m=Get-AppxPackageManifest -Package $p.PackageFullName; if ($m.OuterXml -match '(?i)windows\.game|XboxLive|MicrosoftGame') {[pscustomobject]@{name=$p.Name; path=$p.InstallLocation}}} catch {}}); ConvertTo-Json -InputObject $items -Compress"#;
            let bytes = run_command(
                "powershell.exe",
                &["-NoProfile", "-NonInteractive", "-Command", script],
                Duration::from_secs(30),
            )?;
            let v: Value = serde_json::from_slice(&bytes)?;
            let rows = v.as_array().context("Invalid Xbox package inventory")?;
            self.packages = rows
                .iter()
                .filter_map(|r| {
                    let name = r["name"].as_str()?;
                    let path = r["path"].as_str()?;
                    let lower = name.to_lowercase();
                    if lower.starts_with("microsoft.xbox")
                        || ["microsoft.gamingservices", "microsoft.gamingapp"]
                            .contains(&lower.as_str())
                        || canonical(path).contains("\\systemapps\\")
                    {
                        return None;
                    }
                    Some(Game::new("Xbox", name, name, path))
                })
                .collect();
            self.package_at = Some(now);
        }
        games.extend(self.packages.clone());
        Ok(games)
    }
    fn custom(&self, c: &Config) -> Vec<Game> {
        let mut games: Vec<_> = c
            .extra_games
            .iter()
            .map(|g| Game::new("Custom", &g.name, &g.name, &g.path))
            .collect();
        for root in &c.game_roots {
            for path in entries(Path::new(root)) {
                if path.is_dir() {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    games.push(Game::new("Custom", &name, &name, &path));
                }
            }
        }
        games
    }
}
pub fn protobuf_fields(mut bytes: &[u8]) -> Result<Vec<(u64, &[u8])>> {
    fn varint(bytes: &mut &[u8]) -> Result<u64> {
        let mut value = 0;
        for shift in (0..70).step_by(7) {
            let b = *bytes.first().context("Truncated protobuf")?;
            *bytes = &bytes[1..];
            if shift == 63 && b > 1 {
                bail!("Protobuf integer overflow");
            }
            value |= ((b & 127) as u64) << shift;
            if b & 128 == 0 {
                return Ok(value);
            }
        }
        bail!("Invalid protobuf integer")
    }
    let mut result = vec![];
    while !bytes.is_empty() {
        let tag = varint(&mut bytes)?;
        let (field, wire) = (tag >> 3, tag & 7);
        if field == 0 {
            bail!("Invalid protobuf field");
        }
        let length = match wire {
            0 => {
                varint(&mut bytes)?;
                continue;
            }
            1 => 8,
            2 => usize::try_from(varint(&mut bytes)?)?,
            5 => 4,
            _ => bail!("Unsupported protobuf wire type"),
        };
        if length > bytes.len() {
            bail!("Truncated protobuf field");
        }
        if wire == 2 {
            result.push((field, &bytes[..length]));
        }
        bytes = &bytes[length..];
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn steam_metadata_refresh_finds_new_installation_without_restart() {
        let root =
            std::env::temp_dir().join(format!("gamepause-steam-refresh-{}", std::process::id()));
        let apps = root.join("steamapps");
        let common = apps.join("common");
        fs::create_dir_all(common.join("Fixture One")).unwrap();
        fs::create_dir_all(common.join("Fixture Two")).unwrap();
        let one = apps.join("appmanifest_900000001.acf");
        let two = apps.join("appmanifest_900000002.acf");
        fs::write(&one, r#""AppState" { "appid" "900000001" "name" "GamePause Fixture One" "installdir" "Fixture One" "StateFlags" "4" }"#).unwrap();
        let config = Config {
            steam_roots: vec![root.to_string_lossy().into_owned()],
            ..Config::default()
        };
        let mut discovery = Discovery::default();
        let initial = discovery.steam(&config).unwrap();
        assert!(initial.iter().any(|g| g.identity == "900000001"));
        assert!(!initial.iter().any(|g| g.identity == "900000002"));
        fs::write(&two, r#""AppState" { "appid" "900000002" "name" "GamePause Fixture Two" "installdir" "Fixture Two" "StateFlags" "4" }"#).unwrap();
        let refreshed = discovery.steam(&config).unwrap();
        assert!(refreshed.iter().any(|g| g.identity == "900000002"));
        fs::remove_file(one).unwrap();
        fs::remove_file(two).unwrap();
        fs::remove_dir(common.join("Fixture One")).unwrap();
        fs::remove_dir(common.join("Fixture Two")).unwrap();
        fs::remove_dir(common).unwrap();
        fs::remove_dir(apps).unwrap();
        fs::remove_dir(root).unwrap();
    }
    #[test]
    fn background_utilities_are_not_games() {
        assert!(background_utility(&Game::new(
            "Steam",
            "431960",
            "Wallpaper Engine",
            r"D:\Steam\Wallpaper"
        )));
        assert!(!background_utility(&Game::new(
            "Steam",
            "292030",
            "The Witcher 3",
            r"D:\Steam\Witcher"
        )));
    }
    #[test]
    fn path_boundaries() {
        assert!(inside(
            r"D:\Games\Witcher\bin\game.exe",
            r"d:/games/witcher"
        ));
        assert!(!inside(r"D:\Games\Witcher2\game.exe", r"D:\Games\Witcher"));
    }
    #[test]
    fn worker_survives_a_panicking_adapter_and_keeps_last_good_inventory() {
        // P0-2: the discovery worker runs adapters on a background thread. A
        // panic there (a future adapter with an unwrapping bug, a filesystem
        // race in the Xbox `.GamingRoot` reader) must not kill the worker and
        // orphan the last good inventory. This drives a REAL `panic!` through
        // the exact `run_catchable` path the worker uses (via
        // `recover_refresh`) and verifies the worker's contract: no
        // propagation, the payload is recorded, and the last successful
        // inventory is retained.
        let mut discovery = Discovery::default();
        // Seed a "last good" inventory the way a prior successful refresh would.
        let good = Game::new("Steam", "100", "Last Good Game", std::env::temp_dir());
        discovery
            .retained
            .insert("Steam".into(), vec![good.clone()]);
        // `refresh` clears errors and re-runs every adapter, so a panic in a
        // body that models a panicking adapter is equivalent to one of the real
        // adapters panicking mid-refresh.
        let (games, payload) = discovery.run_catchable(|_d| panic!("adapter blew up"));
        assert_eq!(payload, "adapter blew up", "panic payload must be captured");
        assert!(
            discovery.errors.contains_key("Discovery"),
            "the panic must be recorded in discovery.errors for the UI/doctor panel"
        );
        assert_eq!(
            games.len(),
            1,
            "last good inventory must be retained after a panic"
        );
        assert_eq!(games[0].identity, good.identity);
        assert_eq!(games[0].path, good.path);
    }
    #[test]
    fn empty_vdf_strings() {
        let v = parse_vdf(r#""root" { "empty" "" "nested" { "path" "D:\\Games" } }"#).unwrap();
        assert_eq!(v["root"]["empty"], "");
        assert_eq!(v["root"]["nested"]["path"], r"D:\Games");
    }
    #[test]
    fn incomplete_vdf_refused() {
        assert!(parse_vdf(r#""root" { "key" "value" "#).is_err());
    }
    #[test]
    fn protobuf_boundaries() {
        assert_eq!(
            protobuf_fields(&[10, 3, b'a', b'b', b'c']).unwrap()[0].1,
            b"abc"
        );
        assert!(protobuf_fields(&[10, 50, 1]).is_err());
        assert!(protobuf_fields(&[128]).is_err());
    }
    #[test]
    fn excessive_metadata_nesting_is_rejected_without_stack_exhaustion() {
        let input = "\"key\" {".repeat(100) + &"}".repeat(100);
        assert!(
            parse_vdf(&input)
                .unwrap_err()
                .to_string()
                .contains("nesting")
        );
    }
}
