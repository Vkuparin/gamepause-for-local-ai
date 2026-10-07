//! One adapter per launcher. Each reads that launcher's own records and never
//! scans a drive recursively; `Discovery::refresh` decides when they run.

use super::{
    Discovery, Game,
    metadata::{entries, parse_vdf, protobuf_fields, read_json, read_metadata, read_metadata_text},
    paths::{canonical, env_path, fixed_drives},
    registry::{reg_children, reg_values},
};
use crate::{config::Config, lmstudio::run_command};
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Duration,
};
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
/// GOG registers each installed product under its product ID, written by
/// Galaxy and by the offline installers alike. DLC has its own key naming the
/// base game in `dependsOn`, and is not a game of its own.
pub(super) fn gog_games(rows: Vec<(String, BTreeMap<String, String>)>) -> Vec<Game> {
    let filled = |v: &BTreeMap<String, String>, key: &str| {
        v.get(key).filter(|value| !value.trim().is_empty()).cloned()
    };
    rows.into_iter()
        .filter_map(|(id, v)| {
            if filled(&v, "dependsOn").is_some() {
                return None;
            }
            let folder = filled(&v, "path").map(PathBuf::from).or_else(|| {
                Path::new(&filled(&v, "exe")?)
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .map(Path::to_path_buf)
            })?;
            let name = filled(&v, "gameName")
                .or_else(|| Some(folder.file_name()?.to_str()?.to_owned()))
                .unwrap_or_else(|| id.clone());
            Some(Game::new("GOG", &id, &name, folder))
        })
        .collect()
}
/// How long the Xbox folder list and package inventory stand.
pub(super) const XBOX_REFRESH_SECONDS: f64 = 300.;
impl Discovery {
    pub(super) fn steam(&mut self, c: &Config) -> Result<Vec<Game>> {
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
    pub(super) fn epic(&self, c: &Config) -> Result<Vec<Game>> {
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
    pub(super) fn uninstall() -> Vec<(String, BTreeMap<String, String>)> {
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
    pub(super) fn ea(&self) -> Result<Vec<Game>> {
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
    pub(super) fn ubisoft(&self) -> Result<Vec<Game>> {
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
    pub(super) fn gog(&self) -> Result<Vec<Game>> {
        Ok(gog_games(reg_children(
            HKEY_LOCAL_MACHINE as usize,
            r"SOFTWARE\GOG.com\Games",
        )))
    }
    pub(super) fn battlenet(&self) -> Result<Vec<Game>> {
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
    pub(super) fn xbox(&mut self, now: f64, force: bool, defer: bool) -> Result<Vec<Game>> {
        // Every drive was once probed on each 30-second refresh. The folders
        // rarely change, so they are looked for when forced and otherwise
        // every five minutes, on fixed disks only.
        let stale = self
            .xbox_roots_at
            .is_none_or(|at| now - at >= XBOX_REFRESH_SECONDS);
        let mut roots = BTreeSet::new();
        for drive in (force || stale).then(fixed_drives).into_iter().flatten() {
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
        if force || stale {
            self.xbox_roots = roots.into_iter().filter(|root| root.is_dir()).collect();
            self.xbox_roots_at = Some(now);
        }
        let mut games = vec![];
        for root in self.xbox_roots.clone() {
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
        if !defer
            && (force
                || self
                    .package_at
                    .is_none_or(|n| now - n >= XBOX_REFRESH_SECONDS))
        {
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
    pub(super) fn custom(&self, c: &Config) -> Vec<Game> {
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
