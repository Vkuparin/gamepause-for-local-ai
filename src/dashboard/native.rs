//! Win32 pieces of the dashboard window: its options and icon, the caption
//! tint, the window handle and the folder picker. UI thread only.

use crate::{
    dashboard_theme::{Look, Palette},
    wide,
};
use eframe::egui::{Color32, IconData, ViewportBuilder};
use windows_sys::Win32::{Foundation::HWND, UI::Controls::Dialogs::*};
use winit::platform::windows::EventLoopBuilderExtWindows;
pub(super) fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size([1114.0, 848.0])
            .with_min_inner_size([620.0, 580.0])
            .with_icon(app_icon(
                Palette::for_mode(true, false, Look::default()).accent,
            )),
        renderer: eframe::Renderer::Glow,
        event_loop_builder: Some(Box::new(|builder| {
            builder.with_any_thread(true);
        })),
        ..Default::default()
    }
}
/// The window icon is the pause mark in the current state color.
pub(super) fn app_icon(color: Color32) -> IconData {
    let mut rgba = vec![0; 32 * 32 * 4];
    for y in 4..28 {
        for x in (7..13).chain(19..25) {
            let i = (y * 32 + x) * 4;
            rgba[i..i + 4].copy_from_slice(&[color.r(), color.g(), color.b(), 255]);
        }
    }
    IconData {
        rgba,
        width: 32,
        height: 32,
    }
}
/// Tint the native caption. Windows 10 ignores the color attributes and keeps its own bar.
pub(super) fn caption(hwnd: HWND, palette: Palette, dark: bool) {
    use windows_sys::Win32::Graphics::Dwm::*;
    const SYSTEM: u32 = 0xFFFF_FFFF;
    let set = |attribute: DWMWINDOWATTRIBUTE, value: u32| unsafe {
        DwmSetWindowAttribute(hwnd, attribute as u32, (&raw const value).cast(), 4);
    };
    let (bar, text) = palette.caption.map_or((SYSTEM, SYSTEM), |c| {
        (
            c.r() as u32 | (c.g() as u32) << 8 | (c.b() as u32) << 16,
            0x00FF_FFFF,
        )
    });
    set(DWMWA_USE_IMMERSIVE_DARK_MODE, dark as u32);
    set(DWMWA_CAPTION_COLOR, bar);
    set(DWMWA_TEXT_COLOR, text);
}
pub(super) fn native_window(frame: &eframe::Frame) -> Option<HWND> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
        _ => None,
    }
}
pub(super) fn browse(hwnd: HWND) -> anyhow::Result<Option<String>> {
    unsafe {
        let mut buffer = vec![0u16; 32768];
        let filter = wide("Windows executable (*.exe)\0*.exe\0\0");
        let mut dialog: OPENFILENAMEW = std::mem::zeroed();
        dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        dialog.hwndOwner = hwnd;
        dialog.lpstrFilter = filter.as_ptr();
        dialog.lpstrFile = buffer.as_mut_ptr();
        dialog.nMaxFile = buffer.len() as u32;
        dialog.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
        if GetOpenFileNameW(&mut dialog) == 0 {
            let error = CommDlgExtendedError();
            if error != 0 {
                anyhow::bail!("File picker failed (code {error})");
            }
            return Ok(None);
        }
        let n = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        Ok(Some(String::from_utf16_lossy(&buffer[..n])))
    }
}
