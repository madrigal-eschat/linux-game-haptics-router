mod device;
mod translate;
mod throttle;
mod ebpf;

use anyhow::Result;
use bson::{doc, to_vec};
use device::{list_ff_devices, next_ff_event, stable_id, FfEvent};
use haptics_probe_common::{FfEffect, ProbeEvent};
use std::collections::HashMap;
use std::io::Write;
use tokio::sync::mpsc;

/// Load and attach the eBPF program. Returns a receiver for effect-upload events.
pub async fn load_probe() -> Result<(aya::Bpf, mpsc::Receiver<ProbeEvent>)> {
    let bpf_bytes = include_bytes_aligned!(
        "../../target/bpfel-unknown-none/debug/haptics-probe-ebpf"
    );
    let mut bpf = aya::Bpf::load(bpf_bytes)?;

    // Attach tracepoints
    let enter: &mut aya::programs::TracePoint = bpf.program_mut("sys_enter_ioctl").unwrap().try_into()?;
    enter.load()?;
    enter.attach("syscalls", "sys_enter_ioctl")?;

    let exit: &mut aya::programs::TracePoint = bpf.program_mut("sys_exit_ioctl").unwrap().try_into()?;
    exit.load()?;
    exit.attach("syscalls", "sys_exit_ioctl")?;

    let (tx, rx) = mpsc::channel(256);

    // Poll ring buffer in background task
    let mut ring: aya::maps::RingBuf<ProbeEvent> = bpf.map_mut("EVENTS").unwrap().try_into()?;
    tokio::spawn(async move {
        loop {
            tokio::task::yield_now().await;
            // Drain available events
            while let Some(item) = ring.next() {
                let bytes: &[u8] = &item;
                if bytes.len() < std::mem::size_of::<ProbeEvent>() { continue; }
                let event: ProbeEvent = unsafe {
                    std::ptr::read_unaligned(bytes.as_ptr() as *const ProbeEvent)
                };
                let _ = tx.try_send(event);
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        }
    });

    Ok((bpf, rx))
}

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
    let (_bpf, mut effect_rx) = load_probe().await?;

    // Open all FF devices for evdev reading
    let devices_info = list_ff_devices()?;
    let mut effect_store: HashMap<(u32, i16), FfEffect> = HashMap::new();
    let mut throttle = throttle::Throttle::new();

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
                effect_store.insert((uploaded.tgid, uploaded.effect_id), uploaded);
            }
            Some((device_id, ev)) = ff_rx.recv() => {
                match ev {
                    FfEvent::Stop { effect_id } => {
                        effect_store.remove(&(0, effect_id));
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
