use anyhow::{Context, Result};
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventSummary, FFEffectCode, UInputCode};
use std::collections::BTreeSet;
use std::path::PathBuf;

pub struct FakeGamepad {
    pub device_node: PathBuf,
}

/// Creates a virtual FF-capable gamepad via uinput and spawns a background
/// thread that auto-acknowledges every force-feedback upload/erase request
/// the kernel routes back to this uinput fd (required before any EVIOCSFF
/// ioctl issued against the resulting device node can complete).
pub fn spawn_fake_gamepad(name: &str) -> Result<FakeGamepad> {
    let mut device = VirtualDevice::builder()
        .context("opening /dev/uinput (needs root or a uinput udev rule)")?
        .name(name)
        .with_ff(&AttributeSet::from_iter([
            FFEffectCode::FF_RUMBLE,
            FFEffectCode::FF_PERIODIC,
            FFEffectCode::FF_CONSTANT,
            FFEffectCode::FF_RAMP,
            // The kernel's FF_PERIODIC validation (input_ff_upload in
            // drivers/input/ff-core.c) checks the specific waveform bit
            // against the same capability bitmap as the effect-type bits
            // above, not just FF_PERIODIC itself — an upload with a waveform
            // whose bit isn't advertised here fails EVIOCSFF with EINVAL
            // even though FF_PERIODIC is advertised. Advertise every
            // standard (non-custom) waveform so any periodic scenario works.
            FFEffectCode::FF_SQUARE,
            FFEffectCode::FF_TRIANGLE,
            FFEffectCode::FF_SINE,
            FFEffectCode::FF_SAW_UP,
            FFEffectCode::FF_SAW_DOWN,
        ]))
        .context("advertising FF effect types")?
        .with_ff_effects_max(16)
        .build()
        .context("creating uinput device")?;

    let device_node = device
        .enumerate_dev_nodes_blocking()
        .context("enumerating uinput device nodes")?
        .next()
        .context("no /dev/input node was created for the virtual gamepad")??;

    std::thread::Builder::new()
        .name(format!("ff-ack-{}", name))
        .spawn(move || {
            let mut free_ids: BTreeSet<i16> = (0..16).collect();
            loop {
                let events: Vec<_> = match device.fetch_events() {
                    Ok(evs) => evs.collect(),
                    Err(e) => {
                        log::warn!("gamepad FF ack loop exiting: fetch_events failed: {}", e);
                        return;
                    }
                };
                for event in events {
                    match event.destructure() {
                        EventSummary::UInput(event, UInputCode::UI_FF_UPLOAD, ..) => {
                            if let Ok(mut upload) = device.process_ff_upload(event) {
                                match free_ids.iter().next().copied() {
                                    Some(id) => {
                                        free_ids.remove(&id);
                                        upload.set_effect_id(id);
                                        upload.set_retval(0);
                                    }
                                    None => upload.set_retval(-1),
                                }
                            }
                        }
                        EventSummary::UInput(event, UInputCode::UI_FF_ERASE, ..) => {
                            if let Ok(erase) = device.process_ff_erase(event) {
                                free_ids.insert(erase.effect_id() as i16);
                            }
                        }
                        _ => {}
                    }
                }
            }
        })
        .context("spawning FF ack thread")?;

    Ok(FakeGamepad { device_node })
}
