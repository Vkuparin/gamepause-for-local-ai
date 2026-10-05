//! UI-thread-only native client colors and painting. Native controls keep
//! their window classes, text, input handling and accessibility providers.
use crate::wide;
use std::{ptr::null_mut, rc::Rc};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{
        Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
        Controls::*,
        HiDpi::GetDpiForWindow,
        Input::KeyboardAndMouse::*,
        Shell::*,
        WindowsAndMessaging::*,
    },
};

const SUBCLASS: usize = 0x47505448;
const ACCENT_PROPERTY: &str = "GamePause.Accent";
const KIND_PROPERTY: &str = "GamePause.ButtonKind";
const DARK_PROPERTY: &str = "GamePause.DarkClient";

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
pub(crate) unsafe fn accent(hwnd: HWND) -> COLORREF {
    unsafe {
        let value = GetPropW(hwnd, wide(ACCENT_PROPERTY).as_ptr()) as usize;
        if value == 0 {
            BLUE
        } else {
            (value - 1) as COLORREF
        }
    }
}
pub(crate) unsafe fn set_accent(hwnd: HWND, color: COLORREF) {
    unsafe {
        if GetPropW(hwnd, wide(ACCENT_PROPERTY).as_ptr()) as usize != color as usize + 1 {
            SetPropW(
                hwnd,
                wide(ACCENT_PROPERTY).as_ptr(),
                (color as usize + 1) as HANDLE,
            );
            InvalidateRect(hwnd, std::ptr::null(), 0);
        }
    }
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
    pub dark: bool,
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
                dark,
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

/// Keep native classes, input, tab order and captions; push buttons use owner
/// drawing outside high contrast. Refdata is an integer flag, not an allocation.
pub(crate) unsafe fn apply_control(hwnd: HWND, dark: bool, navigation: bool) -> bool {
    unsafe {
        let installed = SetWindowSubclass(
            hwnd,
            Some(control_proc),
            SUBCLASS,
            usize::from(dark)
                | (usize::from(navigation) << 1)
                | (usize::from(!high_contrast()) << 2),
        ) != 0;
        if installed && !navigation {
            BufferedPaintStopAllAnimations(hwnd);
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            let kind = style & BS_TYPEMASK as u32;
            let stored = GetPropW(hwnd, wide(KIND_PROPERTY).as_ptr()) as usize;
            if !high_contrast()
                && matches!(kind, k if k == BS_PUSHBUTTON as u32 || k == BS_DEFPUSHBUTTON as u32)
            {
                SetPropW(
                    hwnd,
                    wide(KIND_PROPERTY).as_ptr(),
                    (kind as usize + 1) as HANDLE,
                );
                SetWindowLongPtrW(
                    hwnd,
                    GWL_STYLE,
                    ((style & !(BS_TYPEMASK as u32)) | BS_OWNERDRAW as u32) as isize,
                );
            } else if high_contrast() && stored != 0 {
                SetWindowLongPtrW(
                    hwnd,
                    GWL_STYLE,
                    ((style & !(BS_TYPEMASK as u32)) | (stored - 1) as u32) as isize,
                );
                RemovePropW(hwnd, wide(KIND_PROPERTY).as_ptr());
            }
        }
        installed
    }
}

unsafe extern "system" fn control_proc(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    _: usize,
    data: usize,
) -> LRESULT {
    // Never unwind through ComCtl32. Native dispatch remains available after
    // a contained painting failure.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let dark = data & 1 != 0;
        let group = data & 2 == 0
            && GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & BS_TYPEMASK as u32 == BS_GROUPBOX as u32;
        match msg {
            WM_NCDESTROY => {
                RemovePropW(hwnd, wide(DARK_PROPERTY).as_ptr());
                RemovePropW(hwnd, wide(KIND_PROPERTY).as_ptr());
                RemovePropW(hwnd, wide(ACCENT_PROPERTY).as_ptr());
                RemoveWindowSubclass(hwnd, Some(control_proc), SUBCLASS);
                DefSubclassProc(hwnd, msg, w, l)
            }
            WM_PAINT | WM_PRINTCLIENT if dark || group || data & 4 != 0 => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let dc = if msg == WM_PAINT {
                    BeginPaint(hwnd, &mut ps)
                } else {
                    w as HDC
                };
                if !dc.is_null() {
                    paint_control(hwnd, dc, data & 2 != 0, dark);
                }
                if msg == WM_PAINT {
                    EndPaint(hwnd, &ps);
                }
                0
            }
            WM_ERASEBKGND if dark || group || data & 4 != 0 => 1,
            WM_MOUSEMOVE if dark || data & 4 != 0 => {
                if GetPropW(hwnd, wide(DARK_PROPERTY).as_ptr()).is_null() {
                    SetPropW(hwnd, wide(DARK_PROPERTY).as_ptr(), 1usize as HANDLE);
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    TrackMouseEvent(&mut track);
                    InvalidateRect(hwnd, std::ptr::null(), 0);
                }
                DefSubclassProc(hwnd, msg, w, l)
            }
            WM_MOUSELEAVE => {
                RemovePropW(hwnd, wide(DARK_PROPERTY).as_ptr());
                InvalidateRect(hwnd, std::ptr::null(), 0);
                DefSubclassProc(hwnd, msg, w, l)
            }
            WM_SETFOCUS | WM_KILLFOCUS | WM_ENABLE | WM_UPDATEUISTATE | BM_SETCHECK
            | BM_SETSTATE | TCM_SETCURSEL => {
                let result = DefSubclassProc(hwnd, msg, w, l);
                if msg == BM_SETCHECK {
                    BufferedPaintStopAllAnimations(hwnd);
                }
                InvalidateRect(hwnd, std::ptr::null(), 0);
                result
            }
            _ => DefSubclassProc(hwnd, msg, w, l),
        }
    }))
    .unwrap_or_else(|_| unsafe { DefSubclassProc(hwnd, msg, w, l) })
}

