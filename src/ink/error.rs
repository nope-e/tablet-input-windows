//! Errors produced during Windows Ink setup, descriptor queries, and input reads.

/// A setup or input-read failure. Native errors retain their original error code.
///
/// Setup and explicit device refresh return errors directly. Per-message read
/// errors are reported through [`super::MessageResult::error`], where samples
/// may still be usable. [`Win32`](Self::Win32) retains the operation name and
/// exposes the underlying error through [`std::error::Error::source`].
#[derive(Debug, thiserror::Error)]
pub enum InkError {
    /// The HWND supplied during initialization is not a live window.
    #[error("the window handle is invalid")]
    InvalidWindow,
    /// Initialization was attempted on a thread other than the window's owner.
    #[error("Windows Ink must be created on the window's thread")]
    WrongThread,
    /// A native screen position could not be converted into client coordinates.
    #[error("screen-to-client coordinate conversion failed")]
    CoordinateConversion,
    /// The history count was zero, exceeded the defensive limit, or kept changing.
    #[error("invalid or changing pointer history size: {entries}")]
    InvalidHistory {
        /// The rejected or most recently returned history count.
        entries: u32,
    },
    /// A device/property count exceeded the defensive limit or kept changing.
    #[error("{operation}: invalid or changing device/property count: {entries}")]
    InvalidDeviceList {
        /// The enumeration/query operation that returned the count.
        operation: &'static str,
        /// The rejected or most recently required number of elements.
        entries: u32,
    },
    /// A raw pressure descriptor did not define a positive logical range.
    #[error("invalid TIP_PRESSURE logical range: {logical_min}..={logical_max}")]
    InvalidPressureRange {
        /// The descriptor's minimum logical value.
        logical_min: i32,
        /// The descriptor's maximum logical value, which must exceed the minimum.
        logical_max: i32,
    },
    /// Raw pressure and pen history could not be matched by pointer/device/count.
    ///
    /// Samples retain their standard pressure rather than using mismatched data.
    #[error("raw pressure does not match the pen history")]
    PressureHistoryMismatch,
    /// A Win32 API call failed, retaining both operation context and native cause.
    #[error("{operation}: {source}")]
    Win32 {
        /// The name of the failing Win32 operation.
        operation: &'static str,
        /// The native error, including its original HRESULT and error message.
        #[source]
        source: windows::core::Error,
    },
}

impl InkError {
    pub(super) fn win32(operation: &'static str, source: windows::core::Error) -> Self {
        Self::Win32 { operation, source }
    }
}
