//! Portable pen event types, without native handles or GUI dependencies.
//!
//! The Windows backend emits [`PenSample`] values in chronological order within
//! each pointer's history. Applications use [`PenSample::stroke_id`] to retain
//! canvas ownership from [`PenPhase::Down`] through [`PenPhase::Up`] or
//! [`PenPhase::Cancel`].

/// Lifecycle of a pen over a window and of its individual contacts.
///
/// Entering or hovering does not start a stroke. The Windows backend assigns a
/// new stroke identifier only on [`Down`](Self::Down); an in-contact update
/// without a preceding Down does not start drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenPhase {
    /// The pointer entered the window or detection range.
    ///
    /// This does not start a stroke. An Enter received after Down may carry the
    /// active stroke identifier.
    Enter,
    /// A position update without contact.
    Hover,
    /// Contact began in this window; a new stroke was assigned.
    Down,
    /// Contact moved within an existing stroke (possibly outside the window).
    Move,
    /// Contact ended normally.
    Up,
    /// A stroke was interrupted by input cancellation or an explicit request.
    ///
    /// The backend may reuse the last known position and attributes when no
    /// fresh native sample is available. Treat this as a terminal stroke event.
    Cancel,
    /// The pointer left the window or detection range.
    ///
    /// If a stroke remains active, the backend emits Cancel before Leave.
    Leave,
}

/// Independently optional tilt axes, in degrees, each in `-90..=90`.
///
/// Each axis follows the device/Windows convention and its own availability
/// flag. `None` means unavailable; `Some(0.0)` means a reported zero angle.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PenTilt {
    /// Tilt along the X axis, in degrees, when reported by the device.
    pub x: Option<f32>,
    /// Tilt along the Y axis, in degrees, when reported by the device.
    pub y: Option<f32>,
}

/// An owned sample which can be sent to a drawing engine on another thread.
///
/// Coordinates are in the window's client space and attributes describe the
/// sample time. The input layer applies no document transforms or canvas
/// clipping. Optional measurements distinguish unavailable data from zero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PenSample {
    /// OS pointer identifier, valid only for the current pointer lifetime.
    ///
    /// This identifies a pointer, not a physical device. The OS can reuse it
    /// after that pointer leaves detection range.
    pub pointer_id: u32,
    /// Backend-assigned contact identifier, unique within this backend instance.
    ///
    /// Always present for emitted Down/Move/Up/Cancel events; Hover/Leave have
    /// no stroke. Enter can carry a current stroke when delivered after Down.
    /// Identifiers start at one and are not reused by the same backend, even
    /// after cancellation. Separate backend instances have independent counters.
    pub stroke_id: Option<u64>,
    /// Microseconds since this backend was created, monotonic per pointer.
    ///
    /// The backend prefers the performance counter, with a wrapping Windows
    /// millisecond timestamp as fallback. Pre-creation history is clamped to
    /// zero. This is not wall-clock time and cannot be compared directly across
    /// backend instances.
    pub time_us: u64,
    /// Window client coordinates in physical pixels for a DPI-aware window.
    ///
    /// Values may be negative or outside the client area while captured. The
    /// Windows backend currently converts integer pixel coordinates to `f64`;
    /// this field does not imply subpixel hardware precision.
    pub position: [f64; 2],
    /// Normalized pressure in `0..=1`; `None` means unavailable, not zero pressure.
    ///
    /// Windows Ink prefers raw `TIP_PRESSURE`, normalized with
    /// `clamp((raw - logical_min) / (logical_max - logical_min), 0, 1)`.
    /// If the raw property or its reading is unavailable, valid standard Windows
    /// pressure is divided by 1024 instead. No pressure curve is applied.
    pub pressure: Option<f32>,
    /// Number of integer values in the raw pressure property's logical range.
    ///
    /// Filled after a successful raw pressure read as
    /// `logical_max - logical_min + 1`; for example, `0..=16383` gives 16384.
    /// This is range capacity, not the current raw value or a measurement of
    /// actual hardware precision. Standard pressure fallback leaves this `None`,
    /// even when [`pressure`](Self::pressure) is available.
    pub pressure_level: Option<i32>,
    /// Independently optional X/Y tilt measurements at the sample time.
    pub tilt_deg: PenTilt,
    /// Clockwise pen rotation in `0..360` degrees, when reported by the device.
    ///
    /// `None` means the standard rotation field is unavailable.
    pub rotation_deg: Option<f32>,
    /// Whether the driver reports that the eraser end/tool is active.
    pub eraser: bool,
    /// Whether the driver reports an inverted pen, independently of the eraser flag.
    pub inverted: bool,
    /// Whether the driver reports the pen's barrel/side button as pressed.
    pub barrel_button: bool,
    /// The pointer/contact lifecycle event represented by this sample.
    pub phase: PenPhase,
}
