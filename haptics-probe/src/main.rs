mod device;
mod ebpf;
mod translate;
mod throttle;

use anyhow::Result;
use bson::{doc, to_vec};
use device::{list_ff_devices, next_ff_event, FfEvent};
use haptics_probe_common::FfEffect;
use std::collections::HashMap;
use std::io::Write;
use throttle::Throttle;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().collect();

    if args.get(1).map(|s| s.as_str()) == Some("--list-devices") {
        let devices = list_ff_devices()?;
        println!("{}", serde_json::to_string(&devices)?);
        return Ok(());
    }

    // Load eBPF probe
    let (_bpf, mut effect_rx) = ebpf::load_probe().await?;

    // Open all FF devices for evdev reading
    let devices_info = list_ff_devices()?;
    let mut effect_store: HashMap<(u32, i16), FfEffect> = HashMap::new();
    let mut throttle = Throttle::new();

    // Spawn per-device evdev readers
    let (ff_tx, mut ff_rx) = mpsc::channel::<(String, FfEvent)>(256);
    let mut known_devices: HashMap<String, String> = HashMap::new(); // device_id → path
    for info in &devices_info {
        spawn_device_reader(info, &ff_tx);
        known_devices.insert(info.device_id.clone(), info.path.clone());
    }

    // Periodically rescan for devices that appear after startup, or
    // reappear at a new /dev/input path after a reconnect.
    let mut rescan_interval = tokio::time::interval(std::time::Duration::from_secs(5));

    loop {
        tokio::select! {
            _ = rescan_interval.tick() => {
                if let Ok(current) = list_ff_devices() {
                    for info in &current {
                        let is_new = match known_devices.get(&info.device_id) {
                            None => true,
                            Some(known_path) => known_path != &info.path,
                        };
                        if is_new {
                            log::info!("rescan: device {} ({}) at {}", info.device_id, info.name, info.path);
                            spawn_device_reader(info, &ff_tx);
                            known_devices.insert(info.device_id.clone(), info.path.clone());
                        }
                    }
                }
            }
            Some(uploaded) = effect_rx.recv() => {
                log::info!(
                    "effect_store: inserting tgid={} effect_id={}",
                    uploaded.tgid, uploaded.effect_id
                );
                // A numeric effect_id is only meaningful for the most recent upload;
                // the kernel reuses ids across processes/sessions and stale entries
                // from prior tgids are never otherwise cleaned up (no Stop write
                // arrives for effects that finish naturally or are removed via
                // EVIOCRMFF), so purge any other tgid's entry for this id first.
                effect_store.retain(|(_, id), _| *id != uploaded.effect_id);
                effect_store.insert((uploaded.tgid, uploaded.effect_id), uploaded.effect);
            }
            Some((device_id, ev)) = ff_rx.recv() => {
                match ev {
                    FfEvent::Stop { effect_id } => {
                        log::info!("FF event: Stop effect_id={} on {}", effect_id, device_id);
                        effect_store.retain(|(_, id), _| *id != effect_id);
                        emit_bson_doc(&doc! { "type": "stop", "device": &device_id });
                    }
                    FfEvent::Play { effect_id } => {
                        log::info!("FF event: Play effect_id={} on {}", effect_id, device_id);
                        let maybe_effect = effect_store.values()
                            .find(|e| e.id == effect_id)
                            .copied();

                        if let Some(effect) = maybe_effect {
                            if throttle.should_emit_haptic() {
                                let points = translate::translate(&effect);
                                if !points.is_empty() {
                                    let bson_points: Vec<bson::Document> = points.iter().map(|p| {
                                        doc! { "dt_ms": p.dt_ms as i64, "intensity": p.intensity as f64 }
                                    }).collect();
                                    emit_bson_doc(&doc! {
                                        "type": "haptic",
                                        "device": &device_id,
                                        "points": bson_points,
                                    });
                                    throttle.record_haptic_emitted();
                                } else {
                                    log::info!("Play effect_id={}: found effect but no points produced (kind={})", effect_id, effect.kind);
                                }
                            } else {
                                log::info!("Play effect_id={}: throttled, dropping", effect_id);
                            }
                        } else {
                            log::info!(
                                "Play effect_id={}: no matching effect in store ({} known)",
                                effect_id, effect_store.len()
                            );
                        }
                    }
                }
            }
        }
    }
}

fn spawn_device_reader(info: &device::DeviceInfo, ff_tx: &mpsc::Sender<(String, FfEvent)>) {
    emit_bson_doc(&doc! {
        "type": "device_added",
        "device": &info.device_id,
        "name": &info.name,
        "path": &info.path,
    });
    let tx = ff_tx.clone();
    let path = info.path.clone();
    let device_id = info.device_id.clone();
    tokio::task::spawn_blocking(move || {
        let mut backoff = std::time::Duration::from_millis(200);
        const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(5);
        loop {
            match evdev::Device::open(&path) {
                Ok(mut dev) => {
                    backoff = std::time::Duration::from_millis(200);
                    loop {
                        match next_ff_event(&mut dev) {
                            Ok(ev) => { let _ = tx.blocking_send((device_id.clone(), ev)); }
                            Err(e) => {
                                log::warn!(
                                    "evdev read error on {}: {}, will retry reopening",
                                    device_id, e
                                );
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    log::warn!("failed to open {}: {}, will retry", path, e);
                }
            }
            std::thread::sleep(backoff);
            backoff = std::cmp::min(backoff * 2, MAX_BACKOFF);
        }
    });
}

fn emit_bson_doc(doc: &bson::Document) {
    if let Ok(bytes) = to_vec(doc) {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(&bytes);
        let _ = stdout.flush();
    }
}
