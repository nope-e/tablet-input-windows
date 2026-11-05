# tablet-input-windows
![Crates.io Version](https://img.shields.io/crates/v/tablet-input-windows)
![docs.rs](https://img.shields.io/docsrs/tablet-input-windows)
![Crates.io License](https://img.shields.io/crates/l/tablet-input-windows)
![GitHub last commit](https://img.shields.io/github/last-commit/nope-e/tablet-input-windows)


English | [简体中文](README.zh-CN.md)

A Rust pen input library for custom drawing and note-taking canvases. The Windows Ink backend reads desktop `WM_POINTER` messages through the `windows` crate and returns owned samples with stroke identifiers, coalesced history, pressure, tilt, and pen flags.

The library works independently of winit, WinUI, and rendering frameworks. Window lifetime, focus policy, DPI awareness, canvas ownership, and rendering belong to the application.

## Platform and scope

- The `pen` module and its root re-exports build on every platform. The native `ink` module is available only on Windows.
- The current backend is Windows Ink / `WM_POINTER`. WinTab is not implemented.
- The project uses Rust edition 2024, `windows` 0.62, and `thiserror` 2. Winit 0.30 is a Windows example dependency.
- The underlying pointer APIs are available from Windows 8; the Rust toolchain, dependencies, and application can require a newer system. This is not a guarantee that the examples run on Windows 8.

## Quick start

For a consuming project next to this checkout:

```toml
[dependencies]
tablet-input-windows = { path = "../tablet-input-windows" }

[target.'cfg(windows)'.dependencies]
windows = { version = "0.62.2", features = ["Win32_Foundation"] }
```

Use one `WindowsInk` per live HWND. Create and use it on the window's owning thread, and keep it in your window state. These functions illustrate attaching to an existing window and forwarding messages:

```rust
use tablet_input_windows::ink::{InkError, WindowsInk};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};

fn attach(hwnd: HWND) -> Result<WindowsInk, InkError> {
    WindowsInk::new(hwnd)
}

fn on_message(
    ink: &mut WindowsInk,
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> bool {
    let result = ink.handle_message(hwnd, message, wparam, lparam);
    if let Some(error) = result.error {
        eprintln!("Windows Ink: {error}");
    }
    for sample in result.samples {
        // Apply canvas ownership and client-to-document transforms here.
        println!("{sample:?}");
    }
    result.handled
}
```

Call `handle_message` synchronously, before the next `GetMessage` or `PeekMessage`. Native pointer history belongs to the most recently retrieved message and can become unavailable after retrieving another one. [Microsoft: GetPointerPenInfoHistory](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getpointerpeninfohistory)

In a native WndProc, return `LRESULT(0)` when `handled` is true; otherwise continue normal dispatch. In a framework hook, map this boolean to the framework's consumption policy. Forward a pen sequence consistently through one backend to avoid duplicate samples or inconsistent behavior.

## Application lifecycle

The caller chooses when focus loss, application deactivation, cancel mode, tool changes, or closing a canvas should cancel strokes. `handle_message` ignores `WM_KILLFOCUS`, `WM_ACTIVATEAPP`, `WM_CANCELMODE`, `WM_DESTROY`, and `WM_NCDESTROY` without changing contact state.

Explicitly cancel when your application policy requires it:

```rust
// `ink` is the adapter stored in your window state.
for canceled in ink.cancel_all() {
    println!("{canceled:?}"); // Deliver to your drawing engine.
}
```

`cancel_all()` emits one `Cancel` per active stroke, reusing its last position and attributes with an updated timestamp. It clears contact and hover state; repeated calls return no events until new strokes start. Future strokes receive new identifiers.

Cancel and deliver remaining events when replacing or releasing the adapter. Stop native input reads and device refreshes once the HWND is destroyed, and release its adapter. Dropping the adapter does not deliver events or destroy the window. `WindowsInk` is neither `Send` nor `Sync`; owned `PenSample` values can cross threads.

The library still translates input-level interruption into `Cancel`: native cancellation flags, pointer capture loss, and Hover/Leave after a missing Up. These events describe the pen stream itself.

## Event model

`PenSample`, `PenPhase`, and `PenTilt` are available both under `pen` and as root re-exports.

| Field | Meaning |
| --- | --- |
| `pointer_id` | OS pointer identifier for the current pointer lifetime. It can be reused and is not a physical device identifier. |
| `stroke_id` | Contact identifier allocated by this adapter. Present on emitted Down/Move/Up/Cancel; absent on Hover/Leave. Enter can retain an active stroke. |
| `time_us` | Microseconds since adapter creation, nondecreasing while a pointer is tracked. Separate adapters have separate time origins. |
| `position` | Client coordinates `[x, y]`, in physical pixels for a DPI-aware window. Currently derived from integer pixel coordinates. |
| `pressure` | Optional normalized pressure in `0..=1`. `None` means unavailable. |
| `pressure_level` | Optional integer count of values in the raw pressure property's logical range. Standard pressure fallback leaves it `None`. |
| `tilt_deg` | Independently optional X/Y angles in degrees, each in `-90..=90`. |
| `rotation_deg` | Optional clockwise rotation in `0..360` degrees. |
| `eraser` | Driver-reported eraser tool/end flag. |
| `inverted` | Driver-reported inverted-pen flag, independent of `eraser`. |
| `barrel_button` | Driver-reported side-button state. |
| `phase` | Pointer/contact lifecycle phase described below. |

| Phase | Behavior |
| --- | --- |
| `Enter` | Entered the window or detection range; does not begin a stroke. |
| `Hover` | Position update without contact. |
| `Down` | Begins a new stroke and allocates an identifier. |
| `Move` | Updates an existing stroke, including captured movement outside the client area. |
| `Up` | Completes a stroke. |
| `Cancel` | Interrupts a stroke; synthesized events can reuse the last known position. |
| `Leave` | Leaves the window or detection range; an unfinished stroke is canceled first. |

Only Down starts a stroke. Stray in-contact updates or Up events cannot start drawing after cancellation or after entering from another window. Stroke identifiers start at one and are not reused within an adapter instance.

Accept a stroke only if its Down belongs to your canvas, then retain that ownership through Move, Up, and Cancel using `stroke_id`. Positions can be negative or beyond the client area during capture. Apply clipping and document transforms in your drawing engine. No subpixel HIMETRIC mapping is currently performed.

Timestamps prefer the performance counter, fall back to Windows millisecond timestamps, handle 32-bit timestamp wraparound, and clamp history predating adapter creation to zero. They are not wall-clock timestamps.

## Pressure and device caching

At initialization, the adapter enumerates pen devices and caches each `TIP_PRESSURE` descriptor and the reciprocal of its logical range. Cache identity comes from `POINTER_INFO.sourceDevice`, independently of HWNDs and pointer identifiers. Unknown devices are queried lazily on first input.

When a usable descriptor is available, each input batch makes one [`GetRawPointerDeviceData`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getrawpointerdevicedata) call for pressure and reuses a raw-value buffer. Each history point receives its corresponding pressure before chronological delivery. The raw history ordering also follows the approach used by [WPF](https://github.com/dotnet/wpf/blob/main/src/Microsoft.DotNet.Wpf/src/PresentationCore/System/Windows/InterOp/HwndPointerInputProvider.cs).

```text
pressure       = clamp((raw - logical_min) / (logical_max - logical_min), 0, 1)
pressure_level = logical_max - logical_min + 1
```

For `0..=16383`, the logical range contains 16384 integer values and normalization divides by 16383. `pressure_level` describes range capacity, not the current raw value or measured hardware precision.

If the raw property, range, or reading is unavailable, valid standard `GetPointerPenInfo` pressure is normalized by 1024; otherwise `pressure` is `None`. Standard Windows pressure uses `0..=1024`, which does not identify the device's native pressure capacity, so fallback samples keep `pressure_level = None`. [Microsoft: POINTER_PEN_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-pointer_pen_info)

Missing properties, invalid ranges, and descriptor-query failures are cached until invalidation. A cached descriptor error is offered for reporting on that device's first use; a raw read failure is reported for the current batch and retried on subsequent input. `MessageResult.error` retains the first error if multiple failures occur together.

The adapter registers window-local device notifications. `WM_POINTERDEVICECHANGE` and `WM_DEVICECHANGE` invalidate its descriptor cache while remaining unhandled. If your framework does not forward those messages, call `ink.refresh_devices()` on device changes. It clears and rebuilds the cache while preserving active strokes and the time origin. Enumeration errors are returned directly; per-device property errors are deferred until input from that device.

Caching avoids repeated descriptor queries. The additional cost of reading raw pressure has not been benchmarked.

## Results and errors

`handle_message()` returns three independent fields:

| Field | Contract |
| --- | --- |
| `handled` | Whether this backend consumed a pen message. It remains true for a recognized pen message even when sample reading fails. |
| `samples` | Owned events in oldest-to-newest history order per pointer. The current history point is not duplicated. |
| `error` | The first read/descriptor error, which can accompany usable samples. |

History-read failures fall back to the current point. Raw pressure failures retain standard pressure. Failure to read or convert the current batch cancels an active stroke using its saved position. Failure to identify an unknown pointer reports an error and leaves the message unhandled.

[`InkError`](src/ink/error.rs) uses `thiserror` and keeps structured variants for invalid windows, wrong threads, coordinate conversion, invalid history/device counts, pressure ranges, history matching, and Win32 failures. `InkError::Win32 { operation, source }` retains the native error code and exposes its cause through `std::error::Error::source()`.

## Examples

Run these commands from this checkout on Windows:

| Command | Behavior |
| --- | --- |
| `cargo run --example ink_events` | Native Win32 window with Per-Monitor V2 DPI awareness; prints pen events. The application handles lifecycle cancellation in its WndProc. |
| `cargo run --example winit_ink` | Winit `ApplicationHandler` window; prints Down/Move/Up/Cancel to the terminal. |
| `cargo run --example pointer_device_properties` | Enumerates devices and prints HID usages, logical/physical ranges, units, and `TIP_PRESSURE` descriptors. |

The window examples show a plain background and print events; they do not render strokes. The property example reads descriptors, which does not verify live pressure precision.

The [winit example](examples/winit_ink.rs) forwards native messages through `with_msg_hook` and shares the adapter with `Rc<RefCell<Option<WindowsInk>>>` on the same thread. It cancels on `Focused(false)`, requests exit on `CloseRequested`, and cancels/releases the adapter during destruction or exit. Added/Removed device events refresh descriptors. DPI setup and these lifecycle decisions are application code.

## Layout

| Module | Responsibility |
| --- | --- |
| [`pen`](src/pen.rs) | Portable event types. |
| [`ink`](src/ink/mod.rs) | Public Windows adapter, message result, and error type. |
| [`ink::native`](src/ink/native.rs), private | Native calls, history reads, coordinate conversion, and time conversion. |
| [`ink::pressure`](src/ink/pressure.rs), private | Device descriptors, caching, raw pressure batches, and normalization. |
| [`state`](src/state.rs), private | Pointer/contact ownership and stroke lifecycle without native calls. |
