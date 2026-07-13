# E2E VM Test Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a two-layer end-to-end test that boots a QEMU VM matching the host arch, runs a real `game-haptics-router` daemon against a virtual FF gamepad and an in-process fake buttplug server inside it, and asserts two timing bounds: buttplug commands arrive within 150ms of a gesture being issued, and the final zero-magnitude command arrives within 150ms of the gesture's expected end time.

**Architecture:** A new host-target-only crate `linux-game-haptics-router-e2e` provides a single binary, `e2e-tests`, that creates a virtual FF-capable gamepad via the `evdev` crate's uinput support, runs a fake buttplug server using the real `buttplug_core` message types over a real websocket (so wire compatibility is guaranteed, not hand-approximated), spawns the real daemon as a subprocess, and drives + asserts on a small scenario set. An outer bash script (`e2e/run.sh`) boots the VM, copies binaries in over scp, and runs `e2e-tests` over SSH with a timeout, matching VM arch to host arch.

**Tech Stack:** Rust (tokio, evdev 0.12 uinput/FF API, buttplug_core 10.0.3, tokio-tungstenite 0.28), bash, QEMU + cloud-init.

## Global Constraints

- Timing bound: buttplug command received within **150ms** of gesture issue-time.
- Timing bound: final zero-magnitude command received within **150ms** of expected end time (issue-time + effect `replay.length`, in ms — envelope fade does not shift this, per `translate.rs`'s `sample_range_fn`, which always emits its trailing zero-intensity point at `dt_ms == length` regardless of envelope).
- `linux-game-haptics-router-e2e` crate is host-target only, excluded from default `cargo build/test --workspace` the same way `linux-game-haptics-router-ebpf` is excluded (needs `/dev/uinput` + root; not buildable/runnable in a plain sandboxed dev shell).
- VM arch always matches host arch (`uname -m`) — no cross-arch/TCG emulation.
- CI matrixes `ubuntu-latest` (x86_64) and `ubuntu-24.04-arm` (aarch64).
- No separate fake-controller binary/process and no subprocess for the fake buttplug server — both live in-process inside `e2e-tests`.
- Reuses real `buttplug_core` message types (already a transitive dependency, pinned via `buttplug = "10.0.3"` in the daemon) rather than hand-rolled JSON, for guaranteed wire compatibility with the real `buttplug` client the daemon uses.

---

### Task 1: Scaffold the `linux-game-haptics-router-e2e` crate

**Files:**
- Create: `linux-game-haptics-router-e2e/Cargo.toml`
- Create: `linux-game-haptics-router-e2e/src/lib.rs`
- Create: `linux-game-haptics-router-e2e/src/bin/e2e-tests.rs`
- Modify: `Cargo.toml:2-6` (workspace members)
- Modify: `.github/workflows/ci.yml` (exclude pattern)

**Interfaces:**
- Produces: an empty `linux_game_haptics_router_e2e` lib crate that later tasks add modules to, and a placeholder `e2e-tests` binary that later tasks fill in.

- [ ] **Step 1: Add the crate to the workspace**

Edit `Cargo.toml`:

```toml
[workspace]
members = [
    "linux-game-haptics-router-common",
    "linux-game-haptics-router-ebpf",
    "linux-game-haptics-router",
    "linux-game-haptics-router-e2e",
]
resolver = "2"
```

- [ ] **Step 2: Create the crate manifest**

Create `linux-game-haptics-router-e2e/Cargo.toml`:

```toml
[package]
name = "linux-game-haptics-router-e2e"
version = "0.1.0"
edition = "2021"

[lib]
name = "linux_game_haptics_router_e2e"
path = "src/lib.rs"

[[bin]]
name = "e2e-tests"
path = "src/bin/e2e-tests.rs"

[dependencies]
evdev            = "0.12"
buttplug_core    = "10.0.3"
tokio            = { version = "1", features = ["full"] }
tokio-tungstenite = "0.28"
futures-util     = "0.3"
serde_json       = "1"
anyhow           = "1"
log              = "0.4"
env_logger       = "0.11"
```

- [ ] **Step 3: Create an empty lib and placeholder binary**

Create `linux-game-haptics-router-e2e/src/lib.rs`:

```rust
pub mod protocol_server;
pub mod timing;
pub mod gamepad;
pub mod scenarios;
```

Create `linux-game-haptics-router-e2e/src/bin/e2e-tests.rs`:

```rust
fn main() {
    println!("e2e-tests: not yet implemented");
    std::process::exit(1);
}
```

- [ ] **Step 4: Create empty modules so the crate compiles**

Create `linux-game-haptics-router-e2e/src/protocol_server.rs`:

```rust
// Filled in by Task 2.
```

Create `linux-game-haptics-router-e2e/src/timing.rs`:

```rust
// Filled in by Task 3.
```

Create `linux-game-haptics-router-e2e/src/gamepad.rs`:

```rust
// Filled in by Task 4.
```

Create `linux-game-haptics-router-e2e/src/scenarios.rs`:

```rust
// Filled in by Task 5.
```

- [ ] **Step 5: Verify the workspace builds with the new crate**

Run: `cargo build -p linux-game-haptics-router-e2e`
Expected: builds successfully, produces `target/debug/e2e-tests`.

- [ ] **Step 6: Exclude the e2e crate from default workspace build/test in CI**

Read `.github/workflows/ci.yml` first to find the existing `--exclude linux-game-haptics-router-ebpf` occurrences (fmt/check/test jobs), then add `--exclude linux-game-haptics-router-e2e` alongside each one — e.g. any line of the form:

```yaml
- run: cargo test --workspace --exclude linux-game-haptics-router-ebpf
```

becomes:

```yaml
- run: cargo test --workspace --exclude linux-game-haptics-router-ebpf --exclude linux-game-haptics-router-e2e
```

Apply the same addition to every `cargo build`/`cargo check`/`cargo test`/`cargo clippy` invocation in `ci.yml` that already excludes the ebpf crate. Do this for all such lines in one edit pass (read the file, then apply all changes).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml linux-game-haptics-router-e2e .github/workflows/ci.yml
git commit -m "$(cat <<'EOF'
feat(e2e): scaffold linux-game-haptics-router-e2e crate

Host-target-only crate for the end-to-end VM test harness, excluded from
default workspace build/test/clippy the same way the ebpf crate is.
EOF
)"
```

---

### Task 2: Fake buttplug server (protocol_server module)

**Files:**
- Modify: `linux-game-haptics-router-e2e/src/protocol_server.rs`
- Test: same file, `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: nothing from other e2e-crate modules.
- Produces:
  - `pub struct ReceivedCommand { pub device_index: u32, pub feature_index: u32, pub value: i32, pub at: std::time::Instant }`
  - `pub async fn spawn_fake_server() -> anyhow::Result<(std::net::SocketAddr, tokio::sync::mpsc::UnboundedReceiver<ReceivedCommand>)>` — binds `127.0.0.1:0`, spawns the accept loop as a tokio task, returns the bound address (for building the `ws://` URL the daemon connects to) and a channel that yields every `OutputCmd` the (single) connected client sends, tagged with the `Instant` it was received.

