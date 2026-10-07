//! Path comparison shared by discovery, detection and the settings rules:
//! case- and separator-insensitive, without touching the filesystem.

use std::path::PathBuf;
pub fn canonical(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}
/// `canonical` one character at a time, for comparisons on the scan path
/// that must not allocate.
pub(super) fn canonical_chars(path: &str) -> impl Iterator<Item = char> + '_ {
    path.trim_end_matches(['\\', '/'])
        .chars()
        .map(|c| if c == '/' { '\\' } else { c })
        .flat_map(char::to_lowercase)
}
/// Whether two paths are the same location under `canonical`.
pub fn same_path(a: &str, b: &str) -> bool {
    canonical_chars(a).eq(canonical_chars(b))
}
pub fn inside(path: &str, root: &str) -> bool {
    let mut path = canonical_chars(path);
    let mut any = false;
    for expected in canonical_chars(root) {
        any = true;
        if path.next() != Some(expected) {
            return false;
        }
    }
    // The root itself, or something below it; never a longer sibling name.
    any && matches!(path.next(), None | Some('\\'))
}
pub(super) fn env_path(key: &str, fallback: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(key).unwrap_or_else(|| fallback.into()))
}
/// Root folders of local fixed disks. Network, removable and optical drives
/// are left alone: probing them can block or wake hardware for nothing.
pub(super) fn fixed_drives() -> Vec<PathBuf> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    const DRIVE_FIXED: u32 = 3;
    let present = unsafe { GetLogicalDrives() };
    (b'A'..=b'Z')
        .filter(|letter| present & (1 << (letter - b'A')) != 0)
        .map(|letter| format!("{}:\\", letter as char))
        .filter(|root| unsafe { GetDriveTypeW(crate::wide(root).as_ptr()) } == DRIVE_FIXED)
        .map(PathBuf::from)
        .collect()
}
