# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Linux daemon that routes game force-feedback (FF) rumble into buttplug/Intiface
sex-toy vibration commands. An eBPF probe hooks `EVIOCSFF` (the ioctl games use
to upload FF effects to `/dev/input/eventN`) and watches for the FF play/stop
writes on those same devices via evdev, then translates the effect's kernel
parameters into a vibration schedule and streams it to a buttplug server over
websocket.

Single Cargo workspace, three crates:

- `haptics-probe-common` — `#![no_std]`-compatible shared types (`FfEffect`,
  `ProbeEvent`, `Waveform`, `Envelope`, FF_* constants, `eviocsff_nr()`). Built
  twice: natively for userspace (`user` feature, pulls in `bson`/enables
  size-48 assertions for the host arch) and cross-compiled `no_std` into the
  eBPF program.
- `haptics-probe-ebpf` — the actual eBPF program (`aya-ebpf`), attaches
  `sys_enter_ioctl`/`sys_exit_ioctl` tracepoints, matches `EVIOCSFF` calls,
  captures the effect struct from userspace memory, and pushes a
  `ProbeEvent` onto a ring buffer map once the kernel assigns the effect id
  (on the syscall's exit, not enter).
- `haptics-probe` — the userspace binary. Loads/attaches the eBPF program
  (`ebpf.rs`), enumerates FF-capable evdev devices (`device.rs`), maintains an
  effect table + throttle + evdev reader tasks (`app.rs`, `throttle.rs`),
  translates `FfEffect` → intensity/time points per FF type (`translate.rs`),
  and owns the buttplug client connection plus per-device playback scheduling
  (`playback.rs`).

## Build / test

Requires a Rust toolchain with the eBPF cross-compile target set up for
`aya-build`/`aya-ebpf` (bpf-linker etc. per the `aya` project's own
requirements) — building `haptics-probe` compiles `haptics-probe-ebpf` as part
of its `build.rs`.

```bash
cargo build --workspace
cargo test --workspace          # only haptics-probe/-common have tests
cargo test -p haptics-probe translate::tests::   # scope to one module
cargo test -p haptics-probe some_test_name       # single test
```

Running the daemon needs root (eBPF load/attach + raw evdev access):

```bash
sudo ./target/debug/haptics-probe --list-devices
sudo ./target/debug/haptics-probe --ws-url ws://127.0.0.1:12345 --scale 0.8 \
  --device-map '{"usb-0000:00:14.0-1/input0": [0,1]}'
```

## Data flow / architecture notes

1. **eBPF side** (`haptics-probe-ebpf/src/main.rs`): on `sys_enter_ioctl`,
   compares the ioctl `cmd` against `eviocsff_nr()` (computed at compile time
   from the kernel's real `struct ff_effect` size — **48 bytes**, not
   `size_of::<FfEffect>()`, see `KERNEL_FF_EFFECT_SIZE` in
   `haptics-probe-common/src/lib.rs`). Reads the raw kernel struct out of
   userspace memory at its real field offsets (not a memcpy onto `FfEffect`,
   whose layout differs) and stashes it in `ENTER_SCRATCH` keyed by
   `tgid<<32|pid`. On `sys_exit_ioctl`, reads back the effect id the kernel
   just assigned (the whole reason capture is split across enter/exit), stores
   the completed `FfEffect` in `EFFECT_STORE`, and submits a `ProbeEvent` to
   the `EVENTS` ring buffer. Both maps are `LruHashMap`s specifically because
   nothing ever tells the probe when a process exits or an effect is freed via
   `EVIOCRMFF` — plain hashmaps would leak forever.
2. **Loading** (`haptics-probe/src/ebpf.rs`): attaches both tracepoints, then
   polls the ring buffer via `AsyncFd` (event-driven on the map fd's EPOLLIN,
   not a busy/sleep loop) and forwards `EffectUploaded` events to `App`.
3. **App** (`app.rs`) keeps its own userspace `effect_store: HashMap<(tgid,
   effect_id), FfEffect>` (separate from the eBPF map) plus a per-device evdev
   reader spawned per FF-capable device found by `device::list_ff_devices()`.
   A periodic rescan (every 5s) picks up devices that appear after startup or
   reappear at a new `/dev/input` path after reconnecting. Effect ids are only
   unique per-tgid at any instant — the kernel reuses them — so
   `upsert_effect`/`purge_effect_id` evict any other tgid's entry for the same
   numeric id before/when acting on it.
4. **Play/Stop events** read off evdev (`device::next_ff_event`) look up the
   matching effect, run it through `translate::translate()` to get a list of
   `HapticPoint { dt_ms, intensity }`, and hand it to `Playback`.
   `Throttle` (min 10ms between emitted haptics) only gates *Play*, never
   *Stop*.
5. **Translate** (`translate.rs`) implements FF_RUMBLE/FF_PERIODIC/FF_CONSTANT/
   FF_RAMP; unknown kinds produce no points. `apply_envelope` handles
   attack/fade ramps; `sample_waveform` handles the periodic waveforms
   (sine/square/triangle/saw). Samples are taken every `SAMPLE_INTERVAL_MS`
   (25ms).
6. **Playback** (`playback.rs`) owns the `ButtplugClient` and one task per
   `device_id` in `tasks: HashMap<String, (generation, JoinHandle)>`. Every
   new `schedule_sequence` call bumps a generation counter and aborts the
   previous task for that device — the generation lets a finishing task tell
   whether it's still the current occupant before deleting its own bookkeeping
   (so a superseding retrigger's state is never clobbered by the task it
   replaced). `interpolate_points` fills gaps between the sparse boundary
   points with linear interpolation every `STEP_MS` (25ms) so ramps read
   smoothly to the toy. `device_map` (evdev device_id → buttplug device
   indices, or `None`/missing = broadcast) is fixed at startup via
   `--device-map`; only `scale` can be changed live.
7. **Live control**: `main.rs` spawns a task reading JSON lines off stdin
   (`{"scale": 0.8}`) to update `Playback`'s scale at runtime — this is the
   only supported runtime control channel. An external supervisor process
   (referenced in comments as "Python", not part of this repo) is expected to
   own startup config (ws url, scale, device map) and push scale updates this
   way.

## Gotchas

- The `KERNEL_FF_EFFECT_SIZE = 48` constant and the raw byte offsets read in
  `try_enter` are only verified for x86_64/aarch64 LP64; both are guarded by
  `compile_error!` on other targets — don't add new targets without
  re-deriving the kernel's real `struct ff_effect` layout first.
- `FfEffect` (our capture struct) and the kernel's `struct ff_effect` are
  *not* the same layout — never assume you can byte-copy one onto the other.
- eBPF program has no allocator/panic unwinding (`#![no_std]`, spin-loop
  panic handler) — keep `haptics-probe-ebpf` code free of anything requiring
  `alloc` or unwinding.