# FF Play/Upload Race Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix a real daemon correctness gap where a Play event arriving before its effect's upload has been processed is silently dropped, by holding it (event-driven, no timeout) until the matching upload arrives — plus add `EVIOCRMFF` (erase) support to the eBPF probe so a pending play can be correctly dropped if its effect gets erased first.

**Architecture:** A new `kind` discriminant on the shared `ProbeEvent` wire struct lets the eBPF probe emit both "uploaded" and "erased" events over the same ring buffer. `app.rs` gains a `pending_plays: HashMap<device_id, PendingPlay>` (at most one entry per device) resolved by three independent triggers — a matching upload, a Stop for that device, or an erase of that effect id — each logging the elapsed wait. No polling, no fixed delay anywhere.

**Tech Stack:** Rust, `aya`/`aya-ebpf` (eBPF), tokio.

## Global Constraints

- **Build/test environment limitation (this plan's authoring machine is macOS):** `linux-game-haptics-router-ebpf` can only be compiled via `aya-build`'s cross-compile, invoked from `linux-game-haptics-router`'s `build.rs` — which runs, and is required, for *any* `cargo build`/`cargo test`/`cargo check` of the `linux-game-haptics-router` crate, even with `--exclude linux-game-haptics-router-ebpf`. On a macOS dev machine without a working `bpf-linker` cross-compile setup, this means **no task touching `linux-game-haptics-router` (app.rs, main.rs, ebpf.rs) or `linux-game-haptics-router-ebpf` can be locally compiled or tested** — verification for those tasks is CI-only (push the branch, check the `check (x86_64-unknown-linux-gnu)` / `test` / `clippy` GitHub Actions jobs, which run on Linux runners with the full toolchain). Only `linux-game-haptics-router-common` (with `--features user`) can be built/tested locally on macOS.
- No timeout/polling anywhere in the pending-play resolution — purely event-driven per the approved design.
- At most one pending play per device (`HashMap<String, PendingPlay>` naturally enforces this — a new insert for the same key overwrites the old one).
- Elapsed-delay logging on all three resolution outcomes (resolved by upload, cleared by stop, cleared by erase).
- E2E validation (re-running `e2e/run.sh`, removing the known-issue caveat once confirmed fixed) is in scope and required before this branch is considered done.

---

### Task 1: Common crate — wire format changes

**Files:**
- Modify: `linux-game-haptics-router-common/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub const PROBE_EVENT_KIND_UPLOADED: u8 = 0;`
  - `pub const PROBE_EVENT_KIND_ERASED: u8 = 1;`
  - `pub const fn eviocrmff_nr() -> u32`
  - `ProbeEvent` gains a `pub kind: u8` field (first field in the struct)
  - `FfEffect` gains `Default` (all-zero)

- [ ] **Step 1: Add `FfEffect`'s `Default` derive**

In `linux-game-haptics-router-common/src/lib.rs`, change:

```rust
/// Captured effect data — stored in eBPF map, read by userspace
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct FfEffect {
```

to:

```rust
/// Captured effect data — stored in eBPF map, read by userspace
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct FfEffect {
```

- [ ] **Step 2: Write the failing tests for the new constants/function**

Add to the `#[cfg(test)] mod tests` block at the bottom of the file:

```rust
    #[test]
    fn ff_effect_default_is_zeroed() {
        let e = FfEffect::default();
        assert_eq!(e.kind, 0);
        assert_eq!(e.id, 0);
        assert_eq!(e.direction, 0);
        assert_eq!(e.trigger_button, 0);
        assert_eq!(e.trigger_interval, 0);
        assert_eq!(e.replay_length, 0);
        assert_eq!(e.replay_delay, 0);
        assert_eq!(e.u, [0u16; 7]);
    }

    // Derived from the kernel uapi macro `#define EVIOCRMFF _IOW('E', 0x81, int)`
    // rather than a live strace (unlike eviocsff_nr_matches_strace_verified_value,
    // which was strace-verified against a real game) — re-derive against a live
    // strace of an EVIOCRMFF call if this ever needs re-verifying.
    #[test]
    fn eviocrmff_nr_matches_the_ioc_write_e_0x81_int_macro_definition() {
        assert_eq!(eviocrmff_nr(), 0x4004_4581);
    }

    #[test]
    fn probe_event_kind_constants_are_distinct() {
        assert_ne!(PROBE_EVENT_KIND_UPLOADED, PROBE_EVENT_KIND_ERASED);
    }
```

- [ ] **Step 3: Run the tests to confirm they fail to compile**

Run: `cargo test -p linux-game-haptics-router-common --features user`
Expected: FAIL — `eviocrmff_nr`, `PROBE_EVENT_KIND_UPLOADED`, `PROBE_EVENT_KIND_ERASED` not found.

- [ ] **Step 4: Add the constants, the ioctl-number function, and the `ProbeEvent.kind` field**

Directly above the existing `ProbeEvent` struct definition, add:

```rust
/// `ProbeEvent.kind` discriminant: this event is a freshly-uploaded effect
/// (the existing, original event shape — `effect` is meaningful).
pub const PROBE_EVENT_KIND_UPLOADED: u8 = 0;
/// `ProbeEvent.kind` discriminant: this event is an erased effect (freed via
/// `EVIOCRMFF`) — `effect` is unused/zeroed, only `tgid`/`effect_id` matter.
pub const PROBE_EVENT_KIND_ERASED: u8 = 1;
```

Change the `ProbeEvent` struct itself from:

```rust
/// Event emitted from eBPF ring buffer to userspace
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ProbeEvent {
    /// Process group ID of the process that uploaded the effect
    pub tgid: u32,
    /// Assigned effect id
    pub effect_id: i16,
    pub _pad: u16,
    pub effect: FfEffect,
}
```

to:

```rust
/// Event emitted from eBPF ring buffer to userspace. `kind` distinguishes
/// an upload (`PROBE_EVENT_KIND_UPLOADED`, `effect` meaningful) from an
/// erase (`PROBE_EVENT_KIND_ERASED`, `effect` zeroed/unused).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ProbeEvent {
    pub kind: u8,
    /// Process group ID of the process that uploaded (or erased) the effect
    pub tgid: u32,
    /// Assigned effect id
    pub effect_id: i16,
    pub _pad: u16,
    pub effect: FfEffect,
}
```

Directly below the existing `eviocsff_nr()` function, add:

```rust
/// Compute EVIOCRMFF ioctl number at compile time.
/// #define EVIOCRMFF _IOW('E', 0x81, int)
/// = (1<<30) | (size_of::<i32>()<<16) | ('E'<<8) | 0x81
pub const fn eviocrmff_nr() -> u32 {
    (1u32 << 30) | ((4u32 & 0x3fff) << 16) | (0x45u32 << 8) | 0x81u32
}
```

- [ ] **Step 5: Run the tests to confirm they pass**

Run: `cargo test -p linux-game-haptics-router-common --features user`
Expected: PASS, all tests including the 3 new ones.

- [ ] **Step 6: Commit**

```bash
git add linux-game-haptics-router-common/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(common): add EVIOCRMFF support and tag ProbeEvent with a kind

