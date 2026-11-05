//! Framework-independent pen input for custom drawing and note-taking canvases.
//!
//! [`PenSample`], [`PenPhase`] and [`PenTilt`] form the portable event model. On
//! Windows, `ink::WindowsInk` adapts desktop `WM_POINTER` messages into owned
//! samples, including coalesced history and normalized device pressure.
//!
//! # Platform support
//!
//! The [`pen`] module is available on every platform. The `ink` module is
//! available only with `cfg(windows)` and uses `windows` 0.62 handle types.
//! The library does not depend on a windowing or rendering framework.
//!
//! # Integration
//!
//! Create one adapter per live HWND on its owning thread and store it in the
//! window's state. Forward native messages synchronously, before retrieving
//! another message with `GetMessage` or `PeekMessage`.
//!
//! ```no_run
//! # #[cfg(windows)]
//! # fn main() {
//! use tablet_input_windows::ink::{InkError, WindowsInk};
//! use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
//!
//! fn attach(hwnd: HWND) -> Result<WindowsInk, InkError> {
//!     WindowsInk::new(hwnd)
//! }
//!
//! // Return this boolean through your framework's message-consumption policy.
//! fn on_message(
//!     ink: &mut WindowsInk,
//!     hwnd: HWND,
//!     message: u32,
//!     wparam: WPARAM,
//!     lparam: LPARAM,
//! ) -> bool {
//!     let result = ink.handle_message(hwnd, message, wparam, lparam);
//!     if let Some(error) = result.error {
//!         eprintln!("Windows Ink: {error}");
//!     }
//!     for sample in result.samples {
//!         println!("{sample:?}"); // Apply canvas ownership and transforms here.
//!     }
//!     result.handled
//! }
//! # }
//! # #[cfg(not(windows))]
//! # fn main() {}
//! ```
//!
//! The caller owns window lifetime, focus policy, DPI awareness, message
//! dispatch, canvas hit testing, and document transforms. Explicitly call
//! `WindowsInk::cancel_all` when your application needs to end active strokes,
//! then deliver its returned samples. Stop native reads and device refreshes
//! when the window is destroyed, and release its adapter.
//!
//! Only owned samples should cross threads; the Windows adapter is neither
//! [`Send`] nor [`Sync`]. Select one pen backend for each input sequence to avoid
//! delivering the same input twice.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod pen;

#[cfg(windows)]
pub mod ink;

#[cfg(any(windows, test))]
mod state;

pub use pen::{PenPhase, PenSample, PenTilt};
