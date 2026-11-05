//! Windows Ink input through the desktop `WM_POINTER` API.
//!
//! Create one [`WindowsInk`] per HWND on its owning thread. Feed messages to it
//! before retrieving the next message, and use [`MessageResult::handled`] to
//! decide whether to return `LRESULT(0)` from a native window procedure.
//! Do not also process the same message through another pen backend.
//!
//! # Responsibilities
//!
//! The adapter reads pen input, maintains contact identifiers, converts screen
//! positions into client coordinates, and caches raw pressure descriptors.
//! The caller owns window creation/destruction, focus and cancel-mode policy,
//! DPI awareness, rendering, and canvas/document transforms.
//!
//! # Cancellation
//!
//! Native pen cancellation and capture loss end active contacts automatically.
//! Window lifecycle messages are ignored: explicitly call
//! [`WindowsInk::cancel_all`] when application policy requires it, and deliver
//! the returned samples before discarding the adapter.
//!
//! # Partial failures
//!
//! [`MessageResult::error`] is independent of [`MessageResult::handled`] and
//! [`MessageResult::samples`]. A history or raw pressure failure can return
//! usable samples with a fallback. Always inspect all three fields.

mod error;
mod native;
mod pressure;

use std::{marker::PhantomData, rc::Rc};

use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    UI::WindowsAndMessaging::{
        WM_DEVICECHANGE, WM_POINTERCAPTURECHANGED, WM_POINTERDEVICECHANGE, WM_POINTERDOWN,
        WM_POINTERENTER, WM_POINTERLEAVE, WM_POINTERUP, WM_POINTERUPDATE,
    },
};

use crate::{PenSample, state::PenState};
pub use error::InkError;
use native::NativeInput;

/// Message disposition, owned samples, and an optional read error.
///
/// These fields must be considered independently. A consumed pen message can
/// produce no samples, and a read error can accompany usable fallback samples.
/// The default value means unhandled, no samples, and no error.
#[derive(Debug, Default)]
#[must_use]
pub struct MessageResult {
    /// True only for a pen input message consumed by this backend.
    ///
    /// In a WndProc return zero; in a framework hook use its consumption policy.
    /// A consumed message stays handled even if reading its samples fails.
    /// Window lifecycle messages are ignored; the caller decides when to cancel.
    pub handled: bool,
    /// Samples to deliver to the application, including any synthesized Cancel.
    ///
    /// History is ordered oldest to newest per pointer, without duplicating the
    /// current point. State rules suppress Move/Up events without a known Down.
    /// There is no ordering guarantee between separate pointers.
    pub samples: Vec<PenSample>,
    /// The first read failure in this message, independent of message disposition.
    ///
    /// On history failure the current point is returned; on an unreadable current
    /// point the active stroke is canceled using its last known position.
    /// A raw pressure failure preserves samples with standard Windows pressure.
    /// Failure to identify an unknown pointer leaves the message unhandled.
    pub error: Option<InkError>,
}

/// A window's pen input adapter. It does not own or subclass the HWND.
///
/// This type is deliberately neither Send nor Sync. Only its owned samples should
/// cross threads. The caller owns the window lifecycle, message loop, DPI
/// awareness, canvas hit testing, and document transforms. Use [`Self::cancel_all`]
/// for application-driven cancellation, such as focus loss or closing a canvas.
#[derive(Debug)]
pub struct WindowsInk {
    native: NativeInput,
    state: PenState,
    _window_thread: PhantomData<Rc<()>>,
}

