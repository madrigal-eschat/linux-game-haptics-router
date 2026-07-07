# linux-game-haptics-router

[![Release](https://img.shields.io/github/v/release/madrigal-eschat/linux-game-haptics-router)](https://github.com/madrigal-eschat/linux-game-haptics-router/releases/latest)
[![CI](https://github.com/madrigal-eschat/linux-game-haptics-router/actions/workflows/ci.yml/badge.svg)](https://github.com/madrigal-eschat/linux-game-haptics-router/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/madrigal-eschat/linux-game-haptics-router/branch/main/graph/badge.svg)](https://codecov.io/gh/madrigal-eschat/linux-game-haptics-router)

Routes Linux game force-feedback (rumble) into buttplug/Intiface toy vibration
commands. An eBPF probe watches games writing FF effects to
`/dev/input/eventN` via the `EVIOCSFF` ioctl, translates whatever they send
(rumble/periodic/constant/ramp) into a vibration schedule, and streams it to
a running Intiface/buttplug server.

## Install

Grab the tarball for your architecture from the
[latest release](https://github.com/madrigal-eschat/linux-game-haptics-router/releases/latest)
and drop the binary in `/usr/local/bin`:

```bash
# amd64
curl -LO https://github.com/madrigal-eschat/linux-game-haptics-router/releases/latest/download/game-haptics-router-<tag>-linux-amd64.tar.gz
# or aarch64
curl -LO https://github.com/madrigal-eschat/linux-game-haptics-router/releases/latest/download/game-haptics-router-<tag>-linux-aarch64.tar.gz

tar xzf game-haptics-router-<tag>-linux-*.tar.gz
sudo install -m 755 game-haptics-router-<tag>-linux-*/game-haptics-router /usr/local/bin/game-haptics-router
```

Replace `<tag>` with the actual release tag (e.g. `v1.2.0`) shown on the
releases page — GitHub doesn't support a version-agnostic asset URL.

## Usage

Loading the eBPF probe and reading raw evdev nodes both need root. Start
[Intiface Central](https://intiface.com/central/) (or another buttplug
server) first, then:

```bash
sudo game-haptics-router --ws-url ws://127.0.0.1:12345 --scale 0.8
```

`ws://127.0.0.1:12345` is Intiface Central's default WebSocket address.

Other flags:

```bash
sudo game-haptics-router --list-devices   # print FF-capable evdev devices as JSON, then exit

sudo game-haptics-router --ws-url ws://127.0.0.1:12345 \
  --scale 1.0 \
  --device-map '{"usb-0000:00:14.0-1/input0": [0, 1]}'   # route one evdev device to specific toy indices; omit or use null to broadcast to every connected toy
```

While running, the global scale can be updated live by writing a JSON line
to stdin:

```bash
echo '{"scale": 0.5}' | sudo tee /proc/$(pgrep game-haptics-router)/fd/0
```

(or just pipe it in directly if you're supervising the process yourself).

## Caveats

- Only detects **evdev** force-feedback devices — anything a game drives
  through a different haptics path (e.g. DualSense adaptive triggers/haptics
  over its non-evdev HID report, or a game talking to a toy/engine directly)
  is invisible to this tool.
- Must be started **before** the game launches. The eBPF probe only sees
  `EVIOCSFF` calls that happen after it attaches — effects uploaded to a
  device before `game-haptics-router` is running are missed until the game
  re-uploads them (e.g. on a restart).
- No per-game or per-controller filtering yet — every FF-capable evdev device
  found on the system gets routed, and all captured effects across every
  process are translated and played. If you have multiple FF devices or games
  running at once, they'll all drive your toys.

## Project setup & build

Workspace of three crates: `linux-game-haptics-router` (userspace daemon, built as `game-haptics-router`),
`linux-game-haptics-router-ebpf` (the eBPF program), `linux-game-haptics-router-common` (shared
types). See [CLAUDE.md](CLAUDE.md) for the full architecture breakdown.

Building the eBPF program needs a nightly toolchain (for `aya-build`) plus
`bpf-linker`, alongside the stable toolchain used for the rest of the
workspace. `--exclude linux-game-haptics-router-ebpf` is required on every
command below: that crate is `#![no_std]#![no_main]` with its own
`#[panic_handler]` and can only compile through aya-build's cross-compile
(invoked from `linux-game-haptics-router`'s `build.rs`, which still runs and
produces the real embedded bytecode regardless of the exclude) — building it
directly as a normal workspace member links std and collides with its
`#[panic_handler]`.

```bash
rustup toolchain install nightly --component rust-src
cargo install bpf-linker

cargo build --workspace --exclude linux-game-haptics-router-ebpf          # debug build
cargo build --workspace --exclude linux-game-haptics-router-ebpf --release
```

Run the test suite:

```bash
cargo test --workspace --exclude linux-game-haptics-router-ebpf
```

Cross-compiling for aarch64 additionally needs the target and a cross linker:

```bash
rustup target add aarch64-unknown-linux-gnu
sudo apt install gcc-aarch64-linux-gnu   # Debian/Ubuntu
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
  cargo build --workspace --exclude linux-game-haptics-router-ebpf --release --target aarch64-unknown-linux-gnu
```

Running the built binary (whether via `cargo run` or the installed binary)
needs root, same as [Usage](#usage) above.

## End-to-end tests

`e2e/run.sh` boots a QEMU VM and runs the full pipeline (real eBPF probe,
real daemon, a virtual FF gamepad, and an in-process fake buttplug server)
against a handful of gesture scenarios. See [CLAUDE.md](CLAUDE.md#end-to-end-test)
for details.

**Known flaky failure:** the `ff_rumble` scenario — specifically only the
very first gesture issued after the daemon starts — intermittently fails
with "no matching effect in store" on the daemon side. Root cause: the
daemon learns about an uploaded FF effect via two independent, unsynchronized
paths — the eBPF ring buffer (upload notification: kernel ring buffer →
epoll → tokio `AsyncFd` → channel → app select loop) and the evdev reader
(play notification: a blocking `fetch_events()` read in its own OS thread,
which wakes on data almost immediately). The evdev path is structurally
faster. Every scenario after the first has enough idle time beforehand
(the previous scenario's wind-down) that the ring-buffer path catches up
before the next gesture's play write arrives; the very first gesture is
issued immediately after daemon startup with no such gap, so it can lose
that race. A fix needs the daemon itself to either add deliberate slop
between the two paths or unify effect-upload and play detection onto a
single ordering-guaranteed path (e.g. observing both via eBPF). Not yet
fixed — tracked as a known issue in the e2e suite rather than blocking
this branch on it.
