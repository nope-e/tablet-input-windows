//! All native calls and coordinate/time conversions stay inside the adapter.

use windows::Win32::{
    Foundation::{HWND, POINT},
    Graphics::Gdi::ScreenToClient,
    System::{
        Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
        SystemInformation::GetTickCount64,
        Threading::GetCurrentThreadId,
    },
    UI::{
        Controls::RegisterPointerDeviceNotifications,
        Input::Pointer::{
            GetPointerPenInfo, GetPointerPenInfoHistory, GetPointerType, POINTER_FLAG_CANCELED,
            POINTER_FLAG_INCONTACT, POINTER_FLAG_UP, POINTER_PEN_INFO,
        },
        WindowsAndMessaging::{
            GetWindowThreadProcessId, IsWindow, PEN_FLAG_BARREL, PEN_FLAG_ERASER,
            PEN_FLAG_INVERTED, PEN_MASK_PRESSURE, PEN_MASK_ROTATION, PEN_MASK_TILT_X,
            PEN_MASK_TILT_Y, POINTER_INPUT_TYPE, PT_PEN, WM_POINTERDOWN, WM_POINTERENTER,
            WM_POINTERLEAVE, WM_POINTERUP,
        },
    },
};

use super::{InkError, pressure::PressureCache};
use crate::{PenPhase, PenSample, PenTilt};

// Defensive bound against corrupted driver counts, well above a message's normal history.
const MAX_HISTORY: u32 = 65_536;

#[derive(Debug)]
pub(super) struct NativeInput {
    hwnd: HWND,
    clock: Clock,
    history: Vec<POINTER_PEN_INFO>,
    pressure_cache: PressureCache,
}

impl NativeInput {
    pub fn new(hwnd: HWND) -> Result<Self, InkError> {
        // SAFETY: These functions validate handles in the OS; no Rust memory is dereferenced.
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            return Err(InkError::InvalidWindow);
        }
        if unsafe { GetWindowThreadProcessId(hwnd, None) } != unsafe { GetCurrentThreadId() } {
            return Err(InkError::WrongThread);
        }

