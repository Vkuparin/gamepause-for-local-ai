//! Executable icons for dashboard rows. Loaded only while the dashboard is open.
//! The loader never runs on the watcher, detection or control threads.
use crate::{discovery::canonical, wide};
use eframe::egui::{ColorImage, Context, TextureHandle, TextureOptions};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    ptr::null_mut,
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
};
use windows_sys::Win32::{
    Graphics::Gdi::*,
    UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO, PrivateExtractIconsW},
};

const MAX_ICONS: usize = 512;
const MAX_PENDING: usize = 32;
const MAX_UPLOADS: usize = 8;
const MAX_ENTRIES: usize = 2000;
const MAX_FOLDERS: usize = 48;
/// Helpers that ship beside games and would otherwise win on file size.
const NOT_GAMES: [&str; 14] = [
    "unins",
    "crash",
    "redist",
    "setup",
    "install",
    "report",
    "helper",
    "cef",
    "vc_",
    "anticheat",
    "touchup",
    "cleanup",
    "handler",
    "updater",
];

struct Request {
    key: (String, u32),
    path: String,
    hint: Option<String>,
}
struct Loaded {
    key: (String, u32),
    image: Option<ColorImage>,
}
enum Slot {
    Pending { hinted: bool },
    Missing { hinted: bool },
    Ready(TextureHandle),
}
struct Cached {
    slot: Slot,
    used: u64,
}

/// UI-thread cache. Dropping it closes the request channel and ends the loader.
#[derive(Default)]
pub struct Cache {
    slots: HashMap<(String, u32), Cached>,
    link: Option<(SyncSender<Request>, Receiver<Loaded>)>,
    used: u64,
}
impl Cache {
    /// `path` is a game folder or executable. `hint` is its running executable, when known.
    pub fn get(
        &mut self,
        ctx: &Context,
        path: &str,
        hint: Option<&str>,
        size: u32,
    ) -> Option<TextureHandle> {
        let key = (canonical(path), size);
        self.used = self.used.saturating_add(1);
        if let Some(cached) = self.slots.get_mut(&key) {
            cached.used = self.used;
        }
        match self.slots.get(&key).map(|cached| &cached.slot) {
            Some(Slot::Ready(texture)) => return Some(texture.clone()),
            Some(Slot::Pending { .. }) => return None,
            // A game found running later can supply the executable a folder search missed.
            Some(Slot::Missing { hinted }) if *hinted || hint.is_none() => return None,
            _ => (),
        }
        if self
            .slots
            .values()
            .filter(|cached| matches!(cached.slot, Slot::Pending { .. }))
            .count()
            >= MAX_PENDING
        {
            return None;
        }
        if !self.slots.contains_key(&key) && !self.make_room() {
            return None;
        }
        let hinted = hint.is_some();
        if self.link.is_none() {
            self.link = spawn(ctx.clone());
        }
        let sent = if let Some((tx, _)) = &self.link {
            match tx.try_send(Request {
                key: key.clone(),
                path: path.into(),
                hint: hint.map(Into::into),
            }) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => return None,
                Err(TrySendError::Disconnected(_)) => false,
            }
        } else {
            false
        };
        self.slots.insert(
            key,
            Cached {
                used: self.used,
                slot: if sent {
                    Slot::Pending { hinted }
                } else {
                    Slot::Missing { hinted: true }
                },
            },
        );
        None
    }
    fn make_room(&mut self) -> bool {
        if self.slots.len() < MAX_ICONS {
            return true;
        }
        let oldest = self
            .slots
            .iter()
            .filter(|(_, cached)| !matches!(cached.slot, Slot::Pending { .. }))
            .min_by_key(|(_, cached)| cached.used)
            .map(|(key, _)| key.clone());
        if let Some(key) = oldest {
            self.slots.remove(&key);
            true
        } else {
            false
        }
    }
    pub fn poll(&mut self, ctx: &Context) {
        let Some((_, rx)) = &self.link else { return };
        for _ in 0..MAX_UPLOADS {
            let Ok(loaded) = rx.try_recv() else { return };
            let Some(cached) = self.slots.get_mut(&loaded.key) else {
                continue;
            };
            let Slot::Pending { hinted } = cached.slot else {
                continue;
            };
            let slot = match loaded.image {
                Some(image) => Slot::Ready(ctx.load_texture(
                    format!("game-icon-{}-{}", loaded.key.1, loaded.key.0),
                    image,
                    TextureOptions::LINEAR,
                )),
                None => Slot::Missing { hinted },
            };
            cached.slot = slot;
        }
        ctx.request_repaint();
    }
    #[cfg(test)]
    pub fn preload(&mut self, ctx: &Context, path: &str, size: u32, image: ColorImage) {
        self.slots.insert(
            (canonical(path), size),
            Cached {
                used: self.used,
                slot: Slot::Ready(ctx.load_texture(
                    format!("fixture-{size}-{path}"),
                    image,
                    TextureOptions::LINEAR,
                )),
            },
        );
    }
}

