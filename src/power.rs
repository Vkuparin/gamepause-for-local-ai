//! Nonblocking power invalidation shared by the UI and detection/control workers.
use std::sync::atomic::{AtomicU64, Ordering};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub generation: u64,
    pub suspended: bool,
}
#[derive(Default)]
pub struct Signal(AtomicU64);
impl Signal {
    pub fn snapshot(&self) -> Snapshot {
        let state = self.0.load(Ordering::Acquire);
        Snapshot {
            generation: state >> 1,
            suspended: state & 1 != 0,
        }
    }
    /// Modern Windows always sends automatic resume; the later user-interaction
    /// resume notification must not restart reconciliation or its grace period.
    pub fn notify(&self, event: usize) -> bool {
        let suspended = match event {
            4 => true,   // PBT_APMSUSPEND
            18 => false, // PBT_APMRESUMEAUTOMATIC
            _ => return false,
        };
        let _ = self
            .0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                if suspended && state & 1 != 0 {
                    return None;
                }
                state
                    .checked_add(2)
                    .map(|next| (next & !1) | u64::from(suspended))
            });
        true
    }
    pub fn permits(&self, generation: u64) -> bool {
        self.snapshot()
            == Snapshot {
                generation,
                suspended: false,
            }
    }
}

type Entry = (
    std::sync::Arc<Signal>,
    std::sync::mpsc::Sender<crate::app::Action>,
);
type Registry = std::collections::BTreeMap<usize, Entry>;
static REGISTRY: std::sync::OnceLock<std::sync::Mutex<Registry>> = std::sync::OnceLock::new();
static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
fn registry() -> &'static std::sync::Mutex<Registry> {
    REGISTRY.get_or_init(Default::default)
}
unsafe extern "system" fn callback(
    context: *const std::ffi::c_void,
    event: u32,
    _: *const std::ffi::c_void,
) -> u32 {
    // The context is an opaque ID, never a dereferenced allocation. A callback
    // racing unregister either clones a live entry or finds no entry. No UAF.
    let _ = std::panic::catch_unwind(|| {
        let entry = registry()
            .lock()
            .ok()
            .and_then(|entries| entries.get(&(context as usize)).cloned());
        if let Some((signal, wake)) = entry
            && signal.notify(event as usize)
        {
            let _ = wake.send(crate::app::Action::PowerChanged);
        }
    });
    0
}
/// Windowless CLI control cannot receive WM_POWERBROADCAST.
/// Keep one callback registration for its lifetime, with no new thread.
pub struct Registration {
    handle: windows_sys::Win32::System::Power::HPOWERNOTIFY,
    id: usize,
    _subscription: Box<windows_sys::Win32::System::Power::DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS>,
}
impl Registration {
    pub fn headless(
        signal: std::sync::Arc<Signal>,
        wake: std::sync::mpsc::Sender<crate::app::Action>,
    ) -> anyhow::Result<Self> {
        let id = NEXT_ID
            .try_update(Ordering::AcqRel, Ordering::Acquire, |id| id.checked_add(1))
            .map_err(|_| anyhow::anyhow!("Power registration IDs exhausted"))?;
        registry()
            .lock()
            .map_err(|_| anyhow::anyhow!("Power registry unavailable"))?
            .insert(id, (signal, wake));
        let subscription = Box::new(
            windows_sys::Win32::System::Power::DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
                Callback: Some(callback),
                Context: id as *mut std::ffi::c_void,
            },
        );
        let mut handle = std::ptr::null_mut();
        let error = unsafe {
            windows_sys::Win32::System::Power::PowerRegisterSuspendResumeNotification(
                windows_sys::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_CALLBACK,
                (&*subscription as *const _) as *mut std::ffi::c_void,
                &mut handle,
            )
        };
        if error != 0 {
            if let Ok(mut entries) = registry().lock() {
                entries.remove(&id);
            }
            anyhow::bail!("Could not register headless power notifications: Windows error {error}");
        }
        Ok(Self {
            handle: handle as isize,
            id,
            _subscription: subscription,
        })
    }
    fn unregister(&mut self) -> u32 {
        if let Ok(mut entries) = registry().lock() {
            entries.remove(&self.id);
        }
        if self.handle == 0 {
            return 0;
        }
        let error = unsafe {
            windows_sys::Win32::System::Power::PowerUnregisterSuspendResumeNotification(self.handle)
        };
        if error == 0 {
            self.handle = 0;
        }
        error
    }
    pub fn close(mut self) -> anyhow::Result<()> {
        let error = self.unregister();
        if error != 0 {
            anyhow::bail!(
                "Could not unregister headless power notifications: Windows error {error}"
            );
        }
        Ok(())
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        if let Ok(mut entries) = registry().lock() {
            entries.remove(&self.id);
        }
        // Even if native cancellation fails, an outstanding callback cannot
        // access freed state: its opaque registry ID has already been removed.
        self.unregister();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suspend_and_automatic_resume_invalidate_old_scans_without_double_resume() {
        let signal = Signal::default();
        assert!(signal.permits(0));
        assert!(signal.notify(4));
        let suspended = signal.snapshot();
        assert!(suspended.suspended);
        assert!(!signal.permits(suspended.generation));
        signal.notify(4);
        assert_eq!(signal.snapshot(), suspended);
        signal.notify(18);
        let resumed = signal.snapshot();
        assert!(signal.permits(resumed.generation));
        assert!(!signal.permits(0));
        assert!(!signal.notify(7));
        assert_eq!(signal.snapshot(), resumed);
        signal.notify(18); // Wake without an observed suspend also invalidates.
        assert!(!signal.permits(resumed.generation));
    }
    #[test]
    #[ignore = "registers a temporary native Windows power callback; never suspends the machine"]
    fn native_headless_registration_invalidates_and_releases_its_callback() {
        let signal = std::sync::Arc::new(Signal::default());
        let (tx, rx) = std::sync::mpsc::channel();
        let registration = Registration::headless(signal.clone(), tx).unwrap();
        let id = registration.id;
        unsafe {
            callback(id as *const _, 4, std::ptr::null());
        }
        assert!(signal.snapshot().suspended);
        assert!(matches!(
            rx.try_recv(),
            Ok(crate::app::Action::PowerChanged)
        ));
        unsafe {
            callback(id as *const _, 18, std::ptr::null());
        }
        assert!(!signal.snapshot().suspended);
        assert!(matches!(
            rx.try_recv(),
            Ok(crate::app::Action::PowerChanged)
        ));
        let before = signal.snapshot();
        registration.close().unwrap();
        assert!(!registry().lock().unwrap().contains_key(&id));
        unsafe {
            callback(id as *const _, 18, std::ptr::null());
        }
        assert_eq!(signal.snapshot(), before);
        assert!(rx.try_recv().is_err());
    }
}