        // SAFETY: This HWND belongs to this thread. Register window-local device
        // changes; range notifications are unnecessary for descriptor caching.
        unsafe { RegisterPointerDeviceNotifications(hwnd, false) }
            .map_err(|error| InkError::win32("RegisterPointerDeviceNotifications", error))?;
        Ok(Self {
            hwnd,
            clock: Clock::new()?,
            history: Vec::new(),
            pressure_cache: PressureCache::new()?,
        })
    }
    pub fn invalidate_devices(&mut self) {
        self.pressure_cache.clear();
    }

    pub fn refresh_devices(&mut self) -> Result<(), InkError> {
        self.pressure_cache.refresh()
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn now_us(&self) -> u64 {
        self.clock.now_us()
    }

    pub fn is_pen(&self, pointer_id: u32) -> Result<bool, InkError> {
        let mut kind = POINTER_INPUT_TYPE::default();
        // SAFETY: kind is a valid writable output. Called on the window thread.
        unsafe { GetPointerType(pointer_id, &mut kind) }
            .map_err(|e| InkError::win32("GetPointerType", e))?;
        Ok(kind == PT_PEN)
    }

    pub fn read(
        &mut self,
        pointer_id: u32,
        message: u32,
    ) -> Result<(Vec<PenSample>, Option<InkError>), InkError> {
        let mut current = POINTER_PEN_INFO::default();
        // SAFETY: current is a valid writable output. No message has been dequeued since delivery.
        unsafe { GetPointerPenInfo(pointer_id, &mut current) }
            .map_err(|e| InkError::win32("GetPointerPenInfo", e))?;

        let mut warning = None;
        let history_len = if !matches!(message, WM_POINTERENTER | WM_POINTERLEAVE)
            && current.pointerInfo.historyCount > 1
        {
            match self.read_history(pointer_id, current.pointerInfo.historyCount) {
                Ok(len) => len,
                Err(error) => {
                    warning = Some(error);
                    0
                }
            }
        } else {
            0
        };

        let mut samples = if history_len == 0 {
            vec![self.convert(&current, Some(message))?]
        } else {
            convert_history(
                &self.history[..history_len],
                message,
                |info, override_message| self.convert(info, override_message),
            )?
        };

        // One raw query for the whole batch, with matching pointer/device and count.
        let same_device = history_len == 0
            || self.history[..history_len].iter().all(|info| {
                info.pointerInfo.sourceDevice == current.pointerInfo.sourceDevice
                    && info.pointerInfo.pointerId == pointer_id
            });
        if same_device {
            let (raw, pressure_warning) = self.pressure_cache.read(
                current.pointerInfo.sourceDevice,
                pointer_id,
                samples.len(),
            );
            warning = warning.or(pressure_warning);
            if let Some(raw) = raw
                && let Err(error) = raw.apply(&mut samples)
            {
                warning = warning.or(Some(error));
            }
        } else {
            warning = warning.or(Some(InkError::PressureHistoryMismatch));
        }
        Ok((samples, warning))
    }

    fn read_history(&mut self, pointer_id: u32, mut entries: u32) -> Result<usize, InkError> {
        // Retry once if the required size changed; never silently accept truncated history.
        for _ in 0..2 {
            if entries == 0 || entries > MAX_HISTORY {
                return Err(InkError::InvalidHistory { entries });
            }
            self.history
                .resize(entries as usize, POINTER_PEN_INFO::default());
            let capacity = entries;
            // SAFETY: the initialized vector has room for capacity output structures.
            unsafe {
                GetPointerPenInfoHistory(pointer_id, &mut entries, Some(self.history.as_mut_ptr()))
            }
            .map_err(|e| InkError::win32("GetPointerPenInfoHistory", e))?;
            if entries > 0 && entries <= capacity {
                return Ok(entries as usize);
            }
        }
        Err(InkError::InvalidHistory { entries })
    }

    fn convert(
        &self,
        info: &POINTER_PEN_INFO,
        message: Option<u32>,
    ) -> Result<PenSample, InkError> {
        let mut point = info.pointerInfo.ptPixelLocation;
        // SAFETY: point is writable, and the HWND was validated on the owning thread.
        if !unsafe { ScreenToClient(self.hwnd, &mut point) }.as_bool() {
            return Err(InkError::CoordinateConversion);
        }
        Ok(decode(
            info,
            message,
            point,
            self.clock
                .sample_us(info.pointerInfo.PerformanceCount, info.pointerInfo.dwTime),
        ))
    }
}

fn convert_history(
    history: &[POINTER_PEN_INFO],
    message: u32,
    mut convert: impl FnMut(&POINTER_PEN_INFO, Option<u32>) -> Result<PenSample, InkError>,
) -> Result<Vec<PenSample>, InkError> {
    // Windows returns newest first and already includes the current point.
    // Override the phase only on that current point, not on the earlier moves.
    (0..history.len())
        .rev()
        .map(|index| convert(&history[index], (index == 0).then_some(message)))
        .collect()
}

fn decode(info: &POINTER_PEN_INFO, message: Option<u32>, point: POINT, time_us: u64) -> PenSample {
    let flags = info.pointerInfo.pointerFlags;
    let phase = if flags.contains(POINTER_FLAG_CANCELED) {
        PenPhase::Cancel
    } else {
        match message {
            Some(WM_POINTERENTER) => PenPhase::Enter,
            Some(WM_POINTERLEAVE) => PenPhase::Leave,
            Some(WM_POINTERDOWN) => PenPhase::Down,
            Some(WM_POINTERUP) => PenPhase::Up,
            _ if flags.contains(POINTER_FLAG_UP) => PenPhase::Up,
            _ if flags.contains(POINTER_FLAG_INCONTACT) => PenPhase::Move,
            _ => PenPhase::Hover,
        }
    };
    PenSample {
        pointer_id: info.pointerInfo.pointerId,
        stroke_id: None,
        time_us,
        position: [f64::from(point.x), f64::from(point.y)],
        // Fallback only: read() replaces this with normalized raw TIP_PRESSURE
        // when the device descriptor and the whole raw batch are available.
        pressure: (info.penMask & PEN_MASK_PRESSURE != 0)
            .then(|| (info.pressure as f32 / 1024.0).clamp(0.0, 1.0)),
        pressure_level: None,
        tilt_deg: PenTilt {
            x: (info.penMask & PEN_MASK_TILT_X != 0).then_some(info.tiltX as f32),
            y: (info.penMask & PEN_MASK_TILT_Y != 0).then_some(info.tiltY as f32),
        },
        rotation_deg: (info.penMask & PEN_MASK_ROTATION != 0).then_some(info.rotation as f32),
        eraser: info.penFlags & PEN_FLAG_ERASER != 0,
        inverted: info.penFlags & PEN_FLAG_INVERTED != 0,
        barrel_button: info.penFlags & PEN_FLAG_BARREL != 0,
        phase,
    }
}