ProbeEvent gains a `kind` discriminant (PROBE_EVENT_KIND_UPLOADED /
PROBE_EVENT_KIND_ERASED) so the same ring buffer can carry both upload and
erase notifications. eviocrmff_nr() computes EVIOCRMFF's ioctl number the
same way eviocsff_nr() does. FfEffect gains Default (zeroed) for
constructing erase events, which don't carry a meaningful effect payload.
EOF
)"
```

---

### Task 2: eBPF probe — capture EVIOCRMFF

**Files:**
- Modify: `linux-game-haptics-router-ebpf/src/main.rs`

**Interfaces:**
- Consumes: `eviocrmff_nr()`, `PROBE_EVENT_KIND_UPLOADED`, `PROBE_EVENT_KIND_ERASED` (Task 1)
- Produces: erase events on the same `EVENTS` ring buffer, `kind == PROBE_EVENT_KIND_ERASED`

This crate cannot be compiled or tested on this machine (see Global
Constraints) — write the code exactly per the patterns below, verify via
CI once pushed (Task 6).

- [ ] **Step 1: Update the import line**

Change:

```rust
use linux_game_haptics_router_common::{eviocsff_nr, EnterScratch, FfEffect, ProbeEvent};
```

to:

```rust
use linux_game_haptics_router_common::{
    eviocrmff_nr, eviocsff_nr, EnterScratch, FfEffect, ProbeEvent, PROBE_EVENT_KIND_ERASED,
    PROBE_EVENT_KIND_UPLOADED,
};
```

- [ ] **Step 2: Dispatch EVIOCRMFF out of `try_enter`, add `try_enter_erase`**

Change the start of `try_enter`:

```rust
fn try_enter(ctx: &TracePointContext) -> Result<(), i64> {
    let cmd: u64 = unsafe { ctx.read_at(24).map_err(|_| 0i64)? };
    if cmd as u32 != eviocsff_nr() {
        return Ok(());
    }
```

to:

```rust
fn try_enter(ctx: &TracePointContext) -> Result<(), i64> {
    let cmd: u64 = unsafe { ctx.read_at(24).map_err(|_| 0i64)? };
    let cmd = cmd as u32;

    if cmd == eviocrmff_nr() {
        return try_enter_erase(ctx);
    }
    if cmd != eviocsff_nr() {
        return Ok(());
    }
```

(The rest of `try_enter`'s body — reading `arg`, the raw struct, storing
`ENTER_SCRATCH` — is unchanged.)

Add a new function, placed directly after `try_enter`'s closing brace:

```rust
/// EVIOCRMFF ("erase a force-feedback effect") is simpler to capture than
/// EVIOCSFF: the kernel's ioctl handler (`evdev_do_ioctl`) casts its `arg`
/// register directly to an `int` effect id for this command — there is no
/// userspace struct to read, so unlike the upload path, no enter/exit split
/// is needed; the whole thing is captured and submitted here at enter.
fn try_enter_erase(ctx: &TracePointContext) -> Result<(), i64> {
    let arg: u64 = unsafe { ctx.read_at(32).map_err(|_| 0i64)? };
    let effect_id = arg as i32 as i16;

    let tgid_pid = unsafe { aya_ebpf::helpers::bpf_get_current_pid_tgid() };
    let tgid = (tgid_pid >> 32) as u32;

    unsafe {
        bpf_printk!(
            c"game-haptics-router: EVIOCRMFF erase tgid=%d effect_id=%d",
            tgid as i64,
            effect_id as i64
        )
    };

    let event = ProbeEvent {
        kind: PROBE_EVENT_KIND_ERASED,
        tgid,
        effect_id,
        _pad: 0,
        effect: FfEffect::default(),
    };
    if let Some(mut entry) = unsafe { EVENTS.reserve::<ProbeEvent>(0) } {
        entry.write(event);
        entry.submit(0);
    }
    Ok(())
}
```

- [ ] **Step 3: Tag the existing upload event with its kind**

In `try_exit`, change:

```rust
    let event = ProbeEvent {
        tgid,
        effect_id,
        _pad: 0,
        effect,
    };
```

to:

```rust
    let event = ProbeEvent {
        kind: PROBE_EVENT_KIND_UPLOADED,
        tgid,
        effect_id,
        _pad: 0,
        effect,
    };
```

- [ ] **Step 4: Commit**

```bash
git add linux-game-haptics-router-ebpf/src/main.rs
git commit -m "$(cat <<'EOF'
feat(ebpf): capture EVIOCRMFF (effect erase) events

EVIOCRMFF's arg register is the effect id directly (no userspace struct to
read), so it's captured entirely at sys_enter_ioctl with no exit-side work
needed, unlike EVIOCSFF. Tags both upload and erase ProbeEvents with their
kind so userspace can tell them apart on the same ring buffer.
EOF
)"
```

---

### Task 3: Userspace probe loader — forward both event kinds

**Files:**
- Modify: `linux-game-haptics-router/src/ebpf.rs`

**Interfaces:**
- Consumes: `PROBE_EVENT_KIND_UPLOADED`, `PROBE_EVENT_KIND_ERASED` (Task 1), `ProbeEvent.kind` (Task 1/2)
- Produces:
  - `pub enum ProbeEventMsg { Uploaded(EffectUploaded), Erased { tgid: u32, effect_id: i16 } }`
  - `pub async fn load_probe() -> Result<(Ebpf, mpsc::Receiver<ProbeEventMsg>)>` (return type changed from `mpsc::Receiver<EffectUploaded>`)

This crate cannot be compiled/tested on this machine (see Global
Constraints) — verify via CI once pushed (Task 6).

- [ ] **Step 1: Replace the file's contents**

Replace `linux-game-haptics-router/src/ebpf.rs` entirely with:

```rust
use anyhow::Result;
use aya::maps::RingBuf;
use aya::programs::TracePoint;
use aya::{include_bytes_aligned, Ebpf};
use linux_game_haptics_router_common::{ProbeEvent, PROBE_EVENT_KIND_ERASED};
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc;

