//! Taskbar flash that also works while the window is minimized.
//!
//! eframe stops running the UI loop for a minimized window, so a flash
//! requested from the UI only happened after the user reopened the window.
//! On Windows the flash is therefore triggered from the network thread, as
//! soon as the event arrives, with `FlashWindowEx`. On other platforms this is
//! a no-op and the UI-side `RequestUserAttention` is used instead.

#[derive(Clone, Default)]
pub struct TaskbarFlash {
    /// The window's HWND as an integer (raw pointers are not `Send`).
    /// 0 when there is no real window, e.g. in headless tests.
    #[cfg(windows)]
    hwnd: isize,
}

impl TaskbarFlash {
    #[cfg(windows)]
    pub fn from_window(window: &impl raw_window_handle::HasWindowHandle) -> Self {
        use raw_window_handle::RawWindowHandle;
        let hwnd = match window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
            _ => 0,
        };
        TaskbarFlash { hwnd }
    }

    #[cfg(not(windows))]
    pub fn from_window<W>(_window: &W) -> Self {
        TaskbarFlash::default()
    }

    /// Flash the taskbar button until the window is brought to the front.
    /// Does nothing if GhostChat already is the foreground window.
    #[cfg(windows)]
    pub fn flash_if_background(&self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx, GetForegroundWindow,
        };
        if self.hwnd == 0 {
            return;
        }
        let hwnd = self.hwnd as windows_sys::Win32::Foundation::HWND;
        // SAFETY: plain Win32 calls with a valid struct; FlashWindowEx may be
        // called from any thread, and a stale HWND only makes it fail.
        unsafe {
            if GetForegroundWindow() == hwnd {
                return;
            }
            let info = FLASHWINFO {
                cbSize: size_of::<FLASHWINFO>() as u32,
                hwnd,
                dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
                uCount: 0,
                dwTimeout: 0,
            };
            FlashWindowEx(&info);
        }
    }

    #[cfg(not(windows))]
    pub fn flash_if_background(&self) {}
}