fn spawn(ctx: Context) -> Option<(SyncSender<Request>, Receiver<Loaded>)> {
    let (tx, requests) = mpsc::sync_channel::<Request>(MAX_PENDING);
    let (results, rx) = mpsc::sync_channel(MAX_PENDING);
    std::thread::Builder::new()
        .name("gamepause-icons".into())
        .spawn(move || {
            while let Ok(request) = requests.recv() {
                let executable = request
                    .hint
                    .map(PathBuf::from)
                    .filter(|path| path.is_file())
                    .or_else(|| executable(Path::new(&request.path)));
                let image = executable.and_then(|path| extract(&path, request.key.1 as i32));
                if results
                    .send(Loaded {
                        key: request.key,
                        image,
                    })
                    .is_err()
                {
                    break;
                }
                ctx.request_repaint();
            }
        })
        .ok()
        .map(|_| (tx, rx))
}

/// Pick the likely game executable: the largest one at the shallowest level that has any.
/// Bounded to two levels below the folder, with entry and folder caps. Links are not followed.
pub fn executable(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.into());
    }
    let mut budget = MAX_ENTRIES;
    let mut level = vec![path.to_path_buf()];
    for _ in 0..3 {
        let mut best: Option<(u64, PathBuf)> = None;
        let mut next = Vec::new();
        for folder in level.iter().take(MAX_FOLDERS) {
            let Ok(entries) = std::fs::read_dir(folder) else {
                continue;
            };
            for entry in entries.flatten() {
                if budget == 0 {
                    break;
                }
                budget -= 1;
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let name = entry.file_name().to_string_lossy().to_lowercase();
                if kind.is_dir() {
                    next.push(entry.path());
                } else if kind.is_file()
                    && name.ends_with(".exe")
                    && !NOT_GAMES.iter().any(|helper| name.contains(helper))
                {
                    let size = entry.metadata().map_or(0, |data| data.len());
                    if best.as_ref().is_none_or(|(largest, _)| size > *largest) {
                        best = Some((size, entry.path()));
                    }
                }
            }
        }
        if let Some((_, path)) = best {
            return Some(path);
        }
        level = next;
    }
    None
}

fn extract(executable: &Path, size: i32) -> Option<ColorImage> {
    // PrivateExtractIconsW reads a fixed MAX_PATH buffer.
    let mut name = wide(&executable.to_string_lossy());
    if name.len() > 260 {
        return None;
    }
    name.resize(260, 0);
    unsafe {
        let mut icon: HICON = null_mut();
        let found = PrivateExtractIconsW(name.as_ptr(), 0, size, size, &mut icon, null_mut(), 1, 0);
        if found == 0 || found == u32::MAX || icon.is_null() {
            return None;
        }
        let image = pixels(icon, size);
        DestroyIcon(icon);
        image
    }
}

