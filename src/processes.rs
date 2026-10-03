use crate::{
    config::Config,
    discovery::{Game, canonical, inside},
};
use anyhow::Result;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use windows_sys::Win32::{
    Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        Threading::{
            GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
    },
};

pub const HELPERS: &[&str] = &[
    "galaxyclient.exe",
    "galaxyclientservice.exe",
    "galaxycommunication.exe",
    "galaxyclient helper.exe",
    "losslessscaling.exe",
    "wallpaper32.exe",
    "wallpaper64.exe",
    "fpsmonitor.exe",
    "steam.exe",
    "steamwebhelper.exe",
    "epicgameslauncher.exe",
    "epicwebhelper.exe",
    "eadesktop.exe",
    "eabackgroundservice.exe",
    "ealauncher.exe",
    "origin.exe",
    "ubisoftconnect.exe",
    "upc.exe",
    "uplaywebcore.exe",
    "battle.net.exe",
    "agent.exe",
    "gamingservices.exe",
    "gamingservicesnet.exe",
    "gamelaunchhelper.exe",
    "redlauncher.exe",
    "setup.exe",
    "uninstall.exe",
    "unins000.exe",
    "unitycrashhandler64.exe",
    "crashreporter.exe",
    "crashreportclient.exe",
    "werfault.exe",
    "easyanticheat.exe",
    "easyanticheat_eos.exe",
    "beservice.exe",
    "beservice_x64.exe",
    "gamepause.exe",
    "gamepausecli.exe",
];
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ActiveGame {
    pub pid: u32,
    pub game: String,
    pub launcher: String,
}
pub struct Scanner {
    cache: HashMap<(u32, u64), String>,
    pub inaccessible: usize,
    config: Config,
    patterns: Vec<regex::Regex>,
}
impl Scanner {
    pub fn new(config: Config) -> Result<Self> {
        let patterns = config
            .excluded_executables
            .iter()
            .map(|p| {
                let mut pattern = String::from("(?i)^");
                for ch in p.chars() {
                    match ch {
                        '*' => pattern.push_str(".*"),
                        '?' => pattern.push('.'),
                        _ => pattern.push_str(&regex::escape(&ch.to_string())),
                    }
                }
                pattern.push('$');
                regex::Regex::new(&pattern)
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Self {
            cache: HashMap::new(),
            inaccessible: 0,
            config,
            patterns,
        })
    }
    pub fn excluded(&self, name: &str, path: &str) -> bool {
        HELPERS.contains(&name.to_lowercase().as_str())
            || self.patterns.iter().any(|r| r.is_match(name))
            || self
                .config
                .excluded_paths
                .iter()
                .any(|root| inside(path, root))
            || canonical(path)
                .split('\\')
                .any(|p| ["__installer", "_commonredist", "redist"].contains(&p))
    }
    pub fn match_path<'a>(&self, path: &str, games: &'a [Game]) -> Option<&'a Game> {
        let name = path.rsplit(['\\', '/']).next().unwrap_or("");
        if self.excluded(name, path) {
            return None;
        }
        games
            .iter()
            .filter(|g| inside(path, &g.path))
            .max_by_key(|g| g.path.len())
    }
    pub fn scan(&mut self, games: &[Game]) -> Result<Vec<ActiveGame>> {
        let mut active = vec![];
        let mut live = HashSet::new();
        self.inaccessible = 0;
        // Handles are owned only within this enumeration; every successful open is closed.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut present = Process32FirstW(snapshot, &mut entry);
            while present != 0 {
                let name = String::from_utf16_lossy(
                    &entry.szExeFile[..entry
                        .szExeFile
                        .iter()
                        .position(|n| *n == 0)
                        .unwrap_or(entry.szExeFile.len())],
                );
                if !HELPERS.contains(&name.to_lowercase().as_str()) && entry.th32ProcessID != 0 {
                    let process =
                        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, entry.th32ProcessID);
                    if process.is_null() {
                        self.inaccessible += 1;
                    } else {
                        let mut created: FILETIME = std::mem::zeroed();
                        let mut exit = created;
                        let mut kernel = created;
                        let mut user = created;
                        if GetProcessTimes(process, &mut created, &mut exit, &mut kernel, &mut user)
                            != 0
                        {
                            let key = (
                                entry.th32ProcessID,
                                ((created.dwHighDateTime as u64) << 32)
                                    | created.dwLowDateTime as u64,
                            );
                            live.insert(key);
                            if let std::collections::hash_map::Entry::Vacant(v) =
                                self.cache.entry(key)
                            {
                                let mut path = vec![0u16; 32768];
                                let mut size = path.len() as u32;
                                if QueryFullProcessImageNameW(
                                    process,
                                    0,
                                    path.as_mut_ptr(),
                                    &mut size,
                                ) != 0
                                {
                                    v.insert(String::from_utf16_lossy(&path[..size as usize]));
                                } else {
                                    self.inaccessible += 1;
                                }
                            }
                            if let Some(path) = self.cache.get(&key)
                                && let Some(game) = self.match_path(path, games)
                            {
                                active.push(ActiveGame {
                                    pid: key.0,
                                    game: game.name.clone(),
                                    launcher: game.launcher.clone(),
                                });
                            }
                        } else {
                            self.inaccessible += 1;
                        }
                        CloseHandle(process);
                    }
                }
                present = Process32NextW(snapshot, &mut entry);
            }
            CloseHandle(snapshot);
        }
        self.cache.retain(|key, _| live.contains(key));
        Ok(active)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn helpers_and_exclusions() {
        let c = Config {
            excluded_executables: vec!["helper*.exe".into()],
            ..Default::default()
        };
        let s = Scanner::new(c).unwrap();
        assert!(s.excluded("REDlauncher.exe", r"D:\Games\Witcher\REDlauncher.exe"));
        assert!(s.excluded("helper64.exe", r"D:\Games\helper64.exe"));
        assert!(s.excluded("a.exe", r"D:\Games\x\__Installer\a.exe"));
    }
    #[test]
    fn nested_folder_wins() {
        let s = Scanner::new(Config::default()).unwrap();
        let games = [
            Game::new("Steam", "root", "fallback", r"D:\Games"),
            Game::new("Steam", "1", "Witcher", r"D:\Games\Witcher"),
        ];
        assert_eq!(
            s.match_path(r"D:\Games\Witcher\game.exe", &games)
                .unwrap()
                .name,
            "Witcher"
        );
        assert!(
            s.match_path(r"D:\Games\Witcher\redlauncher.exe", &games)
                .is_none()
        );
    }
}
