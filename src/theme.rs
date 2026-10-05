//! UI-thread-only native client colors and painting. Native controls keep
//! their window classes, text, input handling and accessibility providers.
use std::rc::Rc;
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{
        Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
        WindowsAndMessaging::*,
    },
};

pub(crate) const BLUE: COLORREF = 0x00dca848;
pub(crate) const ORANGE: COLORREF = 0x003884ff;
pub(crate) const YELLOW: COLORREF = 0x007cdde8;

pub(crate) fn state_accent(activity: crate::control::Activity, dark: bool) -> COLORREF {
    use crate::control::Activity::*;
    if high_contrast() {
        return unsafe { GetSysColor(COLOR_HIGHLIGHT) };
    }
    match activity {
        Watching | Coexistence => on_color(dark),
        Paused | ManualHold | Countdown => off_color(dark),
        _ => {
            if dark {
                YELLOW
            } else {
                0x002c7485
            }
        }
    }
}
pub(crate) fn on_color(dark: bool) -> COLORREF {
    if dark { BLUE } else { 0x00755e15 }
}
pub(crate) fn off_color(dark: bool) -> COLORREF {
    if dark { ORANGE } else { 0x002061b0 }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Palette {
    pub dark: bool,
    pub color_words: bool,
    pub background: COLORREF,
    pub surface: COLORREF,
    pub text: COLORREF,
    pub disabled: COLORREF,
    pub border: COLORREF,
    pub selected: COLORREF,
    pub selected_text: COLORREF,
}

impl Palette {
    pub(crate) fn for_mode(dark: bool) -> Self {
        if dark {
            Self {
                dark: true,
                color_words: true,
                background: 0x00202020,
                surface: 0x00303030,
                text: 0x00eeeeee,
                disabled: 0x00aaaaaa,
                border: 0x00808080,
                selected: 0x00404040,
                selected_text: 0x00ffffff,
            }
        } else {
            unsafe {
                let color_words = !high_contrast();
                Self {
                    dark: false,
                    color_words,
                    background: GetSysColor(COLOR_WINDOW),
                    surface: GetSysColor(COLOR_BTNFACE),
                    text: GetSysColor(COLOR_WINDOWTEXT),
                    disabled: GetSysColor(COLOR_GRAYTEXT),
                    border: GetSysColor(if color_words {
                        COLOR_3DSHADOW
                    } else {
                        COLOR_WINDOWFRAME
                    }),
                    selected: GetSysColor(if color_words {
                        COLOR_BTNFACE
                    } else {
                        COLOR_HIGHLIGHT
                    }),
                    selected_text: GetSysColor(if color_words {
                        COLOR_WINDOWTEXT
                    } else {
                        COLOR_HIGHLIGHTTEXT
                    }),
                }
            }
        }
    }
}

pub(crate) fn high_contrast() -> bool {
    unsafe {
        let mut contrast: HIGHCONTRASTW = std::mem::zeroed();
        contrast.cbSize = std::mem::size_of::<HIGHCONTRASTW>() as u32;
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            &mut contrast as *mut _ as *mut _,
            0,
        ) == 0
            || contrast.dwFlags & HCF_HIGHCONTRASTON != 0
    }
}

pub(crate) fn effective_dark(appearance: crate::config::Appearance) -> bool {
    !high_contrast()
        && match appearance {
            crate::config::Appearance::System => crate::tray::system_is_dark(),
            crate::config::Appearance::Light => false,
            crate::config::Appearance::Dark => true,
        }
}
/// Rc clones keep brushes alive across nested native dispatch. No handles cross
/// to the worker and there is no UI borrow during resource deletion.
pub(crate) struct Theme {
    pub palette: Palette,
    pub background: HBRUSH,
    pub surface: HBRUSH,
}

impl Theme {
    pub(crate) fn new(dark: bool) -> Option<Rc<Self>> {
        let palette = Palette::for_mode(dark);
        unsafe {
            let background = CreateSolidBrush(palette.background);
            let surface = CreateSolidBrush(palette.surface);
            if background.is_null() || surface.is_null() {
                if !background.is_null() {
                    DeleteObject(background);
                }
                if !surface.is_null() {
                    DeleteObject(surface);
                }
                return None;
            }
            Some(Rc::new(Self {
                palette,
                background,
                surface,
            }))
        }
    }
}
impl Drop for Theme {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.background);
            DeleteObject(self.surface);
        }
    }
}

pub(crate) unsafe fn fill(dc: HDC, rect: &RECT, color: COLORREF) {
    unsafe {
        SetDCBrushColor(dc, color);
        FillRect(dc, rect, GetStockObject(DC_BRUSH) as HBRUSH);
    }
}
