//! Daily, size-bounded worker logs with a fixed retention limit.
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

const MAX_BYTES: u64 = 1024 * 1024;
const MAX_FILES: usize = 10;
static CURRENT: Mutex<Option<Current>> = Mutex::new(None);
struct Current {
    folder: PathBuf,
    day: String,
    sequence: u32,
    path: PathBuf,
    prune_pending: bool,
}

fn name(day: &str, sequence: u32) -> String {
    if sequence == 0 {
        format!("gamepause-{day}.log")
    } else {
        format!("gamepause-{day}.{sequence:03}.log")
    }
}
fn key(name: &str) -> Option<(String, u32)> {
    let stem = name.strip_prefix("gamepause-")?.strip_suffix(".log")?;
    let (day, sequence) = stem
        .split_once('.')
        .map_or((stem, 0), |(day, n)| (day, n.parse().unwrap_or(u32::MAX)));
    if day.len() != 10
        || sequence == u32::MAX
        || !day.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
    {
        return None;
    }
    Some((day.into(), sequence))
}
fn files(folder: &Path) -> io::Result<Vec<((String, u32), PathBuf)>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && let Some(key) = key(&entry.file_name().to_string_lossy())
        {
            files.push((key, entry.path()));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}
fn prune(folder: &Path, current: &Path) -> io::Result<()> {
    let files = files(folder)?;
    let mut remaining = files.len();
    for (_, file) in files {
        if remaining <= MAX_FILES {
            break;
        }
        if file != current {
            fs::remove_file(file)?;
            remaining -= 1;
        }
    }
    Ok(())
}
pub(super) fn status_changed(
    previous: &(crate::control::Activity, String),
    activity: crate::control::Activity,
    message: &str,
) -> bool {
    if activity == crate::control::Activity::Countdown && previous.0 == activity {
        return false;
    }
    previous.0 != activity || previous.1 != message
}
pub(super) fn write(folder: &Path, message: &str, timestamp: &str) -> io::Result<()> {
    write_at(folder, message, timestamp)
}
fn write_at(folder: &Path, message: &str, timestamp: &str) -> io::Result<()> {
    let day = timestamp
        .get(..10)
        .ok_or_else(|| io::Error::other("Invalid log date"))?;
    let log_folder = folder.join("logs");
    fs::create_dir_all(&log_folder)?;
    let line = format!(
        "{timestamp} GamePause {} (PID {}) {}\n",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        message.chars().take(16384).collect::<String>()
    );
    let mut state = CURRENT
        .lock()
        .map_err(|_| io::Error::other("Log state unavailable"))?;
    if state
        .as_ref()
        .is_none_or(|s| s.folder != log_folder || s.day != day)
    {
        let sequence = files(&log_folder)?
            .iter()
            .filter(|((date, _), _)| date == day)
            .map(|((_, n), _)| *n)
            .max()
            .unwrap_or(0);
        *state = Some(Current {
            folder: log_folder.clone(),
            day: day.into(),
            sequence,
            path: log_folder.join(name(day, sequence)),
            prune_pending: true,
        });
    }
    let state = state
        .as_mut()
        .ok_or_else(|| io::Error::other("Log state unavailable"))?;
    let bytes = fs::metadata(&state.path).map_or(0, |m| m.len());
    if bytes.saturating_add(line.len() as u64) > MAX_BYTES {
        state.sequence = state
            .sequence
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Log sequence limit reached"))?;
        state.path = log_folder.join(name(day, state.sequence));
        state.prune_pending = true;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&state.path)?;
    file.write_all(line.as_bytes())?;
    drop(file);
    if state.prune_pending {
        prune(&log_folder, &state.path)?;
        state.prune_pending = false;
    }
    Ok(())
}
pub(crate) fn latest(folder: &Path) -> io::Result<PathBuf> {
    if folder.join("logs").is_dir()
        && let Some((_, path)) = files(&folder.join("logs"))?.pop()
    {
        return Ok(path);
    }
    // Old logs remain readable; new writes always go to logs/.
    Ok(folder.join("gamepause.log"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn countdown_updates_are_quiet_but_state_and_error_changes_are_logged() {
        use crate::control::Activity;
        let countdown = (Activity::Countdown, "restoring in 30s".into());
        assert!(!status_changed(
            &countdown,
            Activity::Countdown,
            "restoring in 28s"
        ));
        assert!(status_changed(
            &(Activity::Recovery, "waiting".into()),
            Activity::Countdown,
            "restoring in 30s"
        ));
        assert!(status_changed(&countdown, Activity::Restoring, "restoring"));
        let failed = (Activity::PartialFailure, "first error".into());
        assert!(!status_changed(
            &failed,
            Activity::PartialFailure,
            "first error"
        ));
        assert!(status_changed(
            &failed,
            Activity::PartialFailure,
            "second error"
        ));
    }
    #[test]
    fn daily_and_size_rotation_keep_ten_files_and_leave_unrelated_files() {
        let folder =
            std::env::temp_dir().join(format!("gamepause-log-rotation-{}", std::process::id()));
        fs::create_dir_all(folder.join("logs")).unwrap();
        fs::write(folder.join("logs/unrelated.log"), "keep").unwrap();
        write_at(&folder, "first", "2026-10-01 12:00:00").unwrap();
        let base = folder.join("logs/gamepause-2026-10-01.log");
        fs::OpenOptions::new()
            .write(true)
            .open(&base)
            .unwrap()
            .set_len(MAX_BYTES - 2)
            .unwrap();
        write_at(&folder, "size rotation", "2026-10-01 12:01:00").unwrap();
        assert!(folder.join("logs/gamepause-2026-10-01.001.log").exists());
        for day in 2..=13 {
            write_at(
                &folder,
                "daily rotation",
                &format!("2026-10-{day:02} 12:00:00"),
            )
            .unwrap();
        }
        let kept = files(&folder.join("logs")).unwrap();
        assert_eq!(kept.len(), MAX_FILES);
        assert!(
            kept.iter()
                .all(|(_, file)| fs::metadata(file).unwrap().len() <= MAX_BYTES)
        );
        assert!(!base.exists());
        assert!(folder.join("logs/unrelated.log").exists());
        assert_eq!(
            latest(&folder).unwrap().file_name().unwrap(),
            "gamepause-2026-10-13.log"
        );
        fs::remove_dir_all(folder).unwrap();
    }
}