The fake server advertises exactly one device (index 0) with one feature (index 0, a `Vibrate` output, range `0..=100`) — matching what `Playback::send_scalar` in `linux-game-haptics-router/src/playback.rs` targets (it iterates `client.devices()` and calls `run_output` with `ClientDeviceOutputCommand::Vibrate(ClientDeviceCommandValue::Percent(_))`).

- [ ] **Step 1: Write the failing test for a full handshake + command round-trip**

Replace the contents of `linux-game-haptics-router-e2e/src/protocol_server.rs` with:

```rust
use anyhow::{Context, Result};
use buttplug_core::message::{
    ButtplugMessage, ButtplugClientMessageV4, ButtplugServerMessageV4, DeviceFeature,
    DeviceFeatureOutput, DeviceFeatureOutputValueProperties, DeviceListV4, DeviceMessageInfoV4,
    OkV0, OutputCmdV4, RequestServerInfoV4, ServerInfoV4,
};
use buttplug_core::util::range::RangeInclusive;
use buttplug_core::util::small_vec_enum_map::SmallVecEnumMap;
use futures_util::{SinkExt, StreamExt};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Instant;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// One `OutputCmd` received from the connected buttplug client, tagged with
/// the moment the fake server's read loop observed it.
#[derive(Debug, Clone)]
pub struct ReceivedCommand {
    pub device_index: u32,
    pub feature_index: u32,
    pub value: i32,
    pub at: Instant,
}

fn device_list_message(id: u32) -> ButtplugServerMessageV4 {
    let mut features = BTreeMap::new();
    features.insert(
        0u32,
        DeviceFeature::new(
            0,
            "Fake Vibrator",
            &SmallVecEnumMap::from_iter([DeviceFeatureOutput::Vibrate(
                DeviceFeatureOutputValueProperties::new(RangeInclusive::new(0, 100)),
            )]),
            &SmallVecEnumMap::default(),
        ),
    );
    let mut info = DeviceMessageInfoV4::new(0, "Fake Vibrator", &None, 0, &features);
    let mut msg = ButtplugServerMessageV4::DeviceList(DeviceListV4::new(vec![info.clone()]));
    let _ = &mut info; // info consumed by DeviceListV4::new via clone above
    if let ButtplugServerMessageV4::DeviceList(ref mut dl) = msg {
        dl.set_id(id);
    }
    msg
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    tx: mpsc::UnboundedSender<ReceivedCommand>,
) -> Result<()> {
    let mut ws = tokio_tungstenite::accept_async(stream)
        .await
        .context("websocket handshake failed")?;

    while let Some(msg) = ws.next().await {
        let msg = msg.context("websocket read error")?;
        let Message::Text(text) = msg else { continue };
        let incoming: Vec<ButtplugClientMessageV4> =
            serde_json::from_str(&text).context("failed to parse client message")?;

        for client_msg in incoming {
            let reply = match client_msg {
                ButtplugClientMessageV4::RequestServerInfo(req) => {
                    let mut info = ServerInfoV4::new(
                        "fake-buttplug-server",
                        buttplug_core::message::ButtplugMessageSpecVersion::Version4,
                        0,
                        0,
                    );
                    info.set_id(req.id());
                    ButtplugServerMessageV4::ServerInfo(info)
                }
                ButtplugClientMessageV4::RequestDeviceList(req) => device_list_message(req.id()),
                ButtplugClientMessageV4::OutputCmd(cmd) => {
                    let _ = tx.send(ReceivedCommand {
                        device_index: cmd.device_index(),
                        feature_index: cmd.feature_index(),
                        value: cmd.command().value(),
                        at: Instant::now(),
                    });
                    let mut ok = OkV0::default();
                    ok.set_id(cmd.id());
                    ButtplugServerMessageV4::Ok(ok)
                }
                other => {
                    log::warn!("fake buttplug server: unhandled client message {:?}", other);
                    continue;
                }
            };
            let text = serde_json::to_string(&[&reply])?;
            ws.send(Message::Text(text.into())).await?;
        }
    }
    Ok(())
}

/// Binds a loopback TCP listener, spawns the accept/handshake/command loop as
/// a background task, and returns the bound address plus a channel that
/// yields every `OutputCmd` the connected client sends.
pub async fn spawn_fake_server() -> Result<(SocketAddr, mpsc::UnboundedReceiver<ReceivedCommand>)> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind fake buttplug server")?;
    let addr = listener.local_addr()?;
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    log::warn!("fake buttplug server: accept failed: {}", e);
                    continue;
                }
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, tx).await {
                    log::warn!("fake buttplug server: connection ended: {}", e);
                }
            });
        }
    });

    Ok((addr, rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use buttplug_core::message::{
        ButtplugMessageSpecVersion, RequestDeviceListV0, RequestServerInfoV4,
    };
    use tokio_tungstenite::tungstenite::Message;

    #[tokio::test]
    async fn handshake_then_output_cmd_is_captured() {
        let (addr, mut rx) = spawn_fake_server().await.unwrap();
        let url = format!("ws://{}", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();

        let req = RequestServerInfoV4::new("test-client", ButtplugMessageSpecVersion::Version4, 0);
        ws.send(Message::Text(
            serde_json::to_string(&[&req]).unwrap().into(),
        ))
        .await
        .unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        assert!(reply.into_text().unwrap().contains("ServerInfo"));

        let req = RequestDeviceListV0::default();
        ws.send(Message::Text(
            serde_json::to_string(&[&req]).unwrap().into(),
        ))
        .await
        .unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        let reply_text = reply.into_text().unwrap();
        assert!(reply_text.contains("DeviceList"));
        assert!(reply_text.contains("Vibrate"));

        let cmd = OutputCmdV4::new(
            0,
            0,
            buttplug_core::message::OutputCommand::Vibrate(
                buttplug_core::message::OutputValue::new(42),
            ),
        );
        ws.send(Message::Text(
            serde_json::to_string(&[&cmd]).unwrap().into(),
        ))
        .await
        .unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        assert!(reply.into_text().unwrap().contains("Ok"));

        let received = rx.recv().await.unwrap();
        assert_eq!(received.device_index, 0);
        assert_eq!(received.feature_index, 0);
        assert_eq!(received.value, 42);
    }
}
```

