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
    env_logger::init();
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
    for info in &devices_info {
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
            if let Ok(mut dev) = evdev::Device::open(&path) {
                loop {
                    match next_ff_event(&mut dev) {
                        Ok(ev) => { let _ = tx.blocking_send((device_id.clone(), ev)); }
                        Err(e) => {
                            log::error!("evdev read error on {}: {}", device_id, e);
                            break;
                        }
                    }
                }
            }
        });
    }

    loop {
        tokio::select! {
            Some(uploaded) = effect_rx.recv() => {
                effect_store.insert((uploaded.tgid, uploaded.effect_id), uploaded.effect);
            }
            Some((device_id, ev)) = ff_rx.recv() => {
                match ev {
                    FfEvent::Stop { effect_id } => {
                        effect_store.retain(|(_, id), _| *id != effect_id);
                        emit_bson_doc(&doc! { "type": "stop", "device": &device_id });
                    }
                    FfEvent::Play { effect_id } => {
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
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn emit_bson_doc(doc: &bson::Document) {
    if let Ok(bytes) = to_vec(doc) {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(&bytes);
        let _ = stdout.flush();
    }
}
