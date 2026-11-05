//! Device descriptors are queried once per device, separate from per-message raw values.

use std::collections::{HashMap, hash_map::Entry};

use windows::Win32::{
    Devices::HumanInterfaceDevice::{HID_USAGE_DIGITIZER_TIP_PRESSURE, HID_USAGE_PAGE_DIGITIZER},
    Foundation::HANDLE,
    UI::{
        Controls::{
            POINTER_DEVICE_INFO, POINTER_DEVICE_PROPERTY, POINTER_DEVICE_TYPE_EXTERNAL_PEN,
            POINTER_DEVICE_TYPE_INTEGRATED_PEN,
        },
        Input::Pointer::{GetPointerDeviceProperties, GetPointerDevices, GetRawPointerDeviceData},
    },
};

use super::InkError;
use crate::PenSample;

const MAX_DEVICE_ITEMS: u32 = 65_536;

// windows::HANDLE does not implement Hash. Keep its numeric identity private.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DeviceKey(usize);

impl From<HANDLE> for DeviceKey {
    fn from(device: HANDLE) -> Self {
        Self(device.0 as usize)
    }
}

#[derive(Debug, Clone, Copy)]
struct PressureDefine {
    property: POINTER_DEVICE_PROPERTY,
    inverse_range: f64,
}

impl PressureDefine {
    fn from_properties(properties: &[POINTER_DEVICE_PROPERTY]) -> Result<Option<Self>, InkError> {
        let Some(property) = properties
            .iter()
            .find(|property| {
                property.usagePageId == HID_USAGE_PAGE_DIGITIZER
                    && property.usageId == HID_USAGE_DIGITIZER_TIP_PRESSURE
            })
            .copied()
        else {
            return Ok(None);
        };
        if property.logicalMax <= property.logicalMin {
            return Err(InkError::InvalidPressureRange {
                logical_min: property.logicalMin,
                logical_max: property.logicalMax,
            });
        }
        // Widen before subtracting: a signed i32 range can span more than i32::MAX.
        let range = f64::from(property.logicalMax) - f64::from(property.logicalMin);
        Ok(Some(Self {
            property,
            inverse_range: 1.0 / range,
        }))
    }

    fn normalize(self, raw: i32) -> f32 {
        ((f64::from(raw) - f64::from(self.property.logicalMin)) * self.inverse_range)
            .clamp(0.0, 1.0) as f32
    }
}

#[derive(Debug)]
struct CachedPressure {
    definition: Option<PressureDefine>,
    // Report a descriptor failure on first use, without querying it on every move.
    warning: Option<InkError>,
}