- [ ] **Step 2: Run the test to confirm it compiles and passes**

Run: `cargo test -p linux-game-haptics-router-e2e handshake_then_output_cmd_is_captured -- --nocapture`
Expected: PASS. If it fails to compile due to a mismatched `buttplug_core` API (getter name, method signature), fix the call site to match the real signature — the exact struct/field/method names above were verified against the vendored `buttplug_core-10.0.3` source, but re-check `cargo doc -p buttplug_core --open` if anything doesn't line up.

- [ ] **Step 3: Remove the stray `let _ = &mut info` workaround**

The `device_list_message` function above has a redundant `info.clone()`/`let _ = &mut info` because `DeviceListV4::new` takes `Vec<DeviceMessageInfoV4>` by value. Simplify:

```rust
fn device_list_message(id: u32) -> ButtplugServerMessageV4 {
    let mut features = BTreeMap::new();
    features.insert(
        0u32,
        DeviceFeature::new(
            0,
            "Fake Vibrator",
            &SmallVecEnumMap::from_iter([DeviceFeatureOutput::Vibrate(
                DeviceFeatureOutputValueProperties::new(RangeInclusive::new(0, 100)),
            )]),
            &SmallVecEnumMap::default(),
        ),
    );
    let info = DeviceMessageInfoV4::new(0, "Fake Vibrator", &None, 0, &features);
    let mut msg = ButtplugServerMessageV4::DeviceList(DeviceListV4::new(vec![info]));
    if let ButtplugServerMessageV4::DeviceList(ref mut dl) = msg {
        dl.set_id(id);
    }
    msg
}
```

- [ ] **Step 4: Re-run the test**

Run: `cargo test -p linux-game-haptics-router-e2e handshake_then_output_cmd_is_captured`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add linux-game-haptics-router-e2e/src/protocol_server.rs
git commit -m "$(cat <<'EOF'
feat(e2e): implement in-process fake buttplug server

