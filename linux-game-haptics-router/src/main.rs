mod app;
mod device;
mod ebpf;
mod playback;
mod throttle;
mod translate;

use anyhow::Result;
use app::App;
use clap::Parser;
use device::{list_ff_devices, FfEvent};
use playback::{DeviceMap, Playback};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

#[derive(Parser)]
struct Args {
    /// Print FF-capable input devices as JSON and exit
    #[arg(long)]
    list_devices: bool,

    /// Buttplug/Intiface websocket URL to connect to
    #[arg(long, required_unless_present = "list_devices")]
    ws_url: Option<String>,

    /// Global intensity scale applied to every haptic command
    #[arg(long, default_value_t = 1.0)]
    scale: f32,

    /// JSON map of evdev device_id -> target buttplug device indices (or null to broadcast)
    #[arg(long, value_parser = parse_device_map, default_value = "{}")]
    device_map: DeviceMap,
}

fn parse_device_map(json: &str) -> Result<DeviceMap, String> {
    serde_json::from_str(json).map_err(|e| format!("--device-map must be JSON: {e}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();

    if args.list_devices {
        let devices = list_ff_devices()?;
        println!("{}", serde_json::to_string(&devices)?);
        return Ok(());
    }

    // Connect to the buttplug/intiface engine ourselves — playback lives
    // entirely in this process now, Python just supplies config.
    let ws_url = args
        .ws_url
        .expect("clap requires --ws-url unless --list-devices");
    let playback = Playback::connect_with_retry(&ws_url, args.scale, args.device_map).await?;

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

    // Spawn per-device evdev readers
    let (ff_tx, mut ff_rx) = mpsc::channel::<(String, FfEvent)>(256);
    let mut app = App::new(playback.clone(), ff_tx);
    for info in &list_ff_devices()? {
        app.spawn_reader(info);
    }

    // Periodically rescan for devices that appear after startup, or
    // reappear at a new /dev/input path after a reconnect.
    let mut rescan_interval = tokio::time::interval(std::time::Duration::from_secs(5));

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                log::info!("received SIGTERM, stopping all devices before exit");
                app.stop_all().await;
                return Ok(());
            }
            _ = tokio::signal::ctrl_c() => {
                log::info!("received ctrl-c, stopping all devices before exit");
                app.stop_all().await;
                return Ok(());
            }
            _ = rescan_interval.tick() => {
                app.rescan_devices();
            }
            Some(msg) = effect_rx.recv() => {
                match msg {
                    ebpf::ProbeEventMsg::Uploaded(uploaded) => {
                        app.handle_effect_uploaded(uploaded).await;
                    }
                    ebpf::ProbeEventMsg::Erased { tgid, effect_id } => {
                        app.handle_effect_erased(tgid, effect_id).await;
                    }
                }
            }
            Some((device_id, ev)) = ff_rx.recv() => {
                app.handle_ff_event(device_id, ev).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_required_without_list_devices() {
        let result = Args::try_parse_from(["game-haptics-router"]);
        assert!(result.is_err());
    }

    #[test]
    fn ws_url_not_required_with_list_devices() {
        let args = Args::try_parse_from(["game-haptics-router", "--list-devices"]).unwrap();
        assert!(args.list_devices);
        assert!(args.ws_url.is_none());
    }

    #[test]
    fn ws_url_accepted_and_defaults_applied() {
        let args =
            Args::try_parse_from(["game-haptics-router", "--ws-url", "ws://localhost:12345"])
                .unwrap();
        assert_eq!(args.ws_url.as_deref(), Some("ws://localhost:12345"));
        assert_eq!(args.scale, 1.0);
        assert!(args.device_map.is_empty());
    }
}