impl CachedPressure {
    fn from_query(result: Result<Option<PressureDefine>, InkError>) -> Self {
        match result {
            Ok(definition) => Self {
                definition,
                warning: None,
            },
            Err(error) => Self {
                definition: None,
                warning: Some(error),
            },
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct PressureCache {
    // sourceDevice is a device HANDLE, independent of both pointer IDs and HWNDs.
    devices: HashMap<DeviceKey, CachedPressure>,
    // One requested property per history point, newest first. Reuse capacity.
    raw_values: Vec<i32>,
}

impl PressureCache {
    pub fn new() -> Result<Self, InkError> {
        let mut cache = Self::default();
        cache.prewarm()?;
        Ok(cache)
    }

    pub fn clear(&mut self) {
        self.devices.clear();
    }

    pub fn refresh(&mut self) -> Result<(), InkError> {
        self.clear();
        self.prewarm()
    }

    fn prewarm(&mut self) -> Result<(), InkError> {
        let devices: Vec<POINTER_DEVICE_INFO> =
            query_list("GetPointerDevices", |count, buffer| {
                // SAFETY: query_list provides a valid count and a correctly sized output buffer.
                unsafe { GetPointerDevices(count, buffer) }
            })?;
        for device in devices {
            if matches!(
                device.pointerDeviceType,
                POINTER_DEVICE_TYPE_INTEGRATED_PEN | POINTER_DEVICE_TYPE_EXTERNAL_PEN
            ) {
                self.devices.insert(
                    device.device.into(),
                    CachedPressure::from_query(query_pressure(device.device)),
                );
            }
        }
        Ok(())
    }

    pub fn read(
        &mut self,
        device: HANDLE,
        pointer_id: u32,
        history_count: usize,
    ) -> (Option<PressureHistory<'_>>, Option<InkError>) {
        self.read_with(device, history_count, query_pressure, |property, values| {
            // SAFETY: One property is requested. values has exactly history_count
            // writable i32 elements, and the descriptor came from this sourceDevice.
            unsafe {
                GetRawPointerDeviceData(
                    pointer_id,
                    values.len() as u32,
                    std::slice::from_ref(property),
                    values.as_mut_ptr(),
                )
            }
            .map_err(|error| InkError::win32("GetRawPointerDeviceData", error))
        })
    }

    fn read_with(
        &mut self,
        device: HANDLE,
        history_count: usize,
        load: impl FnOnce(HANDLE) -> Result<Option<PressureDefine>, InkError>,
        read: impl FnOnce(&POINTER_DEVICE_PROPERTY, &mut [i32]) -> Result<(), InkError>,
    ) -> (Option<PressureHistory<'_>>, Option<InkError>) {
        let cached = match self.devices.entry(device.into()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(CachedPressure::from_query(load(device))),
        };
        let warning = cached.warning.take();
        let Some(definition) = cached.definition else {
            return (None, warning);
        };
        self.raw_values.resize(history_count, 0);
        if let Err(error) = read(&definition.property, &mut self.raw_values) {
            // Keep the descriptor: a transient message-cache failure must be retried
            // on the next message, without re-querying device properties.
            return (None, Some(error));
        }
        (
            Some(PressureHistory {
                definition,
                values: &self.raw_values,
            }),
            warning,
        )
    }
}

pub(super) struct PressureHistory<'a> {
    definition: PressureDefine,
    values: &'a [i32],
}

impl PressureHistory<'_> {
    pub fn apply(&self, chronological_samples: &mut [PenSample]) -> Result<(), InkError> {
        if chronological_samples.len() != self.values.len() {
            return Err(InkError::PressureHistoryMismatch);
        }
        // Raw pointer data and pen history are both newest first. Samples have
        // already been reversed, so match them with reversed raw values.
        for (sample, &raw) in chronological_samples
            .iter_mut()
            .zip(self.values.iter().rev())
        {
            sample.pressure = Some(self.definition.normalize(raw));
            sample.pressure_level =
                Some(self.definition.property.logicalMax - self.definition.property.logicalMin + 1);
        }
        Ok(())
    }
}

fn query_pressure(device: HANDLE) -> Result<Option<PressureDefine>, InkError> {
    let properties = query_list("GetPointerDeviceProperties", |count, buffer| {
        // SAFETY: The device is an OS sourceDevice handle; count/buffer are valid.
        unsafe { GetPointerDeviceProperties(device, count, buffer) }
    })?;
    PressureDefine::from_properties(&properties)
}

fn query_list<T: Default + Clone>(
    operation: &'static str,
    mut query: impl FnMut(&mut u32, Option<*mut T>) -> windows::core::Result<()>,
) -> Result<Vec<T>, InkError> {
    let mut count = 0;
    // Retry if a hot-plug changes the required size between querying and reading.
    for _ in 0..3 {
        query(&mut count, None).map_err(|error| InkError::win32(operation, error))?;
        if count > MAX_DEVICE_ITEMS {
            return Err(InkError::InvalidDeviceList {
                operation,
                entries: count,
            });
        }
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut values = vec![T::default(); count as usize];
        let capacity = count;
        let result = query(&mut count, Some(values.as_mut_ptr()));
        if count > capacity {
            continue;
        }
        result.map_err(|error| InkError::win32(operation, error))?;
        values.truncate(count as usize);
        return Ok(values);
    }
    Err(InkError::InvalidDeviceList {
        operation,
        entries: count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PenPhase, PenTilt};
    use std::cell::Cell;

    fn definition(min: i32, max: i32) -> PressureDefine {
        PressureDefine::from_properties(&[POINTER_DEVICE_PROPERTY {
            logicalMin: min,
            logicalMax: max,
            usagePageId: HID_USAGE_PAGE_DIGITIZER,
            usageId: HID_USAGE_DIGITIZER_TIP_PRESSURE,
            ..Default::default()
        }])
        .unwrap()
        .unwrap()
    }

    fn device(id: usize) -> HANDLE {
        HANDLE(id as *mut _)
    }

    fn samples(count: usize) -> Vec<PenSample> {
        (0..count)
            .map(|index| PenSample {
                pointer_id: 7,
                stroke_id: Some(1),
                time_us: index as u64,
                position: [index as f64, 0.0],
                pressure: Some(0.25),
                pressure_level: None,
                tilt_deg: PenTilt::default(),
                rotation_deg: None,
                eraser: false,
                inverted: false,
                barrel_button: false,
                phase: PenPhase::Move,
            })
            .collect()
    }

    #[test]
    fn normalizes_nonzero_minimum_and_clamps_out_of_range_values() {
        let pressure = definition(100, 1100);
        assert_eq!(pressure.normalize(-100), 0.0);
        assert_eq!(pressure.normalize(100), 0.0);
        assert_eq!(pressure.normalize(600), 0.5);
        assert_eq!(pressure.normalize(1100), 1.0);
        assert_eq!(pressure.normalize(2000), 1.0);
    }

    #[test]
    fn preserves_16k_pressure_steps_and_handles_full_signed_range_without_overflow() {
        let pressure = definition(0, 16383);
        assert_eq!(pressure.normalize(0), 0.0);
        assert_eq!(pressure.normalize(16383), 1.0);
        assert!(pressure.normalize(8193) > pressure.normalize(8192));
        let signed = definition(i32::MIN, i32::MAX);
        assert_eq!(signed.normalize(i32::MIN), 0.0);
        assert_eq!(signed.normalize(i32::MAX), 1.0);
        assert!((signed.normalize(0) - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn requires_digitizer_pressure_usage_and_rejects_zero_or_reversed_ranges() {
        let mut property = definition(0, 1024).property;
        property.usagePageId = 1; // Generic Desktop X has the same usage ID as pressure.
        assert!(
            PressureDefine::from_properties(&[property])
                .unwrap()
                .is_none()
        );
        assert!(PressureDefine::from_properties(&[]).unwrap().is_none());
        for (min, max) in [(0, 0), (100, 99)] {
            property = definition(0, 1024).property;
            property.logicalMin = min;
            property.logicalMax = max;
            assert!(matches!(
                PressureDefine::from_properties(&[property]),
                Err(InkError::InvalidPressureRange { .. })
            ));
        }
    }

    #[test]
    fn caches_each_source_device_once_with_its_own_pressure_range() {
        let mut cache = PressureCache::default();
        let loads = Cell::new(0);
        for (id, max) in [(1, 1024), (2, 16383), (1, 1024), (2, 16383)] {
            let (history, warning) = cache.read_with(
                device(id),
                1,
                |_| {
                    loads.set(loads.get() + 1);
                    Ok(Some(definition(0, max)))
                },
                |_, output| {
                    output[0] = 512;
                    Ok(())
                },
            );
            assert!(warning.is_none());
            let mut points = samples(1);
            history.unwrap().apply(&mut points).unwrap();
            assert_eq!(points[0].pressure, Some(512.0 / max as f32));
        }
        assert_eq!(loads.get(), 2);
    }

    #[test]
    fn caches_missing_properties_and_descriptor_failures_until_invalidated() {
        for fail in [false, true] {
            let mut cache = PressureCache::default();
            let loads = Cell::new(0);
            for attempt in 0..2 {
                let (history, warning) = cache.read_with(
                    device(1),
                    1,
                    |_| {
                        loads.set(loads.get() + 1);
                        if fail {
                            Err(InkError::InvalidPressureRange {
                                logical_min: 0,
                                logical_max: 0,
                            })
                        } else {
                            Ok(None)
                        }
                    },
                    |_, _| panic!("unsupported pressure must not trigger a raw read"),
                );
                assert!(history.is_none());
                assert_eq!(warning.is_some(), fail && attempt == 0);
            }
            assert_eq!(loads.get(), 1);
            cache.clear();
            let (history, warning) = cache.read_with(
                device(1),
                1,
                |_| Ok(Some(definition(0, 16383))),
                |_, output| {
                    output[0] = 16383;
                    Ok(())
                },
            );
            assert!(warning.is_none());
            assert!(history.is_some());
        }
    }

    #[test]
    fn raw_failure_preserves_fallback_and_recovers_without_reloading_descriptor() {
        let mut cache = PressureCache::default();
        let mut points = samples(2);
        let fallback = points.clone();
        let (history, warning) = cache.read_with(
            device(1),
            2,
            |_| Ok(Some(definition(0, 16383))),
            |_, output| {
                output[0] = 100; // A failed API may have written only part of the output.
                Err(InkError::win32(
                    "GetRawPointerDeviceData",
                    windows::core::Error::from_hresult(windows::core::HRESULT(
                        0x8007000D_u32 as i32,
                    )),
                ))
            },
        );
        assert!(history.is_none());
        assert!(warning.is_some());
        assert_eq!(points, fallback);
        let (history, warning) = cache.read_with(
            device(1),
            2,
            |_| panic!("a transient raw read failure must not discard the cached descriptor"),
            |_, output| {
                output.copy_from_slice(&[16383, 0]);
                Ok(())
            },
        );
        assert!(warning.is_none());
        history.unwrap().apply(&mut points).unwrap();
        assert_eq!(points[0].pressure, Some(0.0));
        assert_eq!(points[1].pressure, Some(1.0));
    }

    #[test]
    fn historical_pressure_tracks_each_point_when_converted_to_chronological_order() {
        let values = [16383, 8192, 0]; // current, previous, oldest
        let history = PressureHistory {
            definition: definition(0, 16383),
            values: &values,
        };
        let mut points = samples(3);
        let times: Vec<_> = points.iter().map(|point| point.time_us).collect();
        history.apply(&mut points).unwrap();
        assert_eq!(points[0].pressure, Some(0.0));
        assert_eq!(points[1].pressure, Some(8192.0 / 16383.0));
        assert_eq!(points[2].pressure, Some(1.0));
        assert_eq!(
            points.iter().map(|point| point.time_us).collect::<Vec<_>>(),
            times
        );
        let unchanged = points.clone();
        assert!(matches!(
            history.apply(&mut points[..2]),
            Err(InkError::PressureHistoryMismatch)
        ));
        assert_eq!(points, unchanged);
    }

    #[test]
    fn raw_buffer_is_sized_per_batch_and_reuses_existing_allocation() {
        let mut cache = PressureCache::default();
        let mut first_buffer: *const i32 = std::ptr::null();
        for count in [4, 1, 3] {
            let (history, warning) = cache.read_with(
                device(1),
                count,
                |_| Ok(Some(definition(0, 16383))),
                |_, output| {
                    assert_eq!(output.len(), count);
                    if first_buffer.is_null() {
                        first_buffer = output.as_ptr();
                    } else {
                        assert_eq!(output.as_ptr(), first_buffer);
                    }
                    output.fill(0);
                    Ok(())
                },
            );
            assert!(warning.is_none());
            assert_eq!(history.unwrap().values.len(), count);
        }
    }

    #[test]
    fn warmup_loads_properties_for_currently_attached_pen_devices() {
        // Native descriptor-query smoke test; no pen input is injected or required.
        let devices: Vec<POINTER_DEVICE_INFO> = query_list("GetPointerDevices", |count, buffer| {
            // SAFETY: query_list supplies valid output storage.
            unsafe { GetPointerDevices(count, buffer) }
        })
        .unwrap();
        let cache = PressureCache::new().unwrap();
        for device in devices.into_iter().filter(|device| {
            matches!(
                device.pointerDeviceType,
                POINTER_DEVICE_TYPE_INTEGRATED_PEN | POINTER_DEVICE_TYPE_EXTERNAL_PEN
            )
        }) {
            assert!(cache.devices.contains_key(&device.device.into()));
        }
    }
}