#[derive(Debug)]
pub struct EffectUploaded {
    pub tgid: u32,
    pub effect_id: i16,
    pub effect: linux_game_haptics_router_common::FfEffect,
}

/// A decoded ring-buffer message: either a freshly-uploaded effect or an
/// erase of one, distinguished by `ProbeEvent.kind` on the wire.
#[derive(Debug)]
pub enum ProbeEventMsg {
    Uploaded(EffectUploaded),
    Erased { tgid: u32, effect_id: i16 },
}

/// Load and attach the eBPF program. Returns a receiver for probe events
/// (both effect uploads and effect erasures).
pub async fn load_probe() -> Result<(Ebpf, mpsc::Receiver<ProbeEventMsg>)> {
    let bpf_bytes =
        include_bytes_aligned!(concat!(env!("OUT_DIR"), "/linux-game-haptics-router-ebpf"));

    let mut bpf = Ebpf::load(bpf_bytes)?;

    // Attach tracepoints
    let enter: &mut TracePoint = bpf.program_mut("sys_enter_ioctl").unwrap().try_into()?;
    enter.load()?;
    enter.attach("syscalls", "sys_enter_ioctl")?;

    let exit: &mut TracePoint = bpf.program_mut("sys_exit_ioctl").unwrap().try_into()?;
    exit.load()?;
    exit.attach("syscalls", "sys_exit_ioctl")?;

    log::info!("eBPF probe loaded and tracepoints attached (sys_enter_ioctl, sys_exit_ioctl)");

    let (tx, rx) = mpsc::channel(256);

    // Poll ring buffer in background task (owned map so it outlives this function
    // independent of `bpf`, which the caller keeps alive to hold the attached programs).
    // The ring buf map fd raises EPOLLIN when new data lands, so we ride that via
    // AsyncFd instead of busy-polling with yield_now()/sleep — no CPU spent when idle.
    let ring = RingBuf::try_from(bpf.take_map("EVENTS").unwrap())?;
    let mut async_fd = AsyncFd::new(ring)?;
    tokio::spawn(async move {
        loop {
            let mut guard = match async_fd.readable_mut().await {
                Ok(guard) => guard,
                Err(e) => {
                    log::error!("ring buffer fd unusable: {}", e);
                    return;
                }
            };
            let ring = guard.get_inner_mut();
            while let Some(item) = ring.next() {
                let bytes: &[u8] = &item;
                if bytes.len() < std::mem::size_of::<ProbeEvent>() {
                    continue;
                }
                let event: ProbeEvent =
                    unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const ProbeEvent) };

                let msg = if event.kind == PROBE_EVENT_KIND_ERASED {
                    log::info!(
                        "ring buffer: effect erased tgid={} effect_id={}",
                        event.tgid,
                        event.effect_id
                    );
                    ProbeEventMsg::Erased {
                        tgid: event.tgid,
                        effect_id: event.effect_id,
                    }
                } else {
                    log::info!(
                        "ring buffer: effect uploaded tgid={} effect_id={} kind={}",
                        event.tgid,
                        event.effect_id,
                        event.effect.kind
                    );
                    ProbeEventMsg::Uploaded(EffectUploaded {
                        tgid: event.tgid,
                        effect_id: event.effect_id,
                        effect: event.effect,
                    })
                };
                let _ = tx.try_send(msg);
            }
            guard.clear_ready();
        }
    });

    Ok((bpf, rx))
}
```

- [ ] **Step 2: Commit**

```bash
git add linux-game-haptics-router/src/ebpf.rs
git commit -m "$(cat <<'EOF'
feat(ebpf-loader): forward erase events alongside uploads

