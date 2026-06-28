use anyhow::Result;
use haptics_probe_common::FfEffect;
use std::str::FromStr;
use evdev::{Device, EventType, InputEvent};
use std::path::{Path, PathBuf};

/// Metadata about one FF-capable input device.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeviceInfo {
    pub device_id: String,
    pub name: String,
    pub path: String,
}

/// Enumerate all /dev/input/event* devices that support EV_FF.
pub fn list_ff_devices() -> Result<Vec<DeviceInfo>> {
    let mut result = Vec::new();
    let entries = std::fs::read_dir("/dev/input")?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.to_str().map(|s| s.contains("event")).unwrap_or(false) {
            continue;
        }
        if let Ok(dev) = Device::open(&path) {
            if dev.supported_events().contains(EventType::FORCEFEEDBACK) {
                let device_id = stable_id(&dev, &path);
                let name = dev.name().unwrap_or("Unknown").to_string();
                result.push(DeviceInfo {
                    device_id,
                    name,
                    path: path.to_string_lossy().into_owned(),
                });
            }
        }
    }
    Ok(result)
}

/// Derive a stable device ID: phys string, falling back to basename.
pub fn stable_id(dev: &Device, path: &Path) -> String {
    dev.physical_path()
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .unwrap_or_else(|| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
}

/// A single event from an evdev FF device.
#[derive(Debug, Clone)]
pub enum FfEvent {
    Play { effect_id: i16 },
    Stop { effect_id: i16 },
}

/// Read the next FF play/stop event from an evdev device (non-async).
pub fn next_ff_event(dev: &mut Device) -> Result<FfEvent> {
    loop {
        for ev in dev.fetch_events()? {
            if ev.event_type() == EventType::FORCEFEEDBACK {
                let code: i32 = ev.code();
                let value: i16 = ev.value();
                let effect_id = code as i16;
                return Ok(if value != 0 {
                    FfEvent::Play { effect_id }
                } else {
                    FfEvent::Stop { effect_id }
                });
            }
        }
    }
}
