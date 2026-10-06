use crate::{
    config::Config,
    discovery::{Game, canonical, inside},
};
use anyhow::Result;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_NO_MORE_FILES, FILETIME, GetLastError, INVALID_HANDLE_VALUE},
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

/// Programs that commonly run fullscreen and are not games.
const NOT_GAMES: &[&str] = &[
    "explorer.exe",
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "brave.exe",
    "opera.exe",
    "vivaldi.exe",
    "vlc.exe",
    "mpv.exe",
    "mpc-hc64.exe",
    "mpc-be64.exe",
    "wmplayer.exe",
    "potplayermini64.exe",
    "spotify.exe",
    "powerpnt.exe",
    "winword.exe",
    "excel.exe",
    "acrobat.exe",
    "acrord32.exe",
    "code.exe",
    "devenv.exe",
    "windowsterminal.exe",
    "obs64.exe",
    "mstsc.exe",
    "vmware.exe",
    "virtualboxvm.exe",
    "zoom.exe",
    "teams.exe",
    "ms-teams.exe",
    "discord.exe",
    "slack.exe",
    "applicationframehost.exe",
    "searchhost.exe",
    "shellexperiencehost.exe",
    "lockapp.exe",
    "textinputhost.exe",
    "dwm.exe",
    "lm studio.exe",
    "ollama app.exe",
];
/// Process that owns the foreground window when that window fills its
/// monitor without a caption, as fullscreen and borderless games do. Reads
/// window geometry only; no message is sent to the window.
pub fn fullscreen_foreground() -> Option<u32> {
    use windows_sys::Win32::{
        Foundation::RECT,
        Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
        },
        UI::WindowsAndMessaging::{
            GWL_STYLE, GetForegroundWindow, GetWindowLongW, GetWindowRect,
            GetWindowThreadProcessId, WS_CAPTION,
        },
    };
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return None;
        }
        let mut rect: RECT = std::mem::zeroed();
        let mut monitor: MONITORINFO = std::mem::zeroed();
        monitor.cbSize = size_of::<MONITORINFO>() as u32;
        if GetWindowRect(window, &mut rect) == 0
            || GetMonitorInfoW(
                MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            ) == 0
        {
            return None;
        }
        let screen = monitor.rcMonitor;
        let covers = rect.left <= screen.left
            && rect.top <= screen.top
            && rect.right >= screen.right
            && rect.bottom >= screen.bottom;
        let captioned = GetWindowLongW(window, GWL_STYLE) as u32 & WS_CAPTION == WS_CAPTION;
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, &mut pid);
        (covers && !captioned && pid != 0).then_some(pid)
    }
}

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
    "redprelauncher.exe",
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
    #[serde(skip_serializing)]
    pub created_at: u64,
    #[serde(skip_serializing)]
    pub executable: String,
    pub game: String,
    pub launcher: String,
    pub path: String,
}
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct RunningApp {
    pub name: String,
    pub path: String,
}
pub struct Scanner {
    cache: HashMap<(u32, u64), String>,
    candidates_seen: HashSet<(u32, u64)>,
    pub inaccessible: usize,
    pub uncertain_games: bool,
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
            candidates_seen: HashSet::new(),
            inaccessible: 0,
            uncertain_games: false,
            config,
            patterns,
        })
    }
    /// Runs for every process on every scan, so it compares in place.
    pub fn excluded(&self, name: &str, path: &str) -> bool {
        HELPERS
            .iter()
            .any(|helper| helper.eq_ignore_ascii_case(name))
            || self.patterns.iter().any(|r| r.is_match(name))
            || self
                .config
                .excluded_paths
                .iter()
                .any(|root| inside(path, root))
            || path.split(['\\', '/']).any(|part| {
                ["__installer", "_commonredist", "redist"]
                    .iter()
                    .any(|folder| folder.eq_ignore_ascii_case(part))
            })
    }
    /// The executable of a fullscreen foreground process that nothing
    /// recognises: not a known game, helper, exclusion, Windows component or
    /// common fullscreen program. A hint for the user, never a trigger.
    pub fn suggestion(&self, games: &[Game], foreground: Option<u32>) -> Option<String> {
        let pid = foreground?;
        let path = self
            .cache
            .iter()
            .find(|((candidate, _), _)| *candidate == pid)
            .map(|(_, path)| path)?;
        let name = path.rsplit(['\\', '/']).next().unwrap_or("").to_lowercase();
        let windows = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        (name.ends_with(".exe")
            && !NOT_GAMES.contains(&name.as_str())
            && !self.excluded(&name, path)
            && !inside(path, &windows)
            && self.match_path(path, games).is_none())
        .then(|| path.clone())
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
        let mut uncertain_pids = HashSet::new();
        self.inaccessible = 0;
        self.uncertain_games = false;
        // Match each already-known process once; the enumeration reuses it.
        let known: HashMap<(u32, u64), &Game> = self
            .cache
            .iter()
            .filter_map(|(key, path)| self.match_path(path, games).map(|game| (*key, game)))
            .collect();
        let known_pids: HashSet<u32> = known.keys().map(|key| key.0).collect();
        let known_names: HashSet<String> = games
            .iter()
            .filter_map(|game| {
                game.path
                    .rsplit(['\\', '/'])
                    .next()
                    .filter(|name| name.to_ascii_lowercase().ends_with(".exe"))
                    .map(str::to_lowercase)
            })
            .collect();
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
                let lower = name.to_lowercase();
                if !HELPERS.contains(&lower.as_str()) && entry.th32ProcessID != 0 {
                    let known_game =
                        known_pids.contains(&entry.th32ProcessID) || known_names.contains(&lower);
                    let process =
                        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, entry.th32ProcessID);
                    if process.is_null() {
                        uncertain_pids.insert(entry.th32ProcessID);
                        self.inaccessible += 1;
                        self.uncertain_games |= known_game;
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
                            let fresh = !self.cache.contains_key(&key);
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
                                    self.uncertain_games |= known_game;
                                }
                            }
                            if let Some(path) = self.cache.get(&key)
                                && let Some(game) = if fresh {
                                    self.match_path(path, games)
                                } else {
                                    known.get(&key).copied()
                                }
                            {
                                active.push(ActiveGame {
                                    pid: key.0,
                                    created_at: key.1,
                                    executable: path.clone(),
                                    game: game.name.clone(),
                                    launcher: game.launcher.clone(),
                                    path: game.path.clone(),
                                });
                            }
                        } else {
                            uncertain_pids.insert(entry.th32ProcessID);
                            self.inaccessible += 1;
                            self.uncertain_games |= known_game;
                        }
                        CloseHandle(process);
                    }
                }
                present = Process32NextW(snapshot, &mut entry);
            }
            let enumeration_error = GetLastError();
            CloseHandle(snapshot);
            if enumeration_error != ERROR_NO_MORE_FILES {
                return Err(std::io::Error::from_raw_os_error(enumeration_error as i32).into());
            }
        }
        self.cache
            .retain(|key, _| live.contains(key) || uncertain_pids.contains(&key.0));
        Ok(active)
    }
    pub fn running_apps(&self) -> Vec<RunningApp> {
        let mut paths = HashSet::new();
        let mut apps: Vec<_> = self
            .cache
            .values()
            .filter_map(|path| {
                let name = path.rsplit(['\\', '/']).next().unwrap_or("");
                if self.excluded(name, path) || !paths.insert(canonical(path)) {
                    return None;
                }
                Some(RunningApp {
                    name: name.into(),
                    path: path.clone(),
                })
            })
            .collect();
        apps.sort_by_key(|app| app.name.to_lowercase());
        apps
    }
    pub fn lmstudio_running(&self) -> bool {
        self.cache.values().any(|path| {
            path.rsplit(['\\', '/'])
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case("LM Studio.exe"))
        })
    }
    pub fn new_game_candidate(&mut self, games: &[Game], roots: &[String]) -> bool {
        self.candidates_seen
            .retain(|key| self.cache.contains_key(key));
        let candidates: Vec<_> = self
            .cache
            .iter()
            .filter(|(_, path)| {
                self.match_path(path, games).is_none()
                    && !self.excluded(path.rsplit(['\\', '/']).next().unwrap_or(""), path)
                    && roots.iter().any(|root| inside(path, root))
            })
            .map(|(key, _)| *key)
            .collect();
        let mut found = false;
        for key in candidates {
            found |= self.candidates_seen.insert(key);
        }
        found
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_steam_candidate_requests_refresh_once_without_classifying_it_as_game() {
        let mut scanner = Scanner::new(Config::default()).unwrap();
        scanner.cache.insert(
            (42, 1),
            r"D:\Steam\steamapps\common\New Game\play.exe".into(),
        );
        let roots = vec![r"D:\Steam\steamapps\common".into()];
        assert!(scanner.new_game_candidate(&[], &roots));
        assert!(!scanner.new_game_candidate(&[], &roots));
        assert!(
            scanner
                .match_path(r"D:\Steam\steamapps\common\New Game\play.exe", &[])
                .is_none()
        );
        scanner.cache.insert(
            (42, 2),
            r"D:\Steam\steamapps\common\New Game\play.exe".into(),
        );
        assert!(scanner.new_game_candidate(&[], &roots));
    }
    #[test]
    fn helpers_and_exclusions() {
        let default = Scanner::new(Config::default()).unwrap();
        assert!(default.excluded("REDprelauncher.exe", r"D:\Games\Witcher\REDprelauncher.exe"));
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
    fn registered_executable_does_not_classify_neighboring_applications() {
        let scanner = Scanner::new(Config::default()).unwrap();
        let games = vec![Game::new("Custom", "Game", "Game", r"D:\Games\play.exe")];
        assert!(scanner.match_path(r"d:\games\PLAY.exe", &games).is_some());
        assert!(scanner.match_path(r"D:\Games\other.exe", &games).is_none());
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

    #[test]
    fn only_an_unrecognised_fullscreen_program_is_suggested() {
        let mut config = Config::default();
        config.excluded_paths.push(r"D:\Tools".into());
        let mut scanner = Scanner::new(config).unwrap();
        for (pid, path) in [
            (1, r"D:\Unknown\Launcher Game\play.exe"),
            (2, r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
            (3, r"D:\Known\Game\known.exe"),
            (4, r"D:\Tools\tool.exe"),
            (5, r"D:\Steam\steam.exe"),
        ] {
            scanner.cache.insert((pid, 1), path.into());
        }
        let windows = std::env::var("WINDIR").unwrap();
        scanner
            .cache
            .insert((6, 1), format!(r"{windows}\System32\mspaint.exe"));
        let games = vec![Game::new("Fixture", "known", "Known", r"D:\Known\Game")];
        assert_eq!(
            scanner.suggestion(&games, Some(1)).as_deref(),
            Some(r"D:\Unknown\Launcher Game\play.exe")
        );
        for pid in [2, 3, 4, 5, 6, 99] {
            assert_eq!(scanner.suggestion(&games, Some(pid)), None, "{pid}");
        }
        assert_eq!(scanner.suggestion(&games, None), None);
    }
}