load_probe() now returns mpsc::Receiver<ProbeEventMsg>, an enum of
Uploaded/Erased, decoded from ProbeEvent.kind on the wire instead of always
assuming an upload.
EOF
)"
```

---

### Task 4: `app.rs` — pending-play core logic (pure, testable)

**Files:**
- Modify: `linux-game-haptics-router/src/app.rs`

**Interfaces:**
- Consumes: `EffectUploaded` (unchanged shape, Task 3's module), `FfEffect` (common)
- Produces:
  - `struct PendingPlay { effect_id: i16, issued_at: Instant }`
  - `fn insert_pending(pending: &mut HashMap<String, PendingPlay>, device_id: String, effect_id: i16, issued_at: Instant)`
  - `fn take_pending_for_device(pending: &mut HashMap<String, PendingPlay>, device_id: &str) -> Option<PendingPlay>`
  - `fn take_pending_matching_effect(pending: &mut HashMap<String, PendingPlay>, effect_id: i16) -> Option<(String, PendingPlay)>`
  - `fn take_all_pending_matching_effect(pending: &mut HashMap<String, PendingPlay>, effect_id: i16) -> Vec<(String, PendingPlay)>`
  - `App::handle_effect_uploaded` becomes `pub async fn` (was sync)
  - `App::handle_ff_event` unchanged signature (`pub async fn`)
  - New: `pub async fn handle_effect_erased(&mut self, tgid: u32, effect_id: i16)`

These 4 free functions are pure (no I/O, no async) and directly testable —
same pattern as the existing `upsert_effect`/`purge_effect_id`. The `App`
methods that use them require a real `Arc<Playback>` (a live buttplug
connection) and so, like the rest of this crate, cannot be exercised by a
local `cargo test` on this machine — write them per the exact code below
and verify via CI (Task 6).

- [ ] **Step 1: Write the failing tests for the 4 pure helper functions**

Add to the `#[cfg(test)] mod tests` block (after the existing
`purge_effect_id_removes_regardless_of_tgid` test):

