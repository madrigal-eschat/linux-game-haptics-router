use anyhow::{Context, Result};
use evdev::{Device, FFEffectData, FFEffectKind, FFReplay, FFTrigger};
use linux_game_haptics_router_e2e::gamepad::spawn_fake_gamepad;
use linux_game_haptics_router_e2e::protocol_server::{spawn_fake_server, ReceivedCommand};
use linux_game_haptics_router_e2e::scenarios::smoke_scenarios;
use linux_game_haptics_router_e2e::timing::{
    assert_command_within_bound, assert_final_zero_within_bound, expected_end_time, LATENCY_BOUND,
};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
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

/// A short rumble effect, mirroring `scenarios::smoke_scenarios()`'s
/// `ff_rumble` case, for the multi-gesture scenarios built directly in this
/// orchestrator (rapid retrigger, multi-device) that need more than one
/// gamepad/gesture and so don't fit `scenarios::Scenario`'s one-shot shape.
fn rumble_effect(length_ms: u16) -> FFEffectData {
    FFEffectData {
        direction: 0,
        trigger: FFTrigger::default(),
        replay: FFReplay {
            length: length_ms,
            delay: 0,
        },
        kind: FFEffectKind::Rumble {
            strong_magnitude: 0xffff,
            weak_magnitude: 0xffff,
        },
    }
}

/// Kills (and reaps) the wrapped daemon child on drop, so every exit path out
/// of `main` — including an early `?` return partway through setup — cleans
/// up the subprocess. `Child`'s own `Drop` impl does *not* do this on Unix.
struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_daemon(ws_url: &str, device_map_json: &str) -> Result<Child> {
    Command::new("./game-haptics-router")
        .arg("--ws-url")
        .arg(ws_url)
        .arg("--device-map")
        .arg(device_map_json)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn ./game-haptics-router — did run.sh scp it into the cwd?")
}