#[derive(Debug)]
struct Clock {
    frequency: u64,
    qpc_origin: u64,
    tick_origin: u64,
}

impl Clock {
    fn new() -> Result<Self, InkError> {
        let mut frequency = 0;
        let mut origin = 0;
        // SAFETY: Both output pointers are valid. QPC frequency is positive on supported Windows.
        unsafe { QueryPerformanceFrequency(&mut frequency) }
            .map_err(|e| InkError::win32("QueryPerformanceFrequency", e))?;
        unsafe { QueryPerformanceCounter(&mut origin) }
            .map_err(|e| InkError::win32("QueryPerformanceCounter", e))?;
        Ok(Self {
            frequency: frequency as u64,
            qpc_origin: origin as u64,
            // SAFETY: GetTickCount64 takes no arguments.
            tick_origin: unsafe { GetTickCount64() },
        })
    }

    fn now_us(&self) -> u64 {
        let mut count = 0;
        // SAFETY: count is a valid output; QPC is guaranteed on supported Windows.
        if unsafe { QueryPerformanceCounter(&mut count) }.is_ok() {
            self.qpc_us(count as u64)
        } else {
            // SAFETY: GetTickCount64 takes no arguments.
            unsafe { GetTickCount64() }
                .saturating_sub(self.tick_origin)
                .saturating_mul(1000)
        }
    }

    fn qpc_us(&self, count: u64) -> u64 {
        ((u128::from(count.saturating_sub(self.qpc_origin)) * 1_000_000)
            / u128::from(self.frequency)) as u64
    }

    fn sample_us(&self, performance_count: u64, tick: u32) -> u64 {
        if performance_count != 0 {
            return self.qpc_us(performance_count);
        }
        if tick == 0 {
            return self.now_us();
        }
        // dwTime is a wrapping 32-bit uptime value. Recover the nearest epoch,
        // rather than mixing uncalibrated tick and QPC absolute values.
        // SAFETY: GetTickCount64 takes no arguments.
        let now = unsafe { GetTickCount64() };
        unwrap_tick(tick, now)
            .saturating_sub(self.tick_origin)
            .saturating_mul(1000)
    }
}

fn unwrap_tick(tick: u32, now: u64) -> u64 {
    let delta = tick.wrapping_sub(now as u32) as i32;
    now.saturating_add_signed(i64::from(delta))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(info: &POINTER_PEN_INFO, message: Option<u32>) -> PenSample {
        decode(info, message, POINT { x: -200, y: 300 }, 100)
    }

    #[test]
    fn optional_properties_obey_each_mask_including_partial_tilt() {
        let mut info = POINTER_PEN_INFO {
            pressure: 512,
            tiltX: -30,
            tiltY: 60,
            rotation: 90,
            ..Default::default()
        };
        let absent = point(&info, None);
        assert_eq!(absent.pressure, None);
        assert_eq!(absent.tilt_deg, PenTilt::default());
        assert_eq!(absent.rotation_deg, None);
        info.penMask = PEN_MASK_PRESSURE | PEN_MASK_TILT_X | PEN_MASK_ROTATION;
        let present = point(&info, None);
        assert_eq!(present.pressure, Some(0.5));
        assert_eq!(
            present.tilt_deg,
            PenTilt {
                x: Some(-30.0),
                y: None
            }
        );
        assert_eq!(present.rotation_deg, Some(90.0));
        info.pressure = 2048;
        assert_eq!(point(&info, None).pressure, Some(1.0));
        info.pressure = 0;
        assert_eq!(point(&info, None).pressure, Some(0.0));
    }

    #[test]
    fn current_message_overrides_contact_flags_but_canceled_has_priority() {
        let mut info = POINTER_PEN_INFO::default();
        info.pointerInfo.pointerFlags = POINTER_FLAG_INCONTACT;
        assert_eq!(point(&info, None).phase, PenPhase::Move);
        assert_eq!(point(&info, Some(WM_POINTERDOWN)).phase, PenPhase::Down);
        assert_eq!(point(&info, Some(WM_POINTERENTER)).phase, PenPhase::Enter);
        assert_eq!(point(&info, Some(WM_POINTERUP)).phase, PenPhase::Up);
        info.pointerInfo.pointerFlags |= POINTER_FLAG_CANCELED;
        assert_eq!(point(&info, Some(WM_POINTERUP)).phase, PenPhase::Cancel);
    }

    #[test]
    fn down_flag_in_an_update_or_history_cannot_synthesize_a_window_down() {
        use windows::Win32::UI::{
            Input::Pointer::POINTER_FLAG_DOWN, WindowsAndMessaging::WM_POINTERUPDATE,
        };
        let mut info = POINTER_PEN_INFO::default();
        info.pointerInfo.pointerFlags = POINTER_FLAG_DOWN | POINTER_FLAG_INCONTACT;
        assert_eq!(point(&info, None).phase, PenPhase::Move);
        assert_eq!(point(&info, Some(WM_POINTERUPDATE)).phase, PenPhase::Move);
        assert_eq!(point(&info, Some(WM_POINTERDOWN)).phase, PenPhase::Down);
    }

    #[test]
    fn eraser_inversion_and_barrel_button_are_independent() {
        let mut info = POINTER_PEN_INFO {
            penFlags: PEN_FLAG_INVERTED,
            ..Default::default()
        };
        let inverted = point(&info, None);
        assert!(inverted.inverted);
        assert!(!inverted.eraser);
        assert!(!inverted.barrel_button);
        info.penFlags |= PEN_FLAG_ERASER | PEN_FLAG_BARREL;
        let eraser = point(&info, None);
        assert!(eraser.eraser && eraser.inverted && eraser.barrel_button);
        assert_eq!(eraser.position, [-200.0, 300.0]);
    }

    #[test]
    fn qpc_timestamps_use_wide_integer_arithmetic_and_clamp_old_history() {
        let clock = Clock {
            frequency: 10_000_000,
            qpc_origin: 9_000_000_000_000_000,
            tick_origin: 0,
        };
        assert_eq!(clock.qpc_us(clock.qpc_origin + 15_000_000), 1_500_000);
        assert_eq!(clock.qpc_us(clock.qpc_origin - 1), 0);
    }

    #[test]
    fn millisecond_time_unwraps_across_the_49_day_boundary() {
        let wrap = 1u64 << 32;
        assert_eq!(unwrap_tick(u32::MAX - 5, wrap + 10), wrap - 6);
        assert_eq!(unwrap_tick(5, wrap + 10), wrap + 5);
        assert_eq!(unwrap_tick(15, wrap + 10), wrap + 15);
    }

    #[test]
    fn coalesced_history_is_chronological_and_current_up_is_not_duplicated() {
        let history: Vec<_> = [30, 20, 10]
            .into_iter()
            .map(|time| {
                let mut info = POINTER_PEN_INFO::default();
                info.pointerInfo.pointerId = 7;
                info.pointerInfo.PerformanceCount = time;
                info.pointerInfo.pointerFlags = POINTER_FLAG_INCONTACT;
                info
            })
            .collect();
        let samples = convert_history(&history, WM_POINTERUP, |info, message| {
            Ok(decode(
                info,
                message,
                POINT::default(),
                info.pointerInfo.PerformanceCount,
            ))
        })
        .unwrap();
        assert_eq!(
            samples.iter().map(|s| s.time_us).collect::<Vec<_>>(),
            [10, 20, 30]
        );
        assert_eq!(
            samples.iter().map(|s| s.phase).collect::<Vec<_>>(),
            [PenPhase::Move, PenPhase::Move, PenPhase::Up]
        );
    }
}
