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
curl -LO https://github.com/madrigal-eschat/linux-game-haptics-router/releases/latest/download/haptics-probe-<tag>-linux-amd64.tar.gz
# or aarch64
curl -LO https://github.com/madrigal-eschat/linux-game-haptics-router/releases/latest/download/haptics-probe-<tag>-linux-aarch64.tar.gz

tar xzf haptics-probe-<tag>-linux-*.tar.gz
sudo install -m 755 haptics-probe-<tag>-linux-*/haptics-probe /usr/local/bin/haptics-probe
```

Replace `<tag>` with the actual release tag (e.g. `v1.2.0`) shown on the
releases page — GitHub doesn't support a version-agnostic asset URL.

## Usage

Loading the eBPF probe and reading raw evdev nodes both need root. Start
[Intiface Central](https://intiface.com/central/) (or another buttplug
server) first, then:

```bash
sudo haptics-probe --ws-url ws://127.0.0.1:12345 --scale 0.8
```

`ws://127.0.0.1:12345` is Intiface Central's default WebSocket address.

Other flags:

```bash
sudo haptics-probe --list-devices   # print FF-capable evdev devices as JSON, then exit

sudo haptics-probe --ws-url ws://127.0.0.1:12345 \
  --scale 1.0 \
  --device-map '{"usb-0000:00:14.0-1/input0": [0, 1]}'   # route one evdev device to specific toy indices; omit or use null to broadcast to every connected toy
```

While running, the global scale can be updated live by writing a JSON line
to stdin:

```bash
echo '{"scale": 0.5}' | sudo tee /proc/$(pgrep haptics-probe)/fd/0
```

(or just pipe it in directly if you're supervising the process yourself).

## Caveats

- Only detects **evdev** force-feedback devices — anything a game drives
  through a different haptics path (e.g. DualSense adaptive triggers/haptics
  over its non-evdev HID report, or a game talking to a toy/engine directly)
  is invisible to this tool.
- Must be started **before** the game launches. The eBPF probe only sees
  `EVIOCSFF` calls that happen after it attaches — effects uploaded to a
  device before `haptics-probe` is running are missed until the game
  re-uploads them (e.g. on a restart).
- No per-game or per-controller filtering yet — every FF-capable evdev device
  found on the system gets routed, and all captured effects across every
  process are translated and played. If you have multiple FF devices or games
  running at once, they'll all drive your toys.

## Project setup & build

Workspace of three crates: `haptics-probe` (userspace daemon),
`haptics-probe-ebpf` (the eBPF program), `haptics-probe-common` (shared
types). See [CLAUDE.md](CLAUDE.md) for the full architecture breakdown.

Building the eBPF program needs a nightly toolchain (for `aya-build`) plus
`bpf-linker`, alongside the stable toolchain used for the rest of the
workspace:

```bash
rustup toolchain install nightly --component rust-src
cargo install bpf-linker

cargo build --workspace          # debug build
cargo build --workspace --release
```

Run the test suite:

```bash
cargo test --workspace
```

Cross-compiling for aarch64 additionally needs the target and a cross linker:

```bash
rustup target add aarch64-unknown-linux-gnu
sudo apt install gcc-aarch64-linux-gnu   # Debian/Ubuntu
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
  cargo build --workspace --release --target aarch64-unknown-linux-gnu
```

Running the built binary (whether via `cargo run` or the installed binary)
needs root, same as [Usage](#usage) above.