impl WindowsInk {
    /// Attach to an existing window on its owning thread.
    ///
    /// No global input/DPI settings or pointer capture settings are changed.
    /// Pressure descriptors are cached during setup. Device-change notifications
    /// are registered for this HWND and end when it is destroyed.
    /// Keep the HWND alive while reading input or refreshing devices. The caller
    /// must cancel remaining contacts and detach the adapter when closing it.
    ///
    /// # Errors
    ///
    /// Returns [`InkError::InvalidWindow`] for an invalid HWND or
    /// [`InkError::WrongThread`] for a window owned by another thread. Native
    /// setup/enumeration failures return [`InkError::Win32`] or
    /// [`InkError::InvalidDeviceList`]. A single device's pressure descriptor
    /// failure is cached and reported on its first input message instead of
    /// preventing the adapter from being created.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # #[cfg(windows)]
    /// # fn main() {
    /// use tablet_input_windows::ink::{InkError, WindowsInk};
    /// use windows::Win32::Foundation::HWND;
    ///
    /// fn attach_to_window(hwnd: HWND) -> Result<WindowsInk, InkError> {
    ///     // Store this adapter for the lifetime of the existing window.
    ///     WindowsInk::new(hwnd)
    /// }
    /// # }
    /// # #[cfg(not(windows))]
    /// # fn main() {}
    /// ```
    pub fn new(hwnd: HWND) -> Result<Self, InkError> {
        Ok(Self {
            native: NativeInput::new(hwnd)?,
            state: PenState::default(),
            _window_thread: PhantomData,
        })
    }

    /// Return the borrowed HWND associated with this adapter.
    ///
    /// The adapter does not own the window or extend its lifetime. This accessor
    /// returns the original handle without checking whether it is still live.
    pub fn hwnd(&self) -> HWND {
        self.native.hwnd()
    }

    /// Read a native message immediately on the window's owning thread.
    ///
    /// Call before the next `GetMessage`/`PeekMessage`, while the native pointer
    /// message cache is valid. Pass the original HWND, message identifier, and
    /// WPARAM/LPARAM; LPARAM is currently unused.
    ///
    /// Reads `WM_POINTERDOWN`, `WM_POINTERUPDATE`, `WM_POINTERUP`,
    /// `WM_POINTERENTER`, `WM_POINTERLEAVE` and `WM_POINTERCAPTURECHANGED` when
    /// they describe a pen. Messages for other HWNDs, mouse/touch input and
    /// unrelated messages are ignored. `WM_POINTERDEVICECHANGE` and
    /// `WM_DEVICECHANGE` invalidate the pressure cache and remain unhandled.
    ///
    /// Focus, activation, cancel-mode and destroy messages do not cancel
    /// contacts. The caller handles those through [`Self::cancel_all`].
    ///
    /// # Return value
    ///
    /// Deliver [`MessageResult::samples`] and report [`MessageResult::error`]
    /// separately. Use [`MessageResult::handled`] to decide whether normal
    /// framework/`DefWindowProc` dispatch should continue. A recognized pen
    /// message remains consumed on read failure; an unknown pointer whose type
    /// cannot be queried remains unhandled and reports the query error.
    pub fn handle_message(
        &mut self,
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> MessageResult {
        if hwnd != self.hwnd() {
            return MessageResult::default();
        }

        if matches!(message, WM_POINTERDEVICECHANGE | WM_DEVICECHANGE) {
            // Re-query on the next input, outside the notification itself.
            self.native.invalidate_devices();
            return MessageResult::default();
        }

        if !matches!(
            message,
            WM_POINTERDOWN
                | WM_POINTERUPDATE
                | WM_POINTERUP
                | WM_POINTERENTER
                | WM_POINTERLEAVE
                | WM_POINTERCAPTURECHANGED
        ) {
            return MessageResult::default();
        }

        // GET_POINTERID_WPARAM is the low word, not the full WPARAM (which has flags).
        let pointer_id = (wparam.0 & 0xffff) as u32;
        let known = self.state.contains(pointer_id);
        if !known {
            match self.native.is_pen(pointer_id) {
                Ok(true) => {}
                Ok(false) => return MessageResult::default(),
                // Capture-loss notifications need no fresh sample; the cache may
                // already be gone. An unknown pointer has no state to cancel.
                Err(_) if message == WM_POINTERCAPTURECHANGED => return MessageResult::default(),
                Err(error) => {
                    return MessageResult {
                        error: Some(error),
                        ..Default::default()
                    };
                }
            }
        }

        let mut result = MessageResult {
            handled: true,
            ..Default::default()
        };
        if message == WM_POINTERCAPTURECHANGED {
            self.state
                .forget(pointer_id, self.native.now_us(), &mut result.samples);
            return result;
        }

        match self.native.read(pointer_id, message) {
            Ok((samples, warning)) => {
                for sample in samples {
                    self.state.push(sample, &mut result.samples);
                }
                result.error = warning;
            }
            Err(error) => {
                self.state
                    .cancel_pointer(pointer_id, self.native.now_us(), &mut result.samples);
                result.error = Some(error);
            }
        }
        if message == WM_POINTERLEAVE {
            self.state
                .forget(pointer_id, self.native.now_us(), &mut result.samples);
        }
        result
    }

    /// Explicitly cancel contacts for application lifecycle or policy changes.
    ///
    /// The caller chooses when to use this, for example on focus loss, cancel
    /// mode, closing a canvas, or replacing/detaching the backend.
    /// Returned Cancel events must be delivered to the drawing engine.
    /// Drop alone cannot deliver events to the application.
    ///
    /// Emits one Cancel per active stroke using its last position/attributes
    /// and the current backend time, then clears all tracked pointers, including
    /// hover-only pointers. Repeated calls return an empty vector until another
    /// stroke starts. Future Down events continue with new stroke identifiers.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # #[cfg(windows)]
    /// # fn on_focus_lost(ink: &mut tablet_input_windows::ink::WindowsInk) {
    /// // Application policy: finish all strokes when the window loses focus.
    /// for canceled in ink.cancel_all() {
    ///     println!("{canceled:?}");
    /// }
    /// # }
    /// # fn main() {}
    /// ```
    pub fn cancel_all(&mut self) -> Vec<PenSample> {
        self.state.cancel_all(self.native.now_us())
    }

    /// Re-enumerate pen devices and rebuild their cached pressure descriptors.
    ///
    /// Use this when a framework reports device changes without forwarding
    /// WM_POINTERDEVICECHANGE/WM_DEVICECHANGE to handle_message. Failed or absent
    /// pressure descriptors are cached too, and refreshed on the next change.
    /// Call on the owning thread while the associated HWND remains alive.
    /// Active contacts, stroke identifiers, and the time origin are preserved.
    ///
    /// # Errors
    ///
    /// Returns [`InkError::Win32`] or [`InkError::InvalidDeviceList`] if device
    /// enumeration fails. The old cache is cleared first; subsequent input can
    /// still query an unknown device lazily. Per-device property failures are
    /// cached and reported on that device's first subsequent input message.
    pub fn refresh_devices(&mut self) -> Result<(), InkError> {
        self.native.refresh_devices()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PenPhase, PenTilt};
    use windows::{
        Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
            WM_ACTIVATEAPP, WM_CANCELMODE, WM_DESTROY, WM_KILLFOCUS, WM_NCDESTROY,
        },
        core::w,
    };

