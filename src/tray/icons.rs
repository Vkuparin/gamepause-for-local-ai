//! The state-colored tray icons. Created and destroyed on the tray UI thread.

use super::StateKind;
use windows_sys::Win32::{
    Graphics::Gdi::{CreateBitmap, DeleteObject},
    UI::WindowsAndMessaging::{CreateIconIndirect, DestroyIcon, HICON, ICONINFO},
};
/// The three state-colored tray icons, precomputed once so the timer path
/// never calls GDI. `Idle` is the base glyph; `Paused` and `Attention` are
/// tinted variants (P1-6).
#[derive(Clone)]
pub(super) struct Icons {
    pub(super) idle: HICON,
    pub(super) paused: HICON,
    pub(super) attention: HICON,
}
impl Icons {
    pub(super) fn for_kind(&self, kind: StateKind) -> HICON {
        match kind {
            StateKind::Idle => self.idle,
            StateKind::Paused => self.paused,
            StateKind::Attention => self.attention,
        }
    }
    pub(super) unsafe fn destroy(&self) {
        unsafe {
            if !self.idle.is_null() {
                DestroyIcon(self.idle);
            }
            if !self.paused.is_null() {
                DestroyIcon(self.paused);
            }
            if !self.attention.is_null() {
                DestroyIcon(self.attention);
            }
        }
    }
}
/// The tray glyph's solid fill color per state. Pure + unit-testable: this is
/// the "state -> icon variant" mapping P1-6 wants asserted without Win32.
/// Bytes are in bitmap order, blue-green-red: `Idle` is blue, `Paused` is
/// orange and `Attention` is a warning red that pops against the dark tray.
pub fn icon_tint(kind: StateKind) -> [u8; 3] {
    match kind {
        StateKind::Idle => [220, 168, 72],
        StateKind::Paused => [56, 132, 255],
        StateKind::Attention => [62, 62, 230],
    }
}
/// Draw the pause-bar glyph in `color` and return an HICON. The bars stay
/// white so the state is carried by the background tint alone.
pub(super) unsafe fn icon(color: [u8; 3]) -> HICON {
    let mut pixels = vec![0u8; 32 * 32 * 4];
    for y in 0..32 {
        for x in 0..32 {
            let offset = (y * 32 + x) * 4;
            if (4..28).contains(&x) && (4..28).contains(&y) {
                pixels[offset..offset + 4].copy_from_slice(&[color[0], color[1], color[2], 255]);
            }
            if (9..23).contains(&y) && ((10..14).contains(&x) || (18..22).contains(&x)) {
                pixels[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    unsafe {
        let color = CreateBitmap(32, 32, 1, 32, pixels.as_ptr().cast());
        let mask = CreateBitmap(32, 32, 1, 1, [0u8; 128].as_ptr().cast());
        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let result = CreateIconIndirect(&info);
        DeleteObject(color);
        DeleteObject(mask);
        result
    }
}
