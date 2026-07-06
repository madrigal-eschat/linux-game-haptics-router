use anyhow::{Context, Result};
use evdev::Device;
use linux_game_haptics_router_e2e::gamepad::spawn_fake_gamepad;
use linux_game_haptics_router_e2e::protocol_server::{spawn_fake_server, ReceivedCommand};
use linux_game_haptics_router_e2e::scenarios::smoke_scenarios;
use linux_game_haptics_router_e2e::timing::{
    assert_command_within_bound, assert_final_zero_within_bound, expected_end_time,
};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedReceiver;

/// Drains everything currently queued on the channel without blocking past
/// `deadline`, so assertions see a stable snapshot of what's arrived so far.
async fn drain_until(
    rx: &mut UnboundedReceiver<ReceivedCommand>,
    deadline: Instant,
) -> Vec<ReceivedCommand> {
    let mut out = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(cmd)) => out.push(cmd),
            _ => break,
        }
    }
    out
}

fn spawn_daemon(ws_url: &str, device_id: &str) -> Result<Child> {
    Command::new("./game-haptics-router")
        .arg("--ws-url")
        .arg(ws_url)
        .arg("--device-map")
        .arg(format!("{{\"{}\": null}}", device_id))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn ./game-haptics-router — did run.sh scp it into the cwd?")
}

fn device_id_for(path: &std::path::Path) -> Result<String> {
    let dev = Device::open(path).context("opening virtual gamepad to derive its device_id")?;
    Ok(dev
        .physical_path()
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .unwrap_or_else(|| path.file_name().unwrap().to_string_lossy().into_owned()))
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let (addr, mut rx) = spawn_fake_server().await?;
    let ws_url = format!("ws://{}", addr);

    let gamepad = spawn_fake_gamepad("e2e Fake Gamepad")?;
    let device_id = device_id_for(&gamepad.device_node)?;

    let mut daemon = spawn_daemon(&ws_url, &device_id)?;
    // The daemon connects to the fake server, then opens the evdev device
    // and starts reading FF events — give it a moment before issuing gestures.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut game_dev =
        Device::open(&gamepad.device_node).context("opening virtual gamepad as \"the game\"")?;

    let mut failures = Vec::new();

    for scenario in smoke_scenarios() {
        let mut effect = match game_dev.upload_ff_effect(scenario.effect) {
            Ok(e) => e,
            Err(e) => {
                failures.push(format!("{}: upload_ff_effect failed: {}", scenario.name, e));
                continue;
            }
        };
        let issued_at = Instant::now();
        if let Err(e) = effect.play(1) {
            failures.push(format!("{}: play failed: {}", scenario.name, e));
            continue;
        }

        let expected_end = expected_end_time(issued_at, scenario.expected_end_ms);
        let deadline = expected_end + Duration::from_millis(500);
        let commands = drain_until(&mut rx, deadline).await;

        match assert_command_within_bound(issued_at, &commands) {
            Ok(_) => {}
            Err(e) => failures.push(format!("{}: {}", scenario.name, e)),
        }
        match assert_final_zero_within_bound(expected_end, &commands) {
            Ok(()) => println!("PASS {}", scenario.name),
            Err(e) => failures.push(format!("{}: {}", scenario.name, e)),
        }
    }

    daemon.kill().ok();
    if let Some(stdout) = daemon.stdout.take() {
        use std::io::Read;
        let mut s = String::new();
        let _ = std::io::BufReader::new(stdout).read_to_string(&mut s);
        log::info!("daemon stdout:\n{}", s);
    }

    if failures.is_empty() {
        println!("all scenarios passed");
        Ok(())
    } else {
        for f in &failures {
            eprintln!("FAIL {}", f);
        }
        anyhow::bail!("{} scenario(s) failed", failures.len());
    }
}