pub(crate) unsafe fn fill(dc: HDC, rect: &RECT, color: COLORREF) {
    unsafe {
        SetDCBrushColor(dc, color);
        FillRect(dc, rect, GetStockObject(DC_BRUSH) as HBRUSH);
    }
}

unsafe fn frame(dc: HDC, rect: &RECT, color: COLORREF) {
    unsafe {
        SetDCBrushColor(dc, color);
        FrameRect(dc, rect, GetStockObject(DC_BRUSH) as HBRUSH);
    }
}

pub(crate) unsafe fn paint_control(hwnd: HWND, dc: HDC, navigation: bool, dark: bool) {
    unsafe {
        let saved = SaveDC(dc);
        if saved == 0 {
            return;
        }
        let mut palette = Palette::for_mode(dark);
        palette.selected = accent(hwnd);
        palette.selected_text = if dark { 0x00202020 } else { 0x00ffffff };
        let mut rect: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut rect);
        fill(dc, &rect, palette.background);
        let font = SendMessageW(hwnd, WM_GETFONT, 0, 0) as HFONT;
        if !font.is_null() {
            SelectObject(dc, font);
        }
        SetBkMode(dc, TRANSPARENT as i32);
        let enabled = IsWindowEnabled(hwnd) != 0;
        SetTextColor(
            dc,
            if enabled {
                palette.text
            } else {
                palette.disabled
            },
        );
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let padding = crate::dashboard::scale(8, dpi);
        let hide_focus = SendMessageW(hwnd, WM_QUERYUISTATE, 0, 0) & UISF_HIDEFOCUS as isize != 0;
        let hide_accel = SendMessageW(hwnd, WM_QUERYUISTATE, 0, 0) & UISF_HIDEACCEL as isize != 0;
        let prefix = if hide_accel { DT_HIDEPREFIX } else { 0 };
        if navigation {
            let selected = SendMessageW(hwnd, TCM_GETCURSEL, 0, 0);
            let count = SendMessageW(hwnd, TCM_GETITEMCOUNT, 0, 0).clamp(0, 32);
            for index in 0..count {
                let mut tab: RECT = std::mem::zeroed();
                if SendMessageW(
                    hwnd,
                    TCM_GETITEMRECT,
                    index as usize,
                    &mut tab as *mut _ as isize,
                ) == 0
                {
                    continue;
                }
                let mut buffer = [0u16; 256];
                let mut item: TCITEMW = std::mem::zeroed();
                item.mask = TCIF_TEXT;
                item.pszText = buffer.as_mut_ptr();
                item.cchTextMax = buffer.len() as i32;
                SendMessageW(
                    hwnd,
                    TCM_GETITEMW,
                    index as usize,
                    &mut item as *mut _ as isize,
                );
                let active = index == selected;
                fill(
                    dc,
                    &tab,
                    if active {
                        palette.selected
                    } else {
                        palette.surface
                    },
                );
                frame(dc, &tab, palette.border);
                SetTextColor(
                    dc,
                    if !enabled {
                        palette.disabled
                    } else if active {
                        palette.selected_text
                    } else {
                        palette.text
                    },
                );
                DrawTextW(
                    dc,
                    buffer.as_ptr(),
                    -1,
                    &mut tab,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE | prefix,
                );
                if active && GetFocus() == hwnd && !hide_focus {
                    InflateRect(&mut tab, -3, -3);
                    DrawFocusRect(dc, &tab);
                }
            }
        } else {
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            let kind = style & BS_TYPEMASK as u32;
            let state = SendMessageW(hwnd, BM_GETSTATE, 0, 0) as u32;
            let mut buffer = [0u16; 512];
            GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
            if kind == BS_GROUPBOX as u32 {
                let mut border = rect;
                border.top += crate::dashboard::scale(10, dpi);
                frame(dc, &border, palette.border);
                let mut size: SIZE = std::mem::zeroed();
                let count = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
                GetTextExtentPoint32W(dc, buffer.as_ptr(), count as i32, &mut size);
                let mut label = RECT {
                    left: padding,
                    top: 0,
                    right: padding + size.cx + padding,
                    bottom: size.cy,
                };
                fill(dc, &label, palette.background);
                label.left += padding / 2;
                DrawTextW(dc, buffer.as_ptr(), -1, &mut label, DT_SINGLELINE | prefix);
            } else if matches!(kind, k if k == BS_AUTOCHECKBOX as u32 || k == BS_CHECKBOX as u32) {
                let side = crate::dashboard::scale(16, dpi);
                let box_rect = RECT {
                    left: 0,
                    top: (rect.bottom - side) / 2,
                    right: side,
                    bottom: (rect.bottom + side) / 2,
                };
                fill(dc, &box_rect, palette.surface);
                frame(
                    dc,
                    &box_rect,
                    if enabled {
                        palette.border
                    } else {
                        palette.disabled
                    },
                );
                if SendMessageW(hwnd, BM_GETCHECK, 0, 0) == BST_CHECKED as isize {
                    let pen = CreatePen(
                        PS_SOLID,
                        crate::dashboard::scale(2, dpi),
                        if enabled {
                            palette.text
                        } else {
                            palette.disabled
                        },
                    );
                    if !pen.is_null() {
                        let old = SelectObject(dc, pen);
                        MoveToEx(
                            dc,
                            box_rect.left + side / 5,
                            box_rect.top + side / 2,
                            null_mut(),
                        );
                        LineTo(
                            dc,
                            box_rect.left + side * 2 / 5,
                            box_rect.top + side * 3 / 4,
                        );
                        LineTo(dc, box_rect.left + side * 4 / 5, box_rect.top + side / 4);
                        SelectObject(dc, old);
                        DeleteObject(pen);
                    }
                }
                rect.left = side + padding;
                DrawTextW(
                    dc,
                    buffer.as_ptr(),
                    -1,
                    &mut rect,
                    DT_SINGLELINE | DT_VCENTER | prefix,
                );
            } else {
                let hot = !GetPropW(hwnd, wide(DARK_PROPERTY).as_ptr()).is_null();
                fill(
                    dc,
                    &rect,
                    if enabled && state & BST_PUSHED != 0 {
                        palette.selected
                    } else if enabled && hot {
                        if dark {
                            0x00404040
                        } else {
                            GetSysColor(COLOR_3DLIGHT)
                        }
                    } else {
                        palette.surface
                    },
                );
                frame(
                    dc,
                    &rect,
                    if enabled && [113, 114].contains(&GetDlgCtrlID(hwnd)) {
                        accent(hwnd)
                    } else if kind == BS_DEFPUSHBUTTON as u32 {
                        palette.text
                    } else {
                        palette.border
                    },
                );
                // The narrowest measured caption has eight logical pixels
                // total horizontal padding, not eight on each side.
                rect.left += padding / 2;
                rect.right -= padding / 2;
                DrawTextW(
                    dc,
                    buffer.as_ptr(),
                    -1,
                    &mut rect,
                    DT_CENTER | DT_SINGLELINE | DT_VCENTER | prefix,
                );
            }
            if kind != BS_GROUPBOX as u32 && GetFocus() == hwnd && !hide_focus {
                InflateRect(&mut rect, -3, -3);
                DrawFocusRect(dc, &rect);
            }
        }
        RestoreDC(dc, saved);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

    #[test]
    fn state_accents_keep_paused_and_transitional_work_distinct() {
        use crate::control::Activity;
        for dark in [true, false] {
            for state in [Activity::Paused, Activity::ManualHold, Activity::Countdown] {
                assert_eq!(state_accent(state, dark), off_color(dark));
            }
            for state in [
                Activity::Capturing,
                Activity::WaitingForInference,
                Activity::Unloading,
                Activity::Restoring,
                Activity::Verifying,
            ] {
                assert_ne!(state_accent(state, dark), on_color(dark));
                assert_ne!(state_accent(state, dark), off_color(dark));
            }
        }
    }

    #[test]
    fn native_dark_controls_preserve_text_state_and_dc_at_each_dpi() {
        unsafe {
            let controls = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_TAB_CLASSES,
            };
            assert_ne!(InitCommonControlsEx(&controls), 0);
            let parent = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                wide("Theme fixture").as_ptr(),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                400,
                240,
                null_mut(),
                null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            assert!(!parent.is_null());
            let dc = CreateCompatibleDC(null_mut());
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = 400;
            info.bmiHeader.biHeight = -240;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            let mut bits = null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            assert!(!bitmap.is_null());
            let old_bitmap = SelectObject(dc, bitmap);
            for dpi in [96, 144, 192] {
                let font = CreateFontW(
                    -crate::dashboard::scale(16, dpi),
                    0,
                    0,
                    0,
                    400,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET as u32,
                    0,
                    0,
                    CLEARTYPE_QUALITY as u32,
                    0,
                    wide("Segoe UI").as_ptr(),
                );
                for (class, style, navigation) in [
                    ("BUTTON", BS_PUSHBUTTON, false),
                    ("BUTTON", BS_AUTOCHECKBOX, false),
                    ("BUTTON", BS_GROUPBOX, false),
                    ("SysTabControl32", 0, true),
                ] {
                    let child = CreateWindowExW(
                        0,
                        wide(class).as_ptr(),
                        wide("Fixture caption").as_ptr(),
                        WS_CHILD | WS_TABSTOP | style as u32,
                        0,
                        0,
                        320,
                        80,
                        parent,
                        null_mut(),
                        GetModuleHandleW(std::ptr::null()),
                        std::ptr::null(),
                    );
                    assert!(!child.is_null());
                    assert!(apply_control(child, true, navigation));
                    SendMessageW(child, WM_SETFONT, font as usize, 0);
                    if navigation {
                        let mut caption = wide("Fixture tab");
                        let mut tab: TCITEMW = std::mem::zeroed();
                        tab.mask = TCIF_TEXT;
                        tab.pszText = caption.as_mut_ptr();
                        assert_eq!(
                            SendMessageW(child, TCM_INSERTITEMW, 0, &tab as *const _ as isize),
                            0
                        );
                    }
                    SetBkMode(dc, OPAQUE as i32);
                    SetBkColor(dc, 0x00123456);
                    SetTextColor(dc, 0x00654321);
                    let old_font = GetCurrentObject(dc, OBJ_FONT as u32);
                    for enabled in [true, false] {
                        EnableWindow(child, i32::from(enabled));
                        SendMessageW(child, WM_PRINTCLIENT, dc as usize, PRF_CLIENT as isize);
                        assert_eq!(
                            (
                                GetBkMode(dc),
                                GetBkColor(dc),
                                GetTextColor(dc),
                                GetCurrentObject(dc, OBJ_FONT as u32)
                            ),
                            (OPAQUE as i32, 0x00123456, 0x00654321, old_font)
                        );
                        if !navigation && style == BS_PUSHBUTTON {
                            assert_eq!(GetPixel(dc, 10, 10), Palette::for_mode(true).surface);
                        }
                    }
                    EnableWindow(child, 1);
                    if style == BS_AUTOCHECKBOX && !navigation {
                        // Native BM_CLICK toggles the checkbox and sends its
                        // native notification even with replacement painting.
                        SendMessageW(child, BM_CLICK, 0, 0);
                        assert_eq!(SendMessageW(child, BM_GETCHECK, 0, 0), BST_CHECKED as isize);
                        SendMessageW(child, WM_PRINTCLIENT, dc as usize, PRF_CLIENT as isize);
                    }
                    if navigation {
                        assert_eq!(SendMessageW(child, TCM_GETCURSEL, 0, 0), 0);
                    } else {
                        let mut text = [0u16; 64];
                        GetWindowTextW(child, text.as_mut_ptr(), 64);
                        assert!(String::from_utf16_lossy(&text).starts_with("Fixture caption"));
                        assert_eq!(
                            GetWindowLongPtrW(child, GWL_STYLE) as u32 & BS_TYPEMASK as u32,
                            if style == BS_PUSHBUTTON {
                                BS_OWNERDRAW as u32
                            } else {
                                style as u32
                            }
                        );
                    }
                    // Replacing the same subclass switches the palette without
                    // replacing the window or changing its input/check state.
                    assert!(apply_control(child, false, navigation));
                    assert_ne!(DestroyWindow(child), 0);
                }
                DeleteObject(font);
            }
            SelectObject(dc, old_bitmap);
            DeleteObject(bitmap);
            DeleteDC(dc);
            DestroyWindow(parent);
        }
    }

    #[test]
    fn theme_brushes_match_palette_and_last_snapshot_owns_lifetime() {
        unsafe {
            for dark in [false, true] {
                let theme = Theme::new(dark).unwrap();
                let held = theme.clone();
                let weak = Rc::downgrade(&theme);
                let handle = theme.background;
                drop(theme);
                let mut brush: LOGBRUSH = std::mem::zeroed();
                assert_ne!(
                    GetObjectW(
                        handle,
                        std::mem::size_of::<LOGBRUSH>() as i32,
                        &mut brush as *mut _ as *mut _
                    ),
                    0
                );
                assert_eq!(brush.lbColor, held.palette.background);
                drop(held);
                // A deleted solid brush can still be a valid cached/stock GDI
                // handle. Check ownership, rather than inspecting stale handles.
                assert!(weak.upgrade().is_none());
            }
        }
    }
}
