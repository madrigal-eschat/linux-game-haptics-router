# E2E VM test harness design

## Purpose

Prove the full pipeline works end-to-end on a real kernel: eBPF probe →
userspace daemon → buttplug client, driven by real evdev/FF ioctls instead of
unit-level fakes. Assert two timing properties that unit tests can't cover:

- a buttplug command is sent within **150ms** of the FF gesture being issued
- the final zero-magnitude ("stop") command is sent within **150ms** of the
  gesture's expected end time (start + duration + envelope fade-out)

## Scope

Two layers:

- **Outer layer** (host/CI): boots a VM, deploys built binaries into it,
  runs the in-VM test harness over SSH, collects pass/fail, tears down.
- **Inner layer** (in-VM): a single Rust test-harness binary that creates a
  fake FF-capable gamepad, runs a fake buttplug server in-process, launches
  the real daemon under test, issues gestures, and asserts on timing.

Out of scope: exhaustive waveform/kind matrix (unit tests in `translate.rs`
already cover that math). This harness proves pipeline wiring + timing, not
translate-layer correctness.

## Architecture

```
CI/dev host
  e2e/run.sh
    1. cargo build --release --workspace --exclude linux-game-haptics-router-ebpf
       (ebpf bytecode still produced via build.rs as normal)
    2. fetch/cache a minimal cloud image (e.g. Ubuntu 24.04 cloud img), keep
       it read-only; boot from a throwaway qcow2 overlay on top of it
    3. write a cloud-init seed (SSH pubkey, passwordless sudo for test user)
    4. boot: qemu-system-$(uname -m) -enable-kvm ... (background) — VM arch
       always matches host arch (no cross-arch emulation), so this only ever
       runs KVM-accelerated. trap kills it and cleans temp files on any exit
       path
    5. poll SSH until reachable (bounded retry, overall timeout)
    6. scp the daemon binary + e2e-tests binary into the VM
    7. timeout <N> ssh -t vmuser 'sudo ./e2e-tests' — capture stdout,
       exit code (timeout's 124 counts as failure)
    8. propagate that exit code as run.sh's own exit code
```

Inside the VM, `e2e-tests`:

1. spawns the fake buttplug server as an in-process tokio task — a real
   `ws://127.0.0.1:<port>` listener speaking the subset of the buttplug
   protocol the daemon actually sends (connect handshake + whatever scalar
   command type `playback.rs` emits). Every received command is pushed,
   with an `Instant` timestamp, onto an mpsc channel the assertion logic
   reads.
2. creates a virtual FF-capable gamepad via the `uinput`/`evdev` crate
   (capabilities: `EV_FF` with RUMBLE/PERIODIC/CONSTANT/RAMP bits set),
   keeps the fd open for the rest of the run.
3. spawns the real `game-haptics-router` daemon as a subprocess, pointed at
   the fake ws address and the created device; captures its stdout/stderr
   for dumping on failure.
4. runs each scenario:
   - issues the gesture directly on the held fd: `EVIOCSFF` ioctl upload,
     then `EV_FF` play write; records `Instant::now()` at that point as
     issue-time
   - asserts a matching command arrives on the channel within 150ms of
     issue-time
   - computes expected end time = issue-time + effect duration + envelope
     fade-out (reusing `translate.rs` timing logic/constants rather than
     re-deriving it in the test crate, where feasible)
   - asserts the final zero-magnitude command's timestamp is within 150ms
     of that expected end time
   - failure messages include actual measured latency, not just pass/fail
5. teardown: kill the daemon subprocess (always — success or failure, so a
   leftover process never blocks a repeat local run), print a pass/fail
   summary per scenario, exit with nonzero if any scenario failed.

All timestamps are taken by the single in-VM process — no cross-host clock
skew to account for.

## Components

- **`linux-game-haptics-router-e2e`** — new workspace crate, host-target
  only (no cross-compile), excluded from the default
  `cargo test --workspace` run the same way the ebpf crate is excluded from
  normal builds/tests. Contains a single binary, `bin/e2e-tests.rs`, doing
  everything described above. No separate fake-controller binary and no
  subprocess/IPC for the fake buttplug server — both live in-process inside
  `e2e-tests`.
- **`e2e/run.sh`** — outer orchestration script (bash), not part of the
  cargo workspace. Owns VM lifecycle, artifact copy, SSH invocation +
  timeout, teardown via `trap`.
- **`e2e/cloud-init/`** — user-data/meta-data templates for the VM seed
  (SSH key injection, passwordless sudo for the test user).

## Scenarios (initial smoke set)

1. `FF_RUMBLE` — basic strong/weak magnitude gesture.
2. `FF_PERIODIC` (sine) — exercises waveform sampling path.
3. `FF_CONSTANT` with envelope (attack/fade) — exercises envelope ramp and
   end-time calculation (fade-out affects expected stop time).
4. Rapid retrigger — two plays issued within the daemon's 10ms throttle
   window; assert the play that does get through still lands its buttplug
   command within the 150ms bound.
5. Multi-device — two virtual gamepads with distinct `device_map` entries;
   assert no cross-talk (a gesture on device A never produces a command
   attributed to device B's mapping).

## CI wiring

- New job in `.github/workflows/ci.yml`, matrixed over host arch so the VM
  arch always matches the runner arch (no cross-arch/TCG emulation):
  - `runs-on: ubuntu-latest` (x86_64)
  - `runs-on: ubuntu-24.04-arm` (aarch64)
  Both runner families expose `/dev/kvm` for nested virtualization on
  current GitHub-hosted images — confirm this still holds at implementation
  time, particularly for the arm runner. `run.sh` picks the matching cloud
  image + qemu binary (`qemu-system-x86_64` / `qemu-system-aarch64`) based
  on `uname -m`, mirroring the `target` matrix already in
  `build-release.yml`.
  Each matrix leg builds its own release binaries via existing build steps,
  then runs `e2e/run.sh`, with a job-level `timeout-minutes` as a second
  backstop above the in-script SSH timeout.
- Local dev: identical `e2e/run.sh` invocation on whatever arch the
  developer is on, no CI-specific branching beyond where the base cloud
  image is cached.

## Error handling / teardown

- `run.sh` traps on exit (success, failure, or signal) to kill the QEMU
  process and remove the qcow2 overlay + cloud-init seed ISO. The cached
  base image itself is never mutated (overlay-based boot), so no
  re-download is needed between runs.
- SSH readiness wait is a bounded retry loop with an overall timeout before
  declaring boot failure (distinct from the later test-run timeout).
- The in-VM daemon subprocess's stdout/stderr are captured by `e2e-tests`
  and dumped on any scenario assertion failure, to aid debugging without
  needing a second SSH session.

## Testing

This *is* the test suite — no meta-tests planned beyond the scenarios
above. `e2e-tests` itself should fail loudly (panic/nonzero exit) on setup
problems (e.g. uinput device creation failing, daemon subprocess exiting
early) rather than silently skipping scenarios.