Speaks the real buttplug_core v4 wire messages (handshake, device list,
OutputCmd) over an actual websocket so the daemon's real buttplug client
connects to it unmodified. Captured commands are timestamped and pushed to
a channel for the timing assertions in later tasks.
EOF
)"
```

---

### Task 3: Timing assertion logic (pure, TDD)

**Files:**
- Modify: `linux-game-haptics-router-e2e/src/timing.rs`

**Interfaces:**
- Consumes: `linux_game_haptics_router_e2e::protocol_server::ReceivedCommand` (Task 2).
- Produces:
  - `pub const LATENCY_BOUND: std::time::Duration = std::time::Duration::from_millis(150);`
  - `pub struct TimingError(pub String)` implementing `std::fmt::Display`/`std::error::Error`.
  - `pub fn assert_command_within_bound(issued_at: std::time::Instant, commands: &[ReceivedCommand]) -> Result<&ReceivedCommand, TimingError>` — returns the first command received within `LATENCY_BOUND` of `issued_at`, or an error naming the actual latency if none qualifies.
  - `pub fn assert_final_zero_within_bound(expected_end: std::time::Instant, commands: &[ReceivedCommand]) -> Result<(), TimingError>` — takes the *last* command in `commands`, asserts its `value == 0` and its `at` is within `LATENCY_BOUND` of `expected_end`.
  - `pub fn expected_end_time(issued_at: std::time::Instant, replay_length_ms: u16) -> std::time::Instant`

- [ ] **Step 1: Write the failing tests**

Replace `linux-game-haptics-router-e2e/src/timing.rs`:

```rust
use crate::protocol_server::ReceivedCommand;
use std::fmt;
use std::time::{Duration, Instant};

pub const LATENCY_BOUND: Duration = Duration::from_millis(150);

#[derive(Debug)]
pub struct TimingError(pub String);

impl fmt::Display for TimingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for TimingError {}

pub fn expected_end_time(issued_at: Instant, replay_length_ms: u16) -> Instant {
    issued_at + Duration::from_millis(replay_length_ms as u64)
}

pub fn assert_command_within_bound(
    issued_at: Instant,
    commands: &[ReceivedCommand],
) -> Result<&ReceivedCommand, TimingError> {
    commands
        .iter()
        .find(|c| c.at.saturating_duration_since(issued_at) <= LATENCY_BOUND)
        .ok_or_else(|| {
            let actual = commands
                .first()
                .map(|c| format!("{:?}", c.at.saturating_duration_since(issued_at)))
                .unwrap_or_else(|| "no command received at all".to_string());
            TimingError(format!(
                "no command arrived within {:?} of issue (actual: {})",
                LATENCY_BOUND, actual
            ))
        })
}