    #[test]
    fn lifecycle_messages_preserve_contact_until_caller_cancels() {
        struct Window(HWND);
        impl Drop for Window {
            fn drop(&mut self) {
                // SAFETY: This guard owns the message-only window on this thread.
                unsafe { DestroyWindow(self.0).unwrap() };
            }
        }
        // SAFETY: A built-in STATIC window with no application callbacks or data.
        let window = Window(unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Ink lifecycle test"),
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
        });
        let mut ink = WindowsInk::new(window.0).unwrap();
        let mut output = Vec::new();
        ink.state.push(
            PenSample {
                pointer_id: 7,
                stroke_id: None,
                time_us: 0,
                position: [10.0, 20.0],
                pressure: Some(0.5),
                pressure_level: None,
                tilt_deg: PenTilt::default(),
                rotation_deg: None,
                eraser: false,
                inverted: false,
                barrel_button: false,
                phase: PenPhase::Down,
            },
            &mut output,
        );
        let stroke_id = output[0].stroke_id;
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
        let canceled = ink.cancel_all();
        assert_eq!(canceled.len(), 1);
        assert_eq!(canceled[0].phase, PenPhase::Cancel);
        assert_eq!(canceled[0].stroke_id, stroke_id);
        assert_eq!(canceled[0].position, [10.0, 20.0]);
        assert!(ink.cancel_all().is_empty());
    }
}
