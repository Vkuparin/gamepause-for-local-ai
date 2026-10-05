//! Read-only native Rich Edit details with colored pausing words.
//! Message IDs and ABI below are from the Windows SDK Richedit.h (pack 4).
use crate::{theme::Palette, wide};
use std::sync::OnceLock;
use windows_sys::Win32::{
    Foundation::*, Graphics::Gdi::*, System::LibraryLoader::*, UI::WindowsAndMessaging::*,
};
const EM_EXGETSEL: u32 = WM_USER + 52;
const EM_EXSETSEL: u32 = WM_USER + 55;
const EM_SETBKGNDCOLOR: u32 = WM_USER + 67;
const EM_SETCHARFORMAT: u32 = WM_USER + 68;
const EM_GETSCROLLPOS: u32 = WM_USER + 221;
const EM_SETSCROLLPOS: u32 = WM_USER + 222;
const SCF_SELECTION: usize = 1;
const SCF_ALL: usize = 4;
const CFM_COLOR: u32 = 0x40000000;
#[repr(C)]
#[derive(Default, Debug, PartialEq)]
struct CharRange {
    start: i32,
    end: i32,
}
#[repr(C)]
#[derive(Default)]
struct CharFormat {
    size: u32,
    mask: u32,
    effects: u32,
    height: i32,
    offset: i32,
    color: COLORREF,
    charset: u8,
    pitch: u8,
    face: [u16; 32],
}
pub(crate) fn initialize() -> bool {
    // Windows owns the DLL for this process's lifetime. Load only the OS copy.
    static LOADED: OnceLock<bool> = OnceLock::new();
    *LOADED.get_or_init(|| unsafe {
        !LoadLibraryExW(
            wide("Msftedit.dll").as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
        .is_null()
    })
}
/// Byte range of the metadata word, never a matching word in a game name/path.
pub(crate) fn pausing_word(label: &str) -> Option<(usize, usize, bool)> {
    let label = label.lines().next()?;
    let prefix = "  |  Automatic pausing ";
    let start = label.rfind(prefix)? + prefix.len();
    let suffix = &label[start..];
    for (word, on) in [("on", true), ("off", false)] {
        if suffix == word || suffix.starts_with(&format!("{word}  |  ")) {
            return Some((start, start + word.len(), on));
        }
    }
    None
}
pub(crate) fn format(hwnd: HWND, text: &str, palette: Palette) {
    unsafe {
        let mut selection = CharRange::default();
        let mut scroll = POINT { x: 0, y: 0 };
        SendMessageW(hwnd, EM_EXGETSEL, 0, &mut selection as *mut _ as isize);
        SendMessageW(hwnd, EM_GETSCROLLPOS, 0, &mut scroll as *mut _ as isize);
        let visible = IsWindowVisible(hwnd) != 0;
        if visible {
            SendMessageW(hwnd, WM_SETREDRAW, 0, 0);
        }
        SendMessageW(hwnd, EM_SETBKGNDCOLOR, 0, palette.background as isize);
        let mut style = CharFormat {
            size: std::mem::size_of::<CharFormat>() as u32,
            mask: CFM_COLOR,
            color: palette.text,
            ..Default::default()
        };
        SendMessageW(hwnd, EM_SETCHARFORMAT, SCF_ALL, &style as *const _ as isize);
        if palette.color_words
            && let Some((start, end, on)) = pausing_word(text)
        {
            let span = CharRange {
                start: text[..start].encode_utf16().count() as i32,
                end: text[..end].encode_utf16().count() as i32,
            };
            SendMessageW(hwnd, EM_EXSETSEL, 0, &span as *const _ as isize);
            style.color = if on {
                crate::theme::on_color(palette.dark)
            } else {
                crate::theme::off_color(palette.dark)
            };
            SendMessageW(
                hwnd,
                EM_SETCHARFORMAT,
                SCF_SELECTION,
                &style as *const _ as isize,
            );
        }
        SendMessageW(hwnd, EM_EXSETSEL, 0, &selection as *const _ as isize);
        SendMessageW(hwnd, EM_SETSCROLLPOS, 0, &scroll as *const _ as isize);
        if visible {
            SendMessageW(hwnd, WM_SETREDRAW, 1, 0);
        }
        InvalidateRect(hwnd, std::ptr::null(), 0);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pausing_word_uses_metadata_boundaries_and_utf8_names() {
        let text =
            "\u{1f3ae} on and off  |  Steam  |  Automatic pausing on  |  Running\r\nD:\\fixtures";
        let (start, end, on) = pausing_word(text).unwrap();
        assert_eq!((&text[start..end], on), ("on", true));
        assert!(pausing_word("on and off game").is_none());
        assert!(pausing_word("Game  |  Automatic pausing only").is_none());
        assert!(!pausing_word("Game  |  Automatic pausing off").unwrap().2);
    }
    #[test]
    fn native_rich_text_colors_metadata_and_preserves_selection_and_visibility() {
        assert!(initialize());
        assert_eq!(std::mem::size_of::<CharFormat>(), 92);
        unsafe {
            let parent = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                wide("Fixture").as_ptr(),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                600,
                400,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            let child = CreateWindowExW(
                0,
                wide("RICHEDIT50W").as_ptr(),
                wide("").as_ptr(),
                WS_CHILD | WS_VISIBLE | ES_MULTILINE as u32 | ES_READONLY as u32,
                0,
                0,
                500,
                150,
                parent,
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            assert!(!parent.is_null() && !child.is_null());
            for dark in [true, false] {
                for word in ["on", "off"] {
                    let text = format!(
                        "\u{1f3ae} fixture  |  Steam  |  Automatic pausing {word}\r\nPath: fixture.exe"
                    );
                    SetWindowTextW(child, wide(&text).as_ptr());
                    let selection = CharRange { start: 2, end: 8 };
                    SendMessageW(child, EM_EXSETSEL, 0, &selection as *const _ as isize);
                    format(child, &text, Palette::for_mode(dark));
                    let mut restored = CharRange::default();
                    SendMessageW(child, EM_EXGETSEL, 0, &mut restored as *mut _ as isize);
                    assert_eq!(restored, selection);
                    assert_eq!(
                        IsWindowVisible(parent),
                        0,
                        "formatting must not show the fixture"
                    );
                    let (start, end, on) = pausing_word(&text).unwrap();
                    let span = CharRange {
                        start: text[..start].encode_utf16().count() as i32,
                        end: text[..end].encode_utf16().count() as i32,
                    };
                    SendMessageW(child, EM_EXSETSEL, 0, &span as *const _ as isize);
                    let mut style = CharFormat {
                        size: 92,
                        ..Default::default()
                    };
                    SendMessageW(
                        child,
                        WM_USER + 58,
                        SCF_SELECTION,
                        &mut style as *mut _ as isize,
                    );
                    assert_eq!(
                        style.color,
                        if on {
                            crate::theme::on_color(dark)
                        } else {
                            crate::theme::off_color(dark)
                        }
                    );
                    assert_ne!(style.mask & CFM_COLOR, 0);
                }
            }
            DestroyWindow(parent);
        }
    }
}
