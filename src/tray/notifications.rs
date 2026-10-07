//! Delivers queued notifications through the tray icon: the balloon text,
//! severity and sound for each state. `crate::notifications` owns the policy.

use super::{StateKind, state_toast};
use windows_sys::Win32::{
    Foundation::HWND,
    System::Diagnostics::Debug::MessageBeep,
    UI::{
        Shell::{NIIF_ERROR, NIIF_INFO, NIIF_NOSOUND, NIIF_RESPECT_QUIET_TIME},
        WindowsAndMessaging::{MB_ICONASTERISK, MB_OK},
    },
};
/// Toast severity. Pure data so the mapping is unit-testable (P1-5 acceptance).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    /// Pause-success or restore-success — the "good" system sound.
    Success,
    /// Any failure — the warning system sound.
    Error,
}
/// Titles and severity for typed completion/failure events. The bounded queue
/// supplies factual completion text or the current failure message.
pub struct ToastSpec {
    pub title: &'static str,
    pub severity: Severity,
}
pub fn toast_spec(kind: StateKind) -> ToastSpec {
    match kind {
        StateKind::Attention => ToastSpec {
            title: "GamePause needs attention",
            severity: Severity::Error,
        },
        StateKind::Paused => ToastSpec {
            title: "GamePause",
            severity: Severity::Success,
        },
        StateKind::Idle => ToastSpec {
            title: "GamePause",
            severity: Severity::Success,
        },
    }
}
/// Sound-only mode uses this system sound. Visual notifications use Windows sound.
pub fn beep_code(kind: StateKind) -> u32 {
    match kind {
        StateKind::Attention => MB_ICONASTERISK,
        // Pause-success and restore-success are both "good" → the OK sound.
        StateKind::Idle | StateKind::Paused => MB_OK,
    }
}
pub(super) fn notification_flags(kind: StateKind, sound: bool) -> u32 {
    (if kind == StateKind::Attention {
        NIIF_ERROR
    } else {
        NIIF_INFO
    }) | NIIF_RESPECT_QUIET_TIME
        | if sound { 0 } else { NIIF_NOSOUND }
}
pub(super) struct NativeNotifications {
    pub(super) hwnd: HWND,
}
pub(super) fn notification_kind(kind: crate::notifications::Kind) -> StateKind {
    match kind {
        crate::notifications::Kind::Paused => StateKind::Paused,
        crate::notifications::Kind::Restored | crate::notifications::Kind::Ask => StateKind::Idle,
        crate::notifications::Kind::Failure => StateKind::Attention,
    }
}
impl crate::notifications::Sink for NativeNotifications {
    fn toast(&mut self, event: &crate::notifications::Event, sound: bool) -> bool {
        unsafe { state_toast(self.hwnd, &event.text, notification_kind(event.kind), sound) }
    }
    fn sound(&mut self, event: &crate::notifications::Event) -> bool {
        unsafe { MessageBeep(beep_code(notification_kind(event.kind))) != 0 }
    }
}
