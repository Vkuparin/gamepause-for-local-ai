//! Native owner-drawn tray menus. Windows keeps command, keyboard and menu roles.
use crate::{theme, wide};
use std::{cell::RefCell, ptr::null_mut, rc::Rc};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{Controls::*, HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
};
struct Paint {
    rows: Vec<(usize, String, u32)>,
    theme: Rc<theme::Theme>,
    font: HFONT,
    dpi: i32,
    accent: COLORREF,
}
impl Drop for Paint {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.font);
        }
    }
}
thread_local! { static PAINT: RefCell<Option<Rc<Paint>>> = const { RefCell::new(None) }; }
pub(crate) struct Menu {
    pub handle: HMENU,
    paint: Option<Rc<Paint>>,
}
impl Drop for Menu {
    fn drop(&mut self) {
        unsafe {
            DestroyMenu(self.handle);
        }
        if self.paint.is_some() {
            PAINT.with(|p| {
                p.borrow_mut().take();
            });
        }
    }
}
pub(crate) fn create(
    hwnd: HWND,
    rows: Vec<(usize, String, u32)>,
    dark: bool,
    activity: crate::control::Activity,
) -> Option<Menu> {
    unsafe {
        let handle = CreatePopupMenu();
        if handle.is_null() {
            return None;
        }
        let rows = rows
            .into_iter()
            .filter(|(_, text, flags)| !text.is_empty() || flags & MF_SEPARATOR != 0)
            .collect::<Vec<_>>();
        if theme::high_contrast() {
            for (id, text, flags) in rows {
                if AppendMenuW(
                    handle,
                    MF_STRING | flags,
                    id,
                    wide(&text.replace('&', "&&")).as_ptr(),
                ) == 0
                {
                    DestroyMenu(handle);
                    return None;
                }
            }
            return Some(Menu {
                handle,
                paint: None,
            });
        }
        let Some(theme) = theme::Theme::new(dark) else {
            DestroyMenu(handle);
            return None;
        };
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
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
        if font.is_null() {
            DestroyMenu(handle);
            return None;
        }
        let paint = Rc::new(Paint {
            rows,
            theme,
            font,
            dpi,
            accent: theme::state_accent(activity, dark),
        });
        let menu = Menu {
            handle,
            paint: Some(paint.clone()),
        };
        PAINT.with(|p| *p.borrow_mut() = Some(paint.clone()));
        let info = MENUINFO {
            cbSize: std::mem::size_of::<MENUINFO>() as u32,
            fMask: MIM_BACKGROUND,
            hbrBack: paint.theme.background,
            ..std::mem::zeroed()
        };
        SetMenuInfo(handle, &info);
        for (index, (id, text, flags)) in paint.rows.iter().enumerate() {
            let mut text = wide(text);
            let item = MENUITEMINFOW {
                cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_FTYPE | MIIM_STATE | MIIM_ID | MIIM_DATA | MIIM_STRING,
                fType: MFT_OWNERDRAW
                    | if flags & MF_SEPARATOR != 0 {
                        MFT_SEPARATOR
                    } else {
                        0
                    },
                fState: (if flags & MF_GRAYED != 0 {
                    MFS_DISABLED
                } else {
                    0
                }) | (if flags & MF_CHECKED != 0 {
                    MFS_CHECKED
                } else {
                    0
                }),
                wID: *id as u32,
                dwItemData: index + 1,
                dwTypeData: text.as_mut_ptr(),
                ..std::mem::zeroed()
            };
            if InsertMenuItemW(handle, index as u32, 1, &item) == 0 {
                return None;
            }
        }
        Some(menu)
    }
}
pub(crate) unsafe fn measure(l: LPARAM) -> bool {
    unsafe {
        if l == 0 {
            return false;
        }
        let item = &mut *(l as *mut MEASUREITEMSTRUCT);
        if item.CtlType != ODT_MENU {
            return false;
        }
        let Some(paint) = PAINT.with(|p| p.borrow().clone()) else {
            return false;
        };
        let Some((_, label, flags)) = item.itemData.checked_sub(1).and_then(|i| paint.rows.get(i))
        else {
            return false;
        };
        let dc = CreateCompatibleDC(null_mut());
        if dc.is_null() {
            return false;
        }
        let old = SelectObject(dc, paint.font);
        let mut size: SIZE = std::mem::zeroed();
        let value = wide(label);
        GetTextExtentPoint32W(dc, value.as_ptr(), (value.len() - 1) as i32, &mut size);
        SelectObject(dc, old);
        DeleteDC(dc);
        item.itemWidth = (size.cx + crate::dashboard::scale(64, paint.dpi))
            .min(GetSystemMetrics(SM_CXSCREEN) - 32)
            .max(120) as u32;
        item.itemHeight = if flags & MF_SEPARATOR != 0 {
            crate::dashboard::scale(8, paint.dpi)
        } else {
            size.cy + crate::dashboard::scale(10, paint.dpi)
        } as u32;
        true
    }
}
pub(crate) unsafe fn draw(l: LPARAM) -> bool {
    unsafe {
        if l == 0 {
            return false;
        }
        let item = &*(l as *const DRAWITEMSTRUCT);
        if item.CtlType != ODT_MENU {
            return false;
        }
        let Some(paint) = PAINT.with(|p| p.borrow().clone()) else {
            return false;
        };
        let Some((_, label, flags)) = item.itemData.checked_sub(1).and_then(|i| paint.rows.get(i))
        else {
            return false;
        };
        let saved = SaveDC(item.hDC);
        if saved == 0 {
            return false;
        }
        let palette = paint.theme.palette;
        let selected = item.itemState & ODS_SELECTED != 0;
        theme::fill(
            item.hDC,
            &item.rcItem,
            if selected {
                palette.surface
            } else {
                palette.background
            },
        );
        let padding = crate::dashboard::scale(10, paint.dpi);
        if flags & MF_SEPARATOR != 0 {
            let rect = RECT {
                left: item.rcItem.left + padding,
                right: item.rcItem.right - padding,
                top: (item.rcItem.top + item.rcItem.bottom) / 2,
                bottom: (item.rcItem.top + item.rcItem.bottom) / 2 + 1,
            };
            theme::fill(item.hDC, &rect, palette.border);
        } else {
            SelectObject(item.hDC, paint.font);
            SetBkMode(item.hDC, TRANSPARENT as i32);
            SetTextColor(
                item.hDC,
                if item.itemData == 1 {
                    paint.accent
                } else if flags & MF_GRAYED != 0 {
                    palette.disabled
                } else {
                    palette.text
                },
            );
            let mut text = item.rcItem;
            text.left += crate::dashboard::scale(30, paint.dpi);
            text.right -= padding;
            DrawTextW(
                item.hDC,
                wide(label).as_ptr(),
                -1,
                &mut text,
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
            );
            if flags & MF_CHECKED != 0 {
                let mut check = item.rcItem;
                check.left += padding;
                check.right = text.left - padding / 2;
                DrawTextW(
                    item.hDC,
                    wide("\u{2713}").as_ptr(),
                    -1,
                    &mut check,
                    DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
                );
            }
            if selected {
                let stripe = RECT {
                    right: item.rcItem.left + 3,
                    ..item.rcItem
                };
                theme::fill(item.hDC, &stripe, paint.accent);
            }
        }
        RestoreDC(item.hDC, saved);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn themed_menus_keep_native_labels_flags_and_draw_both_palettes() {
        unsafe {
            for dark in [true, false] {
                let menu = create(
                    null_mut(),
                    vec![
                        (0, "GamePause state".into(), MF_GRAYED),
                        (101, "Automatic pausing".into(), MF_CHECKED),
                        (0, "".into(), MF_SEPARATOR),
                        (113, "Resume AI".into(), 0),
                    ],
                    dark,
                    crate::control::Activity::Restoring,
                )
                .unwrap();
                let mut label = [0u16; 64];
                let mut info = MENUITEMINFOW {
                    cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                    fMask: MIIM_STRING | MIIM_FTYPE | MIIM_STATE | MIIM_ID | MIIM_DATA,
                    dwTypeData: label.as_mut_ptr(),
                    cch: 64,
                    ..std::mem::zeroed()
                };
                assert_ne!(GetMenuItemInfoW(menu.handle, 1, 1, &mut info), 0);
                assert_eq!(
                    String::from_utf16_lossy(&label[..info.cch as usize]),
                    "Automatic pausing"
                );
                assert_eq!(info.wID, 101);
                assert_ne!(info.fState & MFS_CHECKED, 0);
                let dc = CreateCompatibleDC(null_mut());
                let mut bits = null_mut();
                let bitmap_info = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: 40,
                        biWidth: 600,
                        biHeight: -40,
                        biPlanes: 1,
                        biBitCount: 32,
                        ..std::mem::zeroed()
                    },
                    ..std::mem::zeroed()
                };
                let bitmap =
                    CreateDIBSection(dc, &bitmap_info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
                let old = SelectObject(dc, bitmap);
                if !theme::high_contrast() {
                    assert_ne!(info.fType & MFT_OWNERDRAW, 0);
                    let mut measured = MEASUREITEMSTRUCT {
                        CtlType: ODT_MENU,
                        itemData: info.dwItemData,
                        ..std::mem::zeroed()
                    };
                    assert!(measure(&mut measured as *mut _ as isize));
                    assert!(measured.itemWidth > 100 && measured.itemHeight > 16);
                    let item = DRAWITEMSTRUCT {
                        CtlType: ODT_MENU,
                        itemData: info.dwItemData,
                        hDC: dc,
                        rcItem: RECT {
                            left: 0,
                            top: 0,
                            right: 600,
                            bottom: 40,
                        },
                        ..std::mem::zeroed()
                    };
                    SetTextColor(dc, 0x00123456);
                    SetBkMode(dc, OPAQUE as i32);
                    assert!(draw(&item as *const _ as isize));
                    assert_eq!(
                        GetPixel(dc, 590, 5),
                        theme::Palette::for_mode(dark).background
                    );
                    assert_eq!(GetTextColor(dc), 0x00123456);
                    assert_eq!(GetBkMode(dc), OPAQUE as i32);
                }
                SelectObject(dc, old);
                DeleteObject(bitmap);
                DeleteDC(dc);
                drop(menu);
                assert!(PAINT.with(|p| p.borrow().is_none()));
            }
        }
    }
}