```rust
    fn pending_play(effect_id: i16) -> PendingPlay {
        PendingPlay {
            effect_id,
            issued_at: std::time::Instant::now(),
        }
    }

    #[test]
    fn insert_pending_overwrites_existing_for_same_device() {
        let mut pending = HashMap::new();
        insert_pending(&mut pending, "dev-a".to_string(), 5, std::time::Instant::now());
        insert_pending(&mut pending, "dev-a".to_string(), 9, std::time::Instant::now());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending["dev-a"].effect_id, 9);
    }

    #[test]
    fn take_pending_for_device_removes_and_returns() {
        let mut pending = HashMap::new();
        pending.insert("dev-a".to_string(), pending_play(5));
        let taken = take_pending_for_device(&mut pending, "dev-a");
        assert_eq!(taken.map(|p| p.effect_id), Some(5));
        assert!(pending.is_empty());
    }

    #[test]
    fn take_pending_for_device_missing_returns_none() {
        let mut pending: HashMap<String, PendingPlay> = HashMap::new();
        assert!(take_pending_for_device(&mut pending, "dev-a").is_none());
    }

    #[test]
    fn take_pending_matching_effect_finds_by_id_not_device() {
        let mut pending = HashMap::new();
        pending.insert("dev-a".to_string(), pending_play(5));
        pending.insert("dev-b".to_string(), pending_play(9));
        let (device_id, taken) = take_pending_matching_effect(&mut pending, 9).unwrap();
        assert_eq!(device_id, "dev-b");
        assert_eq!(taken.effect_id, 9);
        assert_eq!(pending.len(), 1);
        assert!(pending.contains_key("dev-a"));
    }

    #[test]
    fn take_pending_matching_effect_no_match_returns_none() {
        let mut pending = HashMap::new();
        pending.insert("dev-a".to_string(), pending_play(5));
        assert!(take_pending_matching_effect(&mut pending, 9).is_none());
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn take_all_pending_matching_effect_returns_every_device_with_that_id() {
        let mut pending = HashMap::new();
        pending.insert("dev-a".to_string(), pending_play(5));
        pending.insert("dev-b".to_string(), pending_play(5));
        pending.insert("dev-c".to_string(), pending_play(9));
        let mut taken = take_all_pending_matching_effect(&mut pending, 5);
        taken.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].0, "dev-a");
        assert_eq!(taken[1].0, "dev-b");
        assert_eq!(pending.len(), 1);
        assert!(pending.contains_key("dev-c"));
    }

    #[test]
    fn take_all_pending_matching_effect_no_match_returns_empty() {
        let mut pending = HashMap::new();
        pending.insert("dev-a".to_string(), pending_play(5));
        let taken = take_all_pending_matching_effect(&mut pending, 9);
        assert!(taken.is_empty());
        assert_eq!(pending.len(), 1);
    }
```

- [ ] **Step 2: Run the tests to confirm they fail to compile**

Attempt: `cargo test -p linux-game-haptics-router app::tests::` (this will
fail to even build on this machine per Global Constraints — that's
expected; the failure here is "function/type not found" once it does build
in CI). Proceed to Step 3 regardless.

- [ ] **Step 3: Implement `PendingPlay` and the 4 pure helper functions**

Add near the top of `linux-game-haptics-router/src/app.rs`, after the
existing `use` statements:

```rust
use std::time::Instant;
```

Add a new struct, placed after the `App` struct definition:

```rust
/// A Play that arrived before its effect was found in `effect_store`. Held
/// (not dropped) until resolved by a matching upload, a Stop for the same
/// device, or an erase of the same effect id — whichever comes first. At
/// most one per device: a later Play for the same device before this one
/// resolves simply overwrites it (see `insert_pending`).
struct PendingPlay {
    effect_id: i16,
    issued_at: Instant,
}
```

Add the 4 free functions, placed after the existing `upsert_effect`
function and before the `#[cfg(test)]` block:

```rust
/// Record a new pending play for `device_id`, overwriting (dropping)
/// whatever was already pending for that device — there is only ever at
/// most one pending play per device.
fn insert_pending(
    pending: &mut HashMap<String, PendingPlay>,
    device_id: String,
    effect_id: i16,
    issued_at: Instant,
) {
    pending.insert(device_id, PendingPlay { effect_id, issued_at });
}

/// Remove and return the pending play for a specific device, if any.
fn take_pending_for_device(
    pending: &mut HashMap<String, PendingPlay>,
    device_id: &str,
) -> Option<PendingPlay> {
    pending.remove(device_id)
}

/// Find, remove, and return the (device_id, pending play) pair whose
/// effect_id matches, if any. Pending plays don't carry a tgid (Play events
/// off evdev don't expose one — matching by id-only mirrors the same
/// tgid-agnostic convention `effect_store` lookups already use), so if two
/// different devices' pending plays happened to share the same numeric
/// effect_id, only one (arbitrary) match is resolved here — an inherent
/// limitation of the existing id-only matching convention, not new.
fn take_pending_matching_effect(
    pending: &mut HashMap<String, PendingPlay>,
    effect_id: i16,
) -> Option<(String, PendingPlay)> {
    let device_id = pending
        .iter()
        .find(|(_, p)| p.effect_id == effect_id)
        .map(|(device_id, _)| device_id.clone())?;
    pending.remove(&device_id).map(|p| (device_id, p))
}

/// Find, remove, and return every (device_id, pending play) pair whose
/// effect_id matches. Used for erase: unlike upload resolution (which only
/// ever needs to resolve one specific device's play), an erase should clear
/// every pending play waiting on that now-gone effect id, however many
/// there are.
fn take_all_pending_matching_effect(
    pending: &mut HashMap<String, PendingPlay>,
    effect_id: i16,
) -> Vec<(String, PendingPlay)> {
    let device_ids: Vec<String> = pending
        .iter()
        .filter(|(_, p)| p.effect_id == effect_id)
        .map(|(device_id, _)| device_id.clone())
        .collect();
    device_ids
        .into_iter()
        .filter_map(|device_id| {
            let p = pending.remove(&device_id)?;
            Some((device_id, p))
        })
        .collect()
}
```

