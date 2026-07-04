mod device;
mod ebpf;
mod playback;
mod translate;
mod throttle;

use anyhow::{Context, Result};
use device::{list_ff_devices, next_ff_event, FfEvent};
use haptics_probe_common::FfEffect;
use playback::{DeviceMap, Playback};
use std::collections::HashMap;
use throttle::Throttle;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

struct Args {
    ws_url: String,
    scale: f32,
    device_map: DeviceMap,
}

fn parse_args() -> Result<Args> {
    let raw: Vec<String> = std::env::args().collect();
    let mut ws_url = None;
    let mut scale = 1.0f32;
    let mut device_map = DeviceMap::new();
    let mut i = 1;
    while i < raw.len() {
        match raw[i].as_str() {
            "--ws-url" => {
                ws_url = Some(raw.get(i + 1).context("--ws-url needs a value")?.clone());
                i += 2;
            }
            "--scale" => {
                scale = raw
                    .get(i + 1)
                    .context("--scale needs a value")?
                    .parse()
                    .context("--scale must be a float")?;
                i += 2;
            }
            "--device-map" => {
                let json = raw.get(i + 1).context("--device-map needs a value")?;
                device_map = serde_json::from_str(json).context("--device-map must be JSON")?;
                i += 2;
            }
            other => anyhow::bail!("unrecognized argument: {other}"),
        }
    }
    Ok(Args {
        ws_url: ws_url.context("--ws-url is required")?,
        scale,
        device_map,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let raw_args: Vec<String> = std::env::args().collect();

    if raw_args.get(1).map(|s| s.as_str()) == Some("--list-devices") {
        let devices = list_ff_devices()?;
        println!("{}", serde_json::to_string(&devices)?);
        return Ok(());
    }

    let args = parse_args()?;

    // Connect to the buttplug/intiface engine ourselves — playback lives
    // entirely in this process now, Python just supplies config.
    let playback = Playback::connect_with_retry(&args.ws_url, args.scale, args.device_map).await?;

    // Live scale updates from Python arrive as JSON lines on stdin, e.g.
    // {"scale": 0.8}\n — everything else about playback is fixed at startup.
    {
        let playback = playback.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                match serde_json::from_str::<serde_json::Value>(&line) {
                    Ok(v) => {
                        if let Some(scale) = v.get("scale").and_then(|s| s.as_f64()) {
                            log::info!("scale updated to {}", scale);
                            playback.set_scale(scale as f32);
                        }
                    }
                    Err(e) => log::warn!("bad stdin control line {:?}: {}", line, e),
                }
            }
        });
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

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                log::info!("received SIGTERM, stopping all devices before exit");
                playback.stop_all().await;
                return Ok(());
            }
            _ = tokio::signal::ctrl_c() => {
                log::info!("received ctrl-c, stopping all devices before exit");
                playback.stop_all().await;
                return Ok(());
            }
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
                        playback.stop(&device_id).await;
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
                                    playback.schedule_sequence(device_id.clone(), points).await;
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
    log::info!("device added: {} ({}) at {}", info.device_id, info.name, info.path);
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