/// Spawns a background thread that reads `pipe` to completion (EOF, i.e. the
/// daemon exiting/closing the fd), accumulating everything into the returned
/// `Arc<Mutex<String>>`. Used for both the daemon's stdout and stderr so
/// neither pipe ever fills up while scenarios are running: `env_logger` at
/// the daemon's default `info` level is verbose (every effect upload, every
/// FF event, a full per-schedule keyframe dump from
/// `playback::format_schedule_log`), and across the whole run the volume on
/// stderr in particular can approach the OS pipe buffer size. If nothing
/// drains a full pipe, the daemon blocks inside its own `write()` call,
/// stalling the process under test and corrupting the very timing
/// assertions this harness exists to make.
fn spawn_pipe_drain<R>(pipe: R) -> (Arc<Mutex<String>>, std::thread::JoinHandle<()>)
where
    R: std::io::Read + Send + 'static,
{
    let buf = Arc::new(Mutex::new(String::new()));
    let buf_writer = Arc::clone(&buf);
    let handle = std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let mut reader = BufReader::new(pipe);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let mut guard = buf_writer.lock().unwrap();
                    guard.push_str(&line);
                }
                Err(_) => break,
            }
        }
    });
    (buf, handle)
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

    // A second virtual gamepad, spawned up front (device_map is fixed for
    // the daemon's whole lifetime) so the multi-device scenario below can
    // exercise per-device routing without restarting the daemon. It's
    // mapped to buttplug device index 99, which the fake server never
    // advertises (it only ever advertises index 0) — see that scenario's
    // comment for why this is the isolation check we can actually make.
    let gamepad_b = spawn_fake_gamepad("e2e Fake Gamepad B")?;
    let device_id_b = device_id_for(&gamepad_b.device_node)?;

    let device_map_json = format!(r#"{{"{}": null, "{}": [99]}}"#, device_id, device_id_b);
    let mut daemon_child = spawn_daemon(&ws_url, &device_map_json)?;
    // Drain stdout/stderr concurrently for the daemon's whole lifetime (see
    // `spawn_pipe_drain`'s doc comment) rather than reading them once at the
    // end — otherwise a filled pipe buffer stalls the daemon mid-scenario.
    // `spawn_daemon` always requests `Stdio::piped()` for both, so these
    // `.take()`s are expected to succeed.
    let (daemon_stdout, stdout_drain) =
        spawn_pipe_drain(daemon_child.stdout.take().context("daemon stdout not piped")?);
    let (daemon_stderr, stderr_drain) =
        spawn_pipe_drain(daemon_child.stderr.take().context("daemon stderr not piped")?);
    let mut daemon = DaemonGuard(daemon_child);
    // The daemon connects to the fake server, then opens the evdev device
    // and starts reading FF events — give it a moment before issuing gestures.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut game_dev =
        Device::open(&gamepad.device_node).context("opening virtual gamepad as \"the game\"")?;

    let mut failures = Vec::new();

    for scenario in smoke_scenarios() {
        // NB: `upload_ff_effect` below reaches the daemon's effect table via
        // the eBPF ring-buffer path, while the `play()` call a few lines down
        // reaches it via a separate evdev-reader channel — there's no
        // ordering guarantee between the two. If `Play` were somehow
        // serviced first, the daemon would log "no matching effect in store"
        // and silently drop it. In practice the eBPF path is fast enough
        // that this hasn't been observed to flake, and it's an inherent
        // property of the two-channel design under test here, not a harness
        // bug — not something to paper over with a retry/sleep.
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

        let mut scenario_failed = false;
        if let Err(e) = assert_command_within_bound(issued_at, &commands) {
            failures.push(format!("{}: {}", scenario.name, e));
            scenario_failed = true;
        }
        if let Err(e) = assert_final_zero_within_bound(expected_end, &commands) {
            failures.push(format!("{}: {}", scenario.name, e));
            scenario_failed = true;
        }
        if !scenario_failed {
            println!("PASS {}", scenario.name);
        }
    }

    // ── Scenario 4: rapid retrigger ──
    // Two Play events issued ~5ms apart — well inside the daemon's 10ms
    // throttle window (linux-game-haptics-router/src/throttle.rs,
    // MIN_INTERVAL_MS = 10). Which Play's haptic emission actually gets
    // through is inherently a race: the throttle may swallow the first one
    // entirely. So rather than asserting which Play "wins", we assert the
    // weaker but still meaningful property that whichever Play does get
    // through still lands its buttplug command within the latency bound,
    // measured from the *second* issue time (the later, more conservative
    // reference point — a command that beat the bound relative to the first
    // issue necessarily beats it relative to the second too).
    {
        let name = "rapid_retrigger";
        // See the same-named note in the smoke-scenarios loop above: upload
        // and play race across two independent daemon-side channels
        // (eBPF ring buffer vs. evdev reader). Inherent to the design under
        // test, not a harness bug.
        match game_dev.upload_ff_effect(rumble_effect(300)) {
            Ok(mut effect) => {
                if let Err(e) = effect.play(1) {
                    failures.push(format!("{}: first play failed: {}", name, e));
                } else {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    let second_issue = Instant::now();
                    if let Err(e) = effect.play(1) {
                        failures.push(format!("{}: second play failed: {}", name, e));
                    } else {
                        let deadline = second_issue + LATENCY_BOUND + Duration::from_millis(500);
                        let commands = drain_until(&mut rx, deadline).await;
                        match assert_command_within_bound(second_issue, &commands) {
                            Ok(_) => println!("PASS {}", name),
                            Err(e) => failures.push(format!("{}: {}", name, e)),
                        }
                    }
                }
            }
            Err(e) => failures.push(format!("{}: upload_ff_effect failed: {}", name, e)),
        }
    }

    // ── Scenario 5: multi-device isolation ──
    // The fake buttplug server (protocol_server.rs's device_list_message)
    // only ever advertises a single device at index 0, so we cannot
    // distinguish gamepad A's wire traffic from gamepad B's by
    // ReceivedCommand::device_index/feature_index — both would report 0
    // either way regardless of which virtual gamepad triggered them. A
    // genuine per-device-index crosstalk assertion isn't achievable against
    // this fake server's single-device model.
    //
    // Instead this leans on the *client-side* target-list filtering in
    // Playback::send_scalar (linux-game-haptics-router/src/playback.rs):
    // gamepad B's --device-map entry above points at buttplug device index
    // 99, which doesn't exist, so every OutputCmd for gamepad B's effects
    // should be filtered out client-side and nothing should arrive at all,
    // while gamepad A's `null` (broadcast) entry still reaches the one real
    // device. If the daemon's per-device_id routing ever got crossed — B's
    // gestures picking up A's target list or vice versa — this would flip:
    // B would start producing commands, or A would stop.
    {
        let name = "multi_device_isolation";
        let mut scenario_failed = false;

        match Device::open(&gamepad_b.device_node) {
            Ok(mut game_dev_b) => {
                // Gamepad A: broadcast-mapped, should produce a command.
                // (Same upload-vs-play race noted above applies to every
                // upload_ff_effect call in this file — inherent, not a bug.)
                match game_dev.upload_ff_effect(rumble_effect(200)) {
                    Ok(mut effect) => {
                        let issued_at = Instant::now();
                        if let Err(e) = effect.play(1) {
                            failures.push(format!("{} (gamepad A): play failed: {}", name, e));
                            scenario_failed = true;
                        } else {
                            let deadline = issued_at + LATENCY_BOUND + Duration::from_millis(500);
                            let commands = drain_until(&mut rx, deadline).await;
                            if let Err(e) = assert_command_within_bound(issued_at, &commands) {
                                failures.push(format!("{} (gamepad A): {}", name, e));
                                scenario_failed = true;
                            }
                        }
                    }
                    Err(e) => {
                        failures.push(format!(
                            "{} (gamepad A): upload_ff_effect failed: {}",
                            name, e
                        ));
                        scenario_failed = true;
                    }
                }

                // Gamepad B: mapped to buttplug device index 99, which the
                // fake server never advertises — nothing should arrive.
                match game_dev_b.upload_ff_effect(rumble_effect(200)) {
                    Ok(mut effect) => {
                        if let Err(e) = effect.play(1) {
                            failures.push(format!("{} (gamepad B): play failed: {}", name, e));
                            scenario_failed = true;
                        } else {
                            let deadline = Instant::now() + Duration::from_millis(700);
                            let commands = drain_until(&mut rx, deadline).await;
                            if !commands.is_empty() {
                                failures.push(format!(
                                    "{} (gamepad B): expected no commands routed to a \
                                     nonexistent target index, got {}",
                                    name,
                                    commands.len()
                                ));
                                scenario_failed = true;
                            }
                        }
                    }
                    Err(e) => {
                        failures.push(format!(
                            "{} (gamepad B): upload_ff_effect failed: {}",
                            name, e
                        ));
                        scenario_failed = true;
                    }
                }
            }
            Err(e) => {
                failures.push(format!(
                    "{}: opening gamepad B as \"the game\" failed: {}",
                    name, e
                ));
                scenario_failed = true;
            }
        }

        if !scenario_failed {
            println!("PASS {}", name);
        }
    }

    // Kill+wait explicitly here (rather than just letting `daemon` drop) so
    // the daemon has actually exited and flushed its output before we read
    // the drain threads' accumulated buffers below — `DaemonGuard`'s own
    // `Drop` will run its own kill()/wait() afterwards when `daemon` goes out
    // of scope, but by then the child is already reaped, so that second
    // kill()/wait() is a harmless no-op, not a bug.
    let _ = daemon.0.kill();
    let _ = daemon.0.wait();
    // Join the drain threads so they've observed EOF on both pipes (the
    // daemon having exited above guarantees that EOF is imminent) before we
    // read out everything they've accumulated.
    let _ = stdout_drain.join();
    let _ = stderr_drain.join();
    log::info!("daemon stdout:\n{}", daemon_stdout.lock().unwrap());
    log::info!("daemon stderr:\n{}", daemon_stderr.lock().unwrap());

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
