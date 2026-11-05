#![cfg(windows)]

use tablet_input_windows::{
    PenSample,
    ink::{InkError, WindowsInk},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
            WM_ACTIVATEAPP, WM_CANCELMODE, WM_DESTROY, WM_DEVICECHANGE, WM_KILLFOCUS, WM_NCDESTROY,
            WM_POINTERDEVICECHANGE, WM_POINTERDOWN,
        },
    },
    core::w,
};

struct TestWindow(HWND);

impl TestWindow {
    fn new() -> Self {
        // SAFETY: A message-only built-in STATIC window, created and destroyed on this thread.
        Self(unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Ink API test"),
                WINDOW_STYLE::default(),
                0,
                0,
                1,
                1,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
            .unwrap()
        })
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        // SAFETY: This guard owns a window created on this thread.
        unsafe { DestroyWindow(self.0).unwrap() };
    }
}

#[test]
fn validates_handle_and_owner_thread() {
    assert!(matches!(
        WindowsInk::new(HWND::default()),
        Err(InkError::InvalidWindow)
    ));
    let window = TestWindow::new();
    assert_eq!(WindowsInk::new(window.0).unwrap().hwnd(), window.0);
    let address = window.0.0 as usize;
    let rejected = std::thread::spawn(move || {
        matches!(
            WindowsInk::new(HWND(address as *mut _)),
            Err(InkError::WrongThread)
        )
    })
    .join()
    .unwrap();
    assert!(rejected);
}

#[test]
fn foreign_and_window_lifecycle_messages_are_ignored() {
    let window = TestWindow::new();
    let other = TestWindow::new();
    let mut ink = WindowsInk::new(window.0).unwrap();
    let foreign = ink.handle_message(other.0, WM_POINTERDOWN, WPARAM(0), LPARAM(0));
    assert!(!foreign.handled && foreign.samples.is_empty() && foreign.error.is_none());
    for message in [
        WM_KILLFOCUS,
        WM_ACTIVATEAPP,
        WM_CANCELMODE,
        WM_DESTROY,
        WM_NCDESTROY,
    ] {
        let result = ink.handle_message(window.0, message, WPARAM(0), LPARAM(0));
        assert!(!result.handled && result.samples.is_empty() && result.error.is_none());
    }
    // Synthetic lifecycle messages do not detach the backend. The real HWND
    // remains alive, so its caller can continue using it until actual teardown.
    ink.refresh_devices().unwrap();
}

#[test]
fn owned_samples_can_cross_thread_boundaries() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PenSample>();
}

#[test]
fn device_changes_keep_normal_dispatch_and_support_explicit_cache_refresh() {
    let window = TestWindow::new();
    let mut ink = WindowsInk::new(window.0).unwrap();
    for message in [WM_POINTERDEVICECHANGE, WM_DEVICECHANGE] {
        let result = ink.handle_message(window.0, message, WPARAM(0), LPARAM(0));
        assert!(!result.handled && result.samples.is_empty() && result.error.is_none());
    }
    ink.refresh_devices().unwrap();
}