unsafe fn pixels(icon: HICON, size: i32) -> Option<ColorImage> {
    unsafe {
        let mut info: ICONINFO = std::mem::zeroed();
        if GetIconInfo(icon, &mut info) == 0 {
            return None;
        }
        let mut header: BITMAPINFO = std::mem::zeroed();
        header.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        header.bmiHeader.biWidth = size;
        header.bmiHeader.biHeight = -size;
        header.bmiHeader.biPlanes = 1;
        header.bmiHeader.biBitCount = 32;
        header.bmiHeader.biCompression = BI_RGB;
        let mut bytes = vec![0u8; (size * size * 4) as usize];
        let dc = GetDC(null_mut());
        let lines = if info.hbmColor.is_null() || dc.is_null() {
            0
        } else {
            GetDIBits(
                dc,
                info.hbmColor,
                0,
                size as u32,
                bytes.as_mut_ptr().cast(),
                &mut header,
                DIB_RGB_COLORS,
            )
        };
        if !dc.is_null() {
            ReleaseDC(null_mut(), dc);
        }
        for bitmap in [info.hbmColor, info.hbmMask] {
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
        }
        if lines != size {
            return None;
        }
        // Icons without an alpha channel report zero alpha everywhere.
        let opaque = bytes.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0);
        for pixel in bytes.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            if opaque {
                pixel[3] = 255;
            }
        }
        Some(ColorImage::from_rgba_unmultiplied(
            [size as usize, size as usize],
            &bytes,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slow_loader_has_a_bounded_queue_and_texture_upload_budget() {
        let ctx = Context::default();
        let (tx, requests) = mpsc::sync_channel(MAX_PENDING);
        let (results, rx) = mpsc::sync_channel(MAX_PENDING);
        let mut cache = Cache {
            link: Some((tx, rx)),
            ..Default::default()
        };
        for index in 0..MAX_PENDING * 3 {
            cache.get(&ctx, &format!(r"D:\Fixture\{index}.exe"), None, 64);
        }
        assert_eq!(cache.slots.len(), MAX_PENDING);
        for request in requests.try_iter() {
            results
                .send(Loaded {
                    key: request.key,
                    image: Some(ColorImage::filled([2, 2], eframe::egui::Color32::WHITE)),
                })
                .unwrap();
        }
        for pass in 1..=MAX_PENDING / MAX_UPLOADS {
            cache.poll(&ctx);
            assert_eq!(
                cache
                    .slots
                    .values()
                    .filter(|cached| matches!(cached.slot, Slot::Ready(_)))
                    .count(),
                pass * MAX_UPLOADS
            );
        }
        cache.get(&ctx, r"D:\Fixture\deferred.exe", None, 64);
        assert_eq!(
            requests.try_iter().count(),
            1,
            "deferred requests remain eligible"
        );
    }
    #[test]
    fn cache_evicts_one_old_entry_and_keeps_recently_used_icons() {
        let ctx = Context::default();
        let (tx, requests) = mpsc::sync_channel(MAX_PENDING);
        let (results, rx) = mpsc::sync_channel(MAX_PENDING);
        let mut cache = Cache {
            link: Some((tx, rx)),
            ..Default::default()
        };
        let hot = r"D:\Fixture\hot.exe";
        for index in 0..MAX_ICONS + 50 {
            let path = if index == 0 {
                hot.into()
            } else {
                format!(r"D:\Fixture\{index}.exe")
            };
            cache.get(&ctx, &path, None, 64);
            let request = requests.try_recv().unwrap();
            results
                .send(Loaded {
                    key: request.key,
                    image: None,
                })
                .unwrap();
            cache.poll(&ctx);
            cache.get(&ctx, hot, None, 64);
            assert!(
                requests.try_recv().is_err(),
                "cached hits must not repeat extraction"
            );
            assert!(cache.slots.len() <= MAX_ICONS);
        }
        assert_eq!(cache.slots.len(), MAX_ICONS);
        assert!(cache.slots.contains_key(&(canonical(hot), 64)));
        assert!(
            !cache
                .slots
                .contains_key(&(canonical(r"D:\Fixture\1.exe"), 64))
        );
    }
    #[test]
    fn executable_search_prefers_shallow_large_games_and_stays_bounded() {
        let root = std::env::temp_dir().join(format!("gamepause-icons-{}", std::process::id()));
        let deep = root.join("bin").join("x64");
        let too_deep = root.join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(&too_deep).unwrap();
        std::fs::write(too_deep.join("hidden.exe"), [0; 64]).unwrap();
        assert_eq!(
            executable(&root),
            None,
            "three levels down is out of bounds"
        );
        std::fs::write(deep.join("small.exe"), [0; 8]).unwrap();
        std::fs::write(deep.join("game.exe"), [0; 32]).unwrap();
        std::fs::write(deep.join("CrashReporter.exe"), [0; 128]).unwrap();
        assert_eq!(executable(&root), Some(deep.join("game.exe")));
        std::fs::write(root.join("unins000.exe"), [0; 256]).unwrap();
        assert_eq!(executable(&root), Some(deep.join("game.exe")));
        std::fs::write(root.join("launcher.exe"), [0; 4]).unwrap();
        assert_eq!(executable(&root), Some(root.join("launcher.exe")));
        assert_eq!(
            executable(&deep.join("small.exe")),
            Some(deep.join("small.exe"))
        );
        assert_eq!(executable(&root.join("missing")), None);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn extracts_a_windows_executable_icon_at_the_requested_size() {
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let image = extract(&Path::new(&system).join("explorer.exe"), 48).unwrap();
        assert_eq!(image.size, [48, 48]);
        assert!(image.pixels.iter().any(|pixel| pixel.a() > 0));
        assert!(extract(Path::new(r"C:\gamepause-missing\none.exe"), 48).is_none());
    }
}