- [ ] **Step 4: Run the tests (in CI once pushed — Task 6 — since this crate can't build locally)**

Expected once CI runs: all 7 new tests PASS, plus the pre-existing
`upsert_*`/`purge_effect_id_*` tests still PASS unchanged.

- [ ] **Step 5: Wire the pure helpers into `App`'s async methods**

Add a `pending_plays` field to the `App` struct — change:

```rust
pub struct App {
    playback: Arc<Playback>,
    effect_store: HashMap<(u32, i16), FfEffect>,
    known_devices: HashMap<String, String>, // device_id -> path
    throttle: Throttle,
    ff_tx: mpsc::Sender<(String, FfEvent)>,
}
```

to:

```rust
pub struct App {
    playback: Arc<Playback>,
    effect_store: HashMap<(u32, i16), FfEffect>,
    pending_plays: HashMap<String, PendingPlay>,
    known_devices: HashMap<String, String>, // device_id -> path
    throttle: Throttle,
    ff_tx: mpsc::Sender<(String, FfEvent)>,
}
```

Update the constructor — change:

```rust
    pub fn new(playback: Arc<Playback>, ff_tx: mpsc::Sender<(String, FfEvent)>) -> Self {
        Self {
            playback,
            effect_store: HashMap::new(),
            known_devices: HashMap::new(),
            throttle: Throttle::new(),
            ff_tx,
        }
    }
```

to:

```rust
    pub fn new(playback: Arc<Playback>, ff_tx: mpsc::Sender<(String, FfEvent)>) -> Self {
        Self {
            playback,
            effect_store: HashMap::new(),
            pending_plays: HashMap::new(),
            known_devices: HashMap::new(),
            throttle: Throttle::new(),
            ff_tx,
        }
    }
```

Replace `handle_effect_uploaded` and `handle_ff_event` entirely — change:

```rust
    pub fn handle_effect_uploaded(&mut self, uploaded: EffectUploaded) {
        log::info!(
            "effect_store: inserting tgid={} effect_id={}",
            uploaded.tgid,
            uploaded.effect_id
        );
        upsert_effect(
            &mut self.effect_store,
            uploaded.tgid,
            uploaded.effect_id,
            uploaded.effect,
        );
    }

    pub async fn handle_ff_event(&mut self, device_id: String, ev: FfEvent) {
        match ev {
            FfEvent::Stop { effect_id } => {
                log::info!("FF event: Stop effect_id={} on {}", effect_id, device_id);
                purge_effect_id(&mut self.effect_store, effect_id);
                self.playback.stop(&device_id).await;
            }
            FfEvent::Play { effect_id } => {
                log::info!("FF event: Play effect_id={} on {}", effect_id, device_id);
                let maybe_effect = self
                    .effect_store
                    .values()
                    .find(|e| e.id == effect_id)
                    .copied();

                match maybe_effect {
                    Some(effect) if self.throttle.should_emit_haptic() => {
                        let points = translate::translate(&effect);
                        if !points.is_empty() {
                            self.playback
                                .schedule_sequence(device_id.clone(), points)
                                .await;
                            self.throttle.record_haptic_emitted();
                        } else {
                            log::info!(
                                "Play effect_id={}: found effect but no points produced (kind={})",
                                effect_id,
                                effect.kind
                            );
                        }
                    }
                    Some(_) => {
                        log::info!("Play effect_id={}: throttled, dropping", effect_id);
                    }
                    None => {
                        log::info!(
                            "Play effect_id={}: no matching effect in store ({} known)",
                            effect_id,
                            self.effect_store.len()
                        );
                    }
                }
            }
        }
    }

    pub async fn stop_all(&self) {
        self.playback.stop_all().await;
    }
}
```

to:

```rust
    pub async fn handle_effect_uploaded(&mut self, uploaded: EffectUploaded) {
        log::info!(
            "effect_store: inserting tgid={} effect_id={}",
            uploaded.tgid,
            uploaded.effect_id
        );
        upsert_effect(
            &mut self.effect_store,
            uploaded.tgid,
            uploaded.effect_id,
            uploaded.effect,
        );

        if let Some((device_id, pending)) =
            take_pending_matching_effect(&mut self.pending_plays, uploaded.effect_id)
        {
            log::info!(
                "pending play effect_id={} on {} resolved after {:?}",
                pending.effect_id,
                device_id,
                pending.issued_at.elapsed()
            );
            self.process_play(device_id, pending.effect_id, uploaded.effect)
                .await;
        }
    }

    pub async fn handle_effect_erased(&mut self, tgid: u32, effect_id: i16) {
        log::info!("effect_store: erasing tgid={} effect_id={}", tgid, effect_id);
        purge_effect_id(&mut self.effect_store, effect_id);

        for (device_id, pending) in
            take_all_pending_matching_effect(&mut self.pending_plays, effect_id)
        {
            log::info!(
                "pending play effect_id={} on {} cleared by erase after {:?}",
                pending.effect_id,
                device_id,
                pending.issued_at.elapsed()
            );
        }
    }

    pub async fn handle_ff_event(&mut self, device_id: String, ev: FfEvent) {
        match ev {
            FfEvent::Stop { effect_id } => {
                log::info!("FF event: Stop effect_id={} on {}", effect_id, device_id);
                purge_effect_id(&mut self.effect_store, effect_id);
                if let Some(pending) = take_pending_for_device(&mut self.pending_plays, &device_id) {
                    log::info!(
                        "pending play effect_id={} on {} cleared by stop after {:?}",
                        pending.effect_id,
                        device_id,
                        pending.issued_at.elapsed()
                    );
                }
                self.playback.stop(&device_id).await;
            }
            FfEvent::Play { effect_id } => {
                log::info!("FF event: Play effect_id={} on {}", effect_id, device_id);
                let maybe_effect = self
                    .effect_store
                    .values()
                    .find(|e| e.id == effect_id)
                    .copied();

                match maybe_effect {
                    Some(effect) => {
                        take_pending_for_device(&mut self.pending_plays, &device_id);
                        self.process_play(device_id, effect_id, effect).await;
                    }
                    None => {
                        log::info!(
                            "Play effect_id={}: not yet in store ({} known), holding pending for {}",
                            effect_id,
                            self.effect_store.len(),
                            device_id
                        );
                        insert_pending(&mut self.pending_plays, device_id, effect_id, Instant::now());
                    }
                }
            }
        }
    }

    /// Shared by an immediate Play match and a resolved pending play: runs
    /// the throttle check, translates the effect, and schedules playback.
    async fn process_play(&mut self, device_id: String, effect_id: i16, effect: FfEffect) {
        if self.throttle.should_emit_haptic() {
            let points = translate::translate(&effect);
            if !points.is_empty() {
                self.playback.schedule_sequence(device_id, points).await;
                self.throttle.record_haptic_emitted();
            } else {
                log::info!(
                    "Play effect_id={}: found effect but no points produced (kind={})",
                    effect_id,
                    effect.kind
                );
            }
        } else {
            log::info!("Play effect_id={}: throttled, dropping", effect_id);
        }
    }

    pub async fn stop_all(&self) {
        self.playback.stop_all().await;
    }
}
```

- [ ] **Step 6: Commit**

```bash
git add linux-game-haptics-router/src/app.rs
git commit -m "$(cat <<'EOF'
feat(app): hold Plays that race ahead of their effect upload

A Play arriving before its effect is in effect_store is now held (not
dropped) in a per-device pending-play slot, resolved event-driven (no
timeout) by whichever comes first: the matching upload, a Stop for that
device, or an erase of that effect id. All three resolution paths log the
elapsed wait. At most one pending play per device — a later Play for the
same device before resolution simply overwrites the earlier one.

The 4 new pending-play helper functions are pure and unit-tested directly,
mirroring the existing upsert_effect/purge_effect_id pattern; the async App
methods that use them (and the rest of this crate) can't be locally built
on this machine (see the plan's Global Constraints) and are verified via
CI.
EOF
)"
```

---

### Task 5: `main.rs` — wire the new event kinds into the select loop

**Files:**
- Modify: `linux-game-haptics-router/src/main.rs`

**Interfaces:**
- Consumes: `ebpf::ProbeEventMsg` (Task 3), `App::handle_effect_uploaded` (now async), `App::handle_effect_erased` (Task 4)

This crate cannot be compiled/tested on this machine (see Global
Constraints) — verify via CI once pushed (Task 6).

- [ ] **Step 1: Update the select loop's upload-handling arm**

Change:

```rust
            Some(uploaded) = effect_rx.recv() => {
                app.handle_effect_uploaded(uploaded);
            }
```

to:

```rust
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
```

(`effect_rx`'s declared type comes from `ebpf::load_probe()`'s return type,
already updated in Task 3 — no local variable type annotation needs
changing here.)

- [ ] **Step 2: Commit**

```bash
git add linux-game-haptics-router/src/main.rs
git commit -m "$(cat <<'EOF'
feat(main): dispatch both upload and erase probe events

Matches the ProbeEventMsg enum from the updated load_probe() channel,
routing Uploaded to the now-async handle_effect_uploaded and Erased to the
new handle_effect_erased.
EOF
)"
```

---

### Task 6: CI verification and e2e validation

**Files:** none (verification only, plus doc cleanup below)

- [ ] **Step 1: Push the branch and watch CI**

```bash
git push -u origin fix/ff-play-upload-race
```

Check the `check (x86_64-unknown-linux-gnu)`, `check (aarch64-unknown-linux-gnu)`,
`test`, `clippy (x86_64-unknown-linux-gnu)`, `clippy (aarch64-unknown-linux-gnu)`,
`build (x86_64-unknown-linux-gnu)`, `build (aarch64-unknown-linux-gnu)`,
and `fmt` jobs on the pushed commit. All must go green — this is the first
real compile of every task in this plan. Fix any compile errors found here
(most likely candidates: a typo in one of the hand-written diffs above, or
a borrow-checker complaint in `take_all_pending_matching_effect`'s
closure — if so, restructure the closure to avoid double-borrowing
`pending` inside `filter_map`, e.g. collect into a `Vec` first as already
written, which should already avoid this).

- [ ] **Step 2: Run the e2e suite against this fix**

```bash
./e2e/run.sh
```

(Requires QEMU + KVM locally, or push and check the `e2e (ubuntu-latest)`
CI job.) Expected: all 5 scenarios PASS, including `ff_rumble` — the
first-scenario race this whole plan exists to fix.

- [ ] **Step 3: If `ff_rumble` still fails, stop and investigate before proceeding**

Do not remove the known-issue caveat (next step) unless Step 2 actually
confirms the fix. If it still fails, capture the daemon's stderr log from
the e2e run (same `daemon stderr:` block pattern used throughout this
project's CI debugging) and check whether the new "holding pending for
{device}" / "resolved after {duration}" log lines appear — their absence
would mean the wiring in Tasks 3-5 isn't actually being exercised, their
presence with a "still no matching effect" outcome would point at a bug in
`take_pending_matching_effect`'s id matching.

- [ ] **Step 4: Remove the known-issue caveat**

Once Step 2 confirms all 5 scenarios pass, read `README.md`'s
"End-to-end tests" section (added in the e2e harness branch) and remove
the "Known flaky failure" paragraph about `ff_rumble` entirely — it no
longer applies. Check `CLAUDE.md` for any duplicate mention and remove it
too if present.

```bash
git add README.md CLAUDE.md
git commit -m "$(cat <<'EOF'
docs: remove the ff_rumble known-issue caveat, now fixed

Confirmed via e2e/run.sh: all 5 scenarios pass with the pending-play fix
from this branch.
EOF
)"
```

- [ ] **Step 5: Push the final state**

```bash
git push origin fix/ff-play-upload-race
```

---

## Self-Review Notes

- **Spec coverage:** every section of the design spec maps to a task —
  event-driven pending-play resolution (Task 4), EVIOCRMFF support (Tasks
  1-3), elapsed-delay logging on all three outcomes (Task 4), e2e
  validation + caveat removal (Task 6, brought into scope per the
  approved spec amendment).
- **Placeholder scan:** no TBD/TODO; every step has literal code. The one
  "if broken, investigate X" step (Task 6 Step 3) is a contingency branch
  for CI-only-verifiable code, not a placeholder — it names the exact log
  lines and code paths to check, not a vague "handle errors" instruction.
- **Type consistency:** `PendingPlay { effect_id: i16, issued_at: Instant }`
  used identically across all 4 helper functions and both call sites in
  `App`'s methods; `ProbeEventMsg::Uploaded(EffectUploaded)` /
  `Erased { tgid: u32, effect_id: i16 }` match between Task 3's producer
  and Task 5's consumer.
- **Environment constraint called out explicitly:** unlike the earlier e2e
  harness plan (where only the uinput-touching module was untestable
  locally), *every* task here touches `linux-game-haptics-router` or
  `linux-game-haptics-router-ebpf`, so *no* task in this plan can be
  locally compiled on the authoring machine (macOS, no working
  `bpf-linker` cross-compile) — this is stated once in Global Constraints
  and referenced per-task rather than repeated in full each time.