pub fn assert_final_zero_within_bound(
    expected_end: Instant,
    commands: &[ReceivedCommand],
) -> Result<(), TimingError> {
    let last = commands
        .last()
        .ok_or_else(|| TimingError("no commands received; cannot check final stop".to_string()))?;
    if last.value != 0 {
        return Err(TimingError(format!(
            "final command value was {}, expected 0 (stop)",
            last.value
        )));
    }
    let delta = if last.at >= expected_end {
        last.at - expected_end
    } else {
        expected_end - last.at
    };
    if delta > LATENCY_BOUND {
        return Err(TimingError(format!(
            "final stop command arrived {:?} from expected end time (bound {:?})",
            delta, LATENCY_BOUND
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(value: i32, at: Instant) -> ReceivedCommand {
        ReceivedCommand {
            device_index: 0,
            feature_index: 0,
            value,
            at,
        }
    }

    #[test]
    fn command_within_bound_is_found() {
        let issued = Instant::now();
        let commands = vec![cmd(50, issued + Duration::from_millis(50))];
        assert!(assert_command_within_bound(issued, &commands).is_ok());
    }

    #[test]
    fn command_outside_bound_is_rejected() {
        let issued = Instant::now();
        let commands = vec![cmd(50, issued + Duration::from_millis(200))];
        let err = assert_command_within_bound(issued, &commands).unwrap_err();
        assert!(err.0.contains("150ms"));
    }

    #[test]
    fn no_commands_at_all_is_rejected() {
        let issued = Instant::now();
        let err = assert_command_within_bound(issued, &[]).unwrap_err();
        assert!(err.0.contains("no command received at all"));
    }

    #[test]
    fn final_zero_within_bound_passes() {
        let issued = Instant::now();
        let expected_end = expected_end_time(issued, 500);
        let commands = vec![
            cmd(80, issued + Duration::from_millis(10)),
            cmd(0, expected_end + Duration::from_millis(20)),
        ];
        assert!(assert_final_zero_within_bound(expected_end, &commands).is_ok());
    }

    #[test]
    fn final_command_nonzero_is_rejected() {
        let issued = Instant::now();
        let expected_end = expected_end_time(issued, 500);
        let commands = vec![cmd(80, expected_end)];
        let err = assert_final_zero_within_bound(expected_end, &commands).unwrap_err();
        assert!(err.0.contains("expected 0"));
    }

    #[test]
    fn final_zero_outside_bound_is_rejected() {
        let issued = Instant::now();
        let expected_end = expected_end_time(issued, 500);
        let commands = vec![cmd(0, expected_end + Duration::from_millis(300))];
        let err = assert_final_zero_within_bound(expected_end, &commands).unwrap_err();
        assert!(err.0.contains("bound"));
    }

    #[test]
    fn expected_end_time_adds_replay_length_ms() {
        let issued = Instant::now();
        let end = expected_end_time(issued, 500);
        assert_eq!(end - issued, Duration::from_millis(500));
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p linux-game-haptics-router-e2e timing::`
Expected: all 6 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add linux-game-haptics-router-e2e/src/timing.rs
git commit -m "$(cat <<'EOF'
feat(e2e): add timing assertion logic for the 150ms bounds

Pure functions checking that a command arrived within 150ms of a gesture
being issued, and that the final command is a zero-magnitude stop within
150ms of the effect's expected end time (issue time + replay length).
EOF
)"
```

---

### Task 4: Virtual FF gamepad (gamepad module)

**Files:**
- Modify: `linux-game-haptics-router-e2e/src/gamepad.rs`

**Interfaces:**
- Consumes: nothing from other e2e-crate modules.
- Produces:
  - `pub struct FakeGamepad { pub device_node: std::path::PathBuf }` — `device_node` is the `/dev/input/eventN` path a "game" process (the scenario runner in Task 5/6) opens directly with `evdev::Device::open` to call `upload_ff_effect`/`play`/`stop`.
  - `pub fn spawn_fake_gamepad(name: &str) -> anyhow::Result<FakeGamepad>` — creates the uinput device advertising `FF_RUMBLE`/`FF_PERIODIC`/`FF_CONSTANT`/`FF_RAMP`, spawns a background OS thread that services `UI_FF_UPLOAD`/`UI_FF_ERASE` requests (auto-acking every upload with `retval = 0` and a freshly allocated effect id), and returns once the resulting device node exists.

This module needs a real kernel with `/dev/uinput` writable (root or the right udev rule) — it cannot be exercised under plain `cargo test` in a sandboxed dev shell or CI's default build image. It is verified by actually running `e2e-tests` inside the VM (Task 6), not by an automated unit test here.

- [ ] **Step 1: Implement gamepad creation + the FF upload/erase ack loop**

Replace `linux-game-haptics-router-e2e/src/gamepad.rs`:

```rust
use anyhow::{Context, Result};
use evdev::uinput::{VirtualDeviceBuilder, UInputEventType};
use evdev::{AttributeSet, FFEffectType, InputEventKind};
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
    let mut device = VirtualDeviceBuilder::new()
        .context("opening /dev/uinput (needs root or a uinput udev rule)")?
        .name(name)
        .with_ff(&AttributeSet::from_iter([
            FFEffectType::FF_RUMBLE,
            FFEffectType::FF_PERIODIC,
            FFEffectType::FF_CONSTANT,
            FFEffectType::FF_RAMP,
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
                    let InputEventKind::UInput(code) = event.kind() else {
                        continue;
                    };
                    if code == UInputEventType::UI_FF_UPLOAD.0 {
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
                    } else if code == UInputEventType::UI_FF_ERASE.0 {
                        if let Ok(erase) = device.process_ff_erase(event) {
                            free_ids.insert(erase.effect_id() as i16);
                        }
                    }
                }
            }
        })
        .context("spawning FF ack thread")?;

    Ok(FakeGamepad { device_node })
}
```

- [ ] **Step 2: Confirm the crate still compiles**

Run: `cargo build -p linux-game-haptics-router-e2e`
Expected: builds successfully (this module has no `#[cfg(test)]` block — it's exercised for real in Task 6, not under `cargo test`).

- [ ] **Step 3: Commit**

```bash
git add linux-game-haptics-router-e2e/src/gamepad.rs
git commit -m "$(cat <<'EOF'
feat(e2e): add virtual FF gamepad creation via uinput

Wraps evdev's uinput support to create a gamepad advertising the FF kinds
the daemon translates, and services UI_FF_UPLOAD/UI_FF_ERASE requests so
scenario code issuing real EVIOCSFF uploads against the device node
completes successfully. Requires a real kernel + /dev/uinput access, so
only exercised by actually running e2e-tests (Task 6), not cargo test.
EOF
)"
```

---

### Task 5: Scenario definitions

**Files:**
- Modify: `linux-game-haptics-router-e2e/src/scenarios.rs`

**Interfaces:**
- Consumes: nothing (pure data + one pure helper); used by Task 6's orchestrator.
- Produces:
  - `pub struct Scenario { pub name: &'static str, pub effect: evdev::FFEffectData, pub expected_end_ms: u16 }`
  - `pub fn smoke_scenarios() -> Vec<Scenario>` — the 4 single-device scenarios from the spec (rumble, periodic, constant+envelope, rapid retrigger is handled separately in Task 6 since it issues two plays; multi-device is also handled in Task 6 since it needs two gamepads). This function returns the 3 scenarios that map 1:1 onto a single effect + single assertion pass.

- [ ] **Step 1: Write the failing test for scenario data sanity**

Replace `linux-game-haptics-router-e2e/src/scenarios.rs`:

```rust
use evdev::{FFEffectData, FFEffectKind, FFEnvelope, FFReplay, FFTrigger, FFWaveform};

pub struct Scenario {
    pub name: &'static str,
    pub effect: FFEffectData,
    pub expected_end_ms: u16,
}

/// The smoke-set scenarios that each issue exactly one gesture and check
/// exactly one timing pair. Rapid-retrigger and multi-device scenarios are
/// built directly in the e2e-tests orchestrator (Task 6) since they involve
/// more than one gamepad/gesture.
pub fn smoke_scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "ff_rumble",
            effect: FFEffectData {
                direction: 0,
                trigger: FFTrigger::default(),
                replay: FFReplay {
                    length: 300,
                    delay: 0,
                },
                kind: FFEffectKind::Rumble {
                    strong_magnitude: 0xffff,
                    weak_magnitude: 0xffff,
                },
            },
            expected_end_ms: 300,
        },
        Scenario {
            name: "ff_periodic_sine",
            effect: FFEffectData {
                direction: 0,
                trigger: FFTrigger::default(),
                replay: FFReplay {
                    length: 400,
                    delay: 0,
                },
                kind: FFEffectKind::Periodic {
                    waveform: FFWaveform::Sine,
                    period: 100,
                    magnitude: 0x7fff,
                    offset: 0,
                    phase: 0,
                    envelope: FFEnvelope {
                        attack_length: 0,
                        attack_level: 0,
                        fade_length: 0,
                        fade_level: 0,
                    },
                },
            },
            expected_end_ms: 400,
        },
        Scenario {
            name: "ff_constant_with_envelope",
            effect: FFEffectData {
                direction: 0,
                trigger: FFTrigger::default(),
                replay: FFReplay {
                    length: 500,
                    delay: 0,
                },
                kind: FFEffectKind::Constant {
                    level: 0x7fff,
                    envelope: FFEnvelope {
                        attack_length: 100,
                        attack_level: 0,
                        fade_length: 150,
                        fade_level: 0,
                    },
                },
            },
            expected_end_ms: 500,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_scenarios_has_three_distinct_named_cases() {
        let scenarios = smoke_scenarios();
        assert_eq!(scenarios.len(), 3);
        let names: Vec<_> = scenarios.iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["ff_rumble", "ff_periodic_sine", "ff_constant_with_envelope"]);
    }

    #[test]
    fn every_scenario_expected_end_matches_its_replay_length() {
        for s in smoke_scenarios() {
            assert_eq!(s.effect.replay.length, s.expected_end_ms);
        }
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p linux-game-haptics-router-e2e scenarios::`
Expected: both tests PASS.

- [ ] **Step 3: Commit**

```bash
git add linux-game-haptics-router-e2e/src/scenarios.rs
git commit -m "$(cat <<'EOF'
feat(e2e): define the smoke-set scenario data

Rumble, periodic-sine, and constant-with-envelope cases as FFEffectData +
expected end time, each one gesture -> one timing assertion pair. Rapid
retrigger and multi-device cases are built directly in the orchestrator
since they need more than one gamepad/gesture.
EOF
)"
```

---

### Task 6: e2e-tests orchestrator binary

**Files:**
- Modify: `linux-game-haptics-router-e2e/src/bin/e2e-tests.rs`

**Interfaces:**
- Consumes: `protocol_server::spawn_fake_server`, `gamepad::spawn_fake_gamepad`, `timing::{assert_command_within_bound, assert_final_zero_within_bound, expected_end_time}`, `scenarios::smoke_scenarios` (Tasks 2–5).
- Produces: the `e2e-tests` binary's behavior — takes `--daemon-path <path> --device-map-json <json>` (or simpler: hardcodes the relative path `./game-haptics-router` since `run.sh` scps it into the same directory `e2e-tests` runs from), prints a PASS/FAIL line per scenario, exits nonzero if any scenario failed.

This binary is integration-only: it needs a real kernel (`/dev/uinput`, evdev), and it spawns the real daemon binary as a subprocess. It cannot run under `cargo test`. It is verified by Task 7's `run.sh` actually executing it inside the VM — there is no automated test for this task; manual verification steps are given instead.

- [ ] **Step 1: Implement the orchestrator**

Replace `linux-game-haptics-router-e2e/src/bin/e2e-tests.rs`:

```rust
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
```

- [ ] **Step 2: Confirm it compiles**

Run: `cargo build -p linux-game-haptics-router-e2e --bin e2e-tests`
Expected: builds successfully.

- [ ] **Step 3: Manual verification (requires a Linux box with `/dev/uinput` writable and the real daemon binary built)**

This step cannot run in a plain dev sandbox — do it once Task 7's VM is available, or on a local Linux machine with root:

```bash
cargo build --release --workspace --exclude linux-game-haptics-router-ebpf
cp target/release/game-haptics-router linux-game-haptics-router-e2e/
sudo ./target/release/e2e-tests
```

Expected: `PASS ff_rumble`, `PASS ff_periodic_sine`, `PASS ff_constant_with_envelope`, then `all scenarios passed`, exit code 0.

- [ ] **Step 4: Commit**

```bash
git add linux-game-haptics-router-e2e/src/bin/e2e-tests.rs
git commit -m "$(cat <<'EOF'
feat(e2e): implement the e2e-tests orchestrator binary

Wires the fake buttplug server, virtual gamepad, and real daemon subprocess
together, runs the smoke-set scenarios, and asserts both timing bounds per
scenario. Integration-only — verified by actually running it (in the VM),
not by cargo test.
EOF
)"
```

---

### Task 7: Outer VM orchestration script

**Files:**
- Create: `e2e/run.sh`
- Create: `e2e/cloud-init/user-data.tmpl`
- Create: `e2e/cloud-init/meta-data`

**Interfaces:**
- Consumes: `linux-game-haptics-router-e2e/target/.../e2e-tests` and `linux-game-haptics-router/target/.../game-haptics-router` binaries (built by the script itself before boot).
- Produces: `e2e/run.sh`'s exit code — 0 if the in-VM `e2e-tests` run passed, nonzero (including 124 on timeout) otherwise. This is what CI (Task 8) checks.

- [ ] **Step 1: Write the cloud-init seed templates**

Create `e2e/cloud-init/meta-data`:

```yaml
instance-id: linux-game-haptics-router-e2e
local-hostname: e2e-vm
```

Create `e2e/cloud-init/user-data.tmpl` (`__SSH_PUBKEY__` is substituted by `run.sh`):

```yaml
#cloud-config
users:
  - name: e2e
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    ssh_authorized_keys:
      - __SSH_PUBKEY__
ssh_pwauth: false
```

- [ ] **Step 2: Write `run.sh`**

Create `e2e/run.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WORK_DIR="$(mktemp -d)"
ARCH="$(uname -m)"

case "$ARCH" in
    x86_64)  QEMU_BIN=qemu-system-x86_64; CLOUD_IMAGE_URL="https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img" ;;
    aarch64) QEMU_BIN=qemu-system-aarch64; CLOUD_IMAGE_URL="https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-arm64.img" ;;
    *) echo "unsupported host arch: $ARCH" >&2; exit 1 ;;
esac

CACHE_DIR="${E2E_CACHE_DIR:-$HOME/.cache/linux-game-haptics-router-e2e}"
mkdir -p "$CACHE_DIR"
BASE_IMAGE="$CACHE_DIR/base-$ARCH.img"

QEMU_PID=""
cleanup() {
    if [ -n "$QEMU_PID" ] && kill -0 "$QEMU_PID" 2>/dev/null; then
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

if [ ! -f "$BASE_IMAGE" ]; then
    echo "downloading base cloud image for $ARCH..."
    curl -fL -o "$BASE_IMAGE.tmp" "$CLOUD_IMAGE_URL"
    mv "$BASE_IMAGE.tmp" "$BASE_IMAGE"
fi

echo "building release binaries..."
(cd "$REPO_ROOT" && cargo build --release --workspace --exclude linux-game-haptics-router-ebpf)
(cd "$REPO_ROOT" && cargo build --release -p linux-game-haptics-router-e2e --bin e2e-tests)

SSH_KEY="$WORK_DIR/id_ed25519"
ssh-keygen -t ed25519 -N "" -f "$SSH_KEY" -q

OVERLAY="$WORK_DIR/overlay.qcow2"
qemu-img create -f qcow2 -F qcow2 -b "$BASE_IMAGE" "$OVERLAY" >/dev/null

sed "s#__SSH_PUBKEY__#$(cat "$SSH_KEY.pub")#" \
    "$SCRIPT_DIR/cloud-init/user-data.tmpl" > "$WORK_DIR/user-data"
cp "$SCRIPT_DIR/cloud-init/meta-data" "$WORK_DIR/meta-data"

SEED_ISO="$WORK_DIR/seed.iso"
if command -v cloud-localds >/dev/null; then
    cloud-localds "$SEED_ISO" "$WORK_DIR/user-data" "$WORK_DIR/meta-data"
else
    genisoimage -output "$SEED_ISO" -volid cidata -joliet -rock \
        "$WORK_DIR/user-data" "$WORK_DIR/meta-data"
fi

SSH_PORT=10222
"$QEMU_BIN" \
    -m 2048 -smp 2 -enable-kvm -nographic \
    -drive file="$OVERLAY",if=virtio,format=qcow2 \
    -drive file="$SEED_ISO",if=virtio,format=raw \
    -netdev user,id=net0,hostfwd=tcp::"$SSH_PORT"-:22 \
    -device virtio-net-pci,netdev=net0 \
    >"$WORK_DIR/qemu.log" 2>&1 &
QEMU_PID=$!

echo "waiting for SSH..."
SSH_OPTS=(-i "$SSH_KEY" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -p "$SSH_PORT")
for _ in $(seq 1 60); do
    if ssh "${SSH_OPTS[@]}" e2e@127.0.0.1 true 2>/dev/null; then
        break
    fi
    sleep 2
done
if ! ssh "${SSH_OPTS[@]}" e2e@127.0.0.1 true 2>/dev/null; then
    echo "VM never became SSH-reachable" >&2
    exit 1
fi

echo "copying binaries into VM..."
scp "${SSH_OPTS[@]}" \
    "$REPO_ROOT/target/release/game-haptics-router" \
    "$REPO_ROOT/target/release/e2e-tests" \
    e2e@127.0.0.1:~/

echo "running e2e-tests in VM..."
set +e
timeout 300 ssh "${SSH_OPTS[@]}" e2e@127.0.0.1 'sudo ./e2e-tests'
RESULT=$?
set -e

exit "$RESULT"
```

- [ ] **Step 3: Make it executable and shellcheck it**

```bash
chmod +x e2e/run.sh
shellcheck e2e/run.sh
```

Expected: `shellcheck` reports no errors (warnings about unused `$WORK_DIR` subpaths etc. are fine to leave; fix anything flagged as an actual bug, e.g. unquoted expansions).

- [ ] **Step 4: Manual verification (local Linux machine with QEMU + KVM)**

```bash
./e2e/run.sh
```

Expected: downloads the base image on first run, boots the VM, copies binaries, runs `e2e-tests`, prints the PASS lines from Task 6, exits 0. Re-run to confirm the cached base image is reused (no re-download) and the run is faster the second time.

- [ ] **Step 5: Commit**

```bash
git add e2e/run.sh e2e/cloud-init
git commit -m "$(cat <<'EOF'
feat(e2e): add outer VM orchestration script

Boots a QEMU VM matching the host arch from a cached cloud image, seeds it
via cloud-init, copies the built daemon + e2e-tests binaries in over scp,
and runs e2e-tests over SSH under a timeout, propagating its exit code.
EOF
)"
```

---

### Task 8: CI wiring

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `e2e/run.sh` (Task 7).
- Produces: a new `e2e` job in the existing CI workflow.

- [ ] **Step 1: Read the existing workflow structure**

Read `.github/workflows/ci.yml` in full to match its existing job style (it already uses `./.github/actions/rust-bpf-setup` for toolchain setup, per Task 1's Step 6 edits).

- [ ] **Step 2: Add the e2e job**

Add a new job to `.github/workflows/ci.yml`:

```yaml
  e2e:
    strategy:
      matrix:
        include:
          - runner: ubuntu-latest
          - runner: ubuntu-24.04-arm
    runs-on: ${{ matrix.runner }}
    timeout-minutes: 20
    steps:
      - uses: actions/checkout@v4
      - uses: ./.github/actions/rust-bpf-setup
      - run: sudo apt-get update && sudo apt-get install -y qemu-system cloud-image-utils
      - run: ./e2e/run.sh
```

- [ ] **Step 3: Verify the workflow YAML is well-formed**

Run: `python3 -c "import yaml, sys; yaml.safe_load(open('.github/workflows/ci.yml'))"`
Expected: no output (parses successfully). If `yaml` isn't available, `ruby -ryaml -e "YAML.load_file('.github/workflows/ci.yml')"` is an equivalent check.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "$(cat <<'EOF'
ci: run the e2e VM harness on x86_64 and aarch64 runners

Matrixes over ubuntu-latest and ubuntu-24.04-arm so the VM boots natively
KVM-accelerated on each arch, matching e2e/run.sh's host-arch-matching
behavior. 20 minute job timeout backstops run.sh's own SSH timeout.
EOF
)"
```

- [ ] **Step 5: Push the branch and confirm the new CI job runs**

```bash
git push -u origin test/e2e-vm-harness
```

Then check the Actions tab (or `gh run watch`) for the pushed branch and confirm the `e2e` job's both matrix legs start and reach the `./e2e/run.sh` step. Full green here depends on prior tasks' manual verification steps already having passed locally — if this is the first time the whole pipeline runs, treat any failure as ordinary debugging, not a plan defect: check the job logs, fix the specific broken step, recommit.

---

### Task 9: Documentation

**Files:**
- Modify: `CLAUDE.md`

**Interfaces:** none (docs only).

- [ ] **Step 1: Add an E2E section to CLAUDE.md**

Read `CLAUDE.md`'s `## Build / test` section, then add a new section after it:

```markdown
## End-to-end test

`e2e/run.sh` boots a QEMU VM matching the host arch, deploys the built
`game-haptics-router` daemon plus a test harness binary (`e2e-tests`, in the
`linux-game-haptics-router-e2e` crate) into it, and runs a smoke-set of FF
gestures against a virtual gamepad (via `evdev`'s uinput support) and an
in-process fake buttplug server, asserting two 150ms timing bounds: command
dispatch latency, and final zero-magnitude ("stop") command latency
relative to the gesture's expected end time.

```bash
./e2e/run.sh
```

Requires QEMU with KVM and `cloud-image-utils` (for `cloud-localds`) on the
host. `linux-game-haptics-router-e2e` is excluded from the default
`cargo build/test --workspace` the same way the ebpf crate is — it needs
`/dev/uinput` and root, which a plain dev shell doesn't have.
```

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "$(cat <<'EOF'
docs: document the e2e VM test harness

EOF
)"
```

---

## Self-Review Notes

- **Spec coverage:** every section of the design spec maps to a task — outer script (Task 7), fake buttplug server (Task 2), virtual gamepad (Task 4), scenarios (Task 5), timing model (Task 3), orchestrator (Task 6), CI wiring (Task 8), error handling/teardown (folded into Task 7's `trap`/timeout), docs (Task 9).
- **Deviation from spec, deliberate:** the spec speculated about "reusing `translate.rs` timing logic... where feasible" for the expected-end-time calculation. Investigation during planning showed `translate.rs`'s envelope math only affects intensity, never timing — every effect kind's translation always emits its trailing zero-intensity point at `dt_ms == replay_length`. `translate.rs` also isn't part of a library target (it's a private module inside the `game-haptics-router` binary crate), so importing it from the e2e crate isn't possible without adding a lib target to the daemon crate purely for this. Task 3 instead computes `expected_end_time` directly from `issued_at + replay_length_ms`, which is exact, not an approximation, and needs no cross-crate dependency change.
- **Placeholder scan:** no TBD/TODO markers; every step has literal, complete code.
- **Type consistency:** `ReceivedCommand` (Task 2) is consumed identically in Task 3's signatures and Task 6's orchestrator; `Scenario` (Task 5) fields (`name`, `effect`, `expected_end_ms`) match their use in Task 6.
