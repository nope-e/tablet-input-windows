//! Enumerate attached pointer devices and print their native HID property ranges.
//! `cargo run --example pointer_device_properties`

#[cfg(not(windows))]
fn main() {
    eprintln!("This example requires Windows.");
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use windows::Win32::{
        Devices::HumanInterfaceDevice::{
            HID_USAGE_DIGITIZER_TIP_PRESSURE, HID_USAGE_PAGE_DIGITIZER,
        },
        UI::{
            Controls::{
                POINTER_DEVICE_INFO, POINTER_DEVICE_PROPERTY, POINTER_DEVICE_TYPE_EXTERNAL_PEN,
                POINTER_DEVICE_TYPE_INTEGRATED_PEN, POINTER_DEVICE_TYPE_TOUCH,
                POINTER_DEVICE_TYPE_TOUCH_PAD,
            },
            Input::Pointer::{GetPointerDeviceProperties, GetPointerDevices},
        },
    };

    let devices: Vec<POINTER_DEVICE_INFO> = query_list(|count, buffer| {
        // SAFETY: query_list supplies a valid count and a buffer with that many elements.
        unsafe { GetPointerDevices(count, buffer) }
    })?;
    println!("Pointer devices: {}", devices.len());
    let pen_count = devices
        .iter()
        .filter(|device| {
            matches!(
                device.pointerDeviceType,
                POINTER_DEVICE_TYPE_INTEGRATED_PEN | POINTER_DEVICE_TYPE_EXTERNAL_PEN
            )
        })
        .count();
    println!("Pen devices: {pen_count}");

    for (index, device) in devices.iter().enumerate() {
        let name_len = device
            .productString
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(device.productString.len());
        let name = String::from_utf16_lossy(&device.productString[..name_len]);
        let kind = match device.pointerDeviceType {
            POINTER_DEVICE_TYPE_INTEGRATED_PEN => "INTEGRATED_PEN",
            POINTER_DEVICE_TYPE_EXTERNAL_PEN => "EXTERNAL_PEN",
            POINTER_DEVICE_TYPE_TOUCH => "TOUCH",
            POINTER_DEVICE_TYPE_TOUCH_PAD => "TOUCH_PAD",
            _ => "UNKNOWN",
        };
        println!(
            "\nDevice[{index}]: {kind} ({}) product={name:?}",
            device.pointerDeviceType.0
        );
        println!(
            "  handle={:?} max_active_contacts={} starting_cursor_id={}",
            device.device, device.maxActiveContacts, device.startingCursorId
        );

        let properties: Vec<POINTER_DEVICE_PROPERTY> = match query_list(|count, buffer| {
            // SAFETY: The handle came from GetPointerDevices; count and buffer are valid.
            unsafe { GetPointerDeviceProperties(device.device, count, buffer) }
        }) {
            Ok(properties) => properties,
            Err(error) => {
                eprintln!("  GetPointerDeviceProperties failed: {error}");
                continue;
            }
        };
        println!("  properties: {}", properties.len());
        for (index, property) in properties.iter().enumerate() {
            let label = if property.usagePageId == HID_USAGE_PAGE_DIGITIZER
                && property.usageId == HID_USAGE_DIGITIZER_TIP_PRESSURE
            {
                " TIP_PRESSURE"
            } else {
                ""
            };
            println!(
                "  [{index:02}] page=0x{:04X} usage=0x{:04X}{label} \
                logical={}..={} physical={}..={} unit=0x{:08X} exponent=0x{:08X}",
                property.usagePageId,
                property.usageId,
                property.logicalMin,
                property.logicalMax,
                property.physicalMin,
                property.physicalMax,
                property.unit,
                property.unitExponent
            );
        }
    }
    Ok(())
}

#[cfg(windows)]
fn query_list<T: Default + Clone>(
    mut query: impl FnMut(&mut u32, Option<*mut T>) -> windows::core::Result<()>,
) -> Result<Vec<T>, Box<dyn std::error::Error>> {
    // A device can disconnect or change the required count between the two calls.
    for _ in 0..3 {
        let mut count = 0;
        query(&mut count, None)?;
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut values = vec![T::default(); count as usize];
        let capacity = count;
        let result = query(&mut count, Some(values.as_mut_ptr()));
        if count > capacity {
            continue;
        }
        result?;
        values.truncate(count as usize);
        return Ok(values);
    }
    Err(std::io::Error::other("device/property count kept changing; please retry").into())
}
