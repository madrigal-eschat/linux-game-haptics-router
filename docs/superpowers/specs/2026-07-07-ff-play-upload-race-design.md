# FF Play/Upload race fix — design

## Purpose

The daemon learns about an uploaded FF effect via two independent,
unsynchronized channels: the eBPF ring buffer (upload notification —
kernel ring buffer → epoll → tokio `AsyncFd` → channel → `App`'s select
loop) and the per-device evdev reader (Play/Stop notification — a blocking
`fetch_events()` read in its own OS thread, which wakes on data almost
immediately). The evdev path is structurally faster with fewer hops. A
game that uploads an effect and immediately calls `play()` — a normal,
common pattern — can have its Play arrive at `App` before the
corresponding upload has been processed, and `App` currently drops that
Play silently ("no matching effect in store", no retry). This was found via
the e2e VM test harness (`ff_rumble`'s first-scenario flake) but is a real
production correctness gap, not a test-only artifact.

Separately: nothing today tells the probe when an effect is freed via
`EVIOCRMFF`. `EFFECT_STORE`/`ENTER_SCRATCH` are `LruHashMap`s specifically
to survive this leak, but a freed id can be silently reused by an unrelated
upload before the probe would ever know the old one was gone.

## Scope

Two changes, landed together since the erase signal is used by the pending-play
resolution logic:

1. **Event-driven pending-play resolution** in `app.rs`: a Play that arrives
   before its effect is in the store is held (not dropped) and resolved
   the moment the matching upload arrives — no timeout, no polling.
2. **`EVIOCRMFF` (erase) support** in the eBPF probe: a new event kind so
   `App` learns when an effect is freed, both to purge `effect_store`
   proactively (rather than only reactively on id reuse) and to correctly
   drop a pending play waiting on an effect that's been erased out from
   under it.

Out of scope: fixing the e2e harness itself (already merged, separate
branch) — though re-running it against this fix is a natural validation
step, not a blocking part of this branch's own tests.

## eBPF probe changes

`EVIOCRMFF` is simpler to capture than `EVIOCSFF`: the kernel's ioctl
handler (`evdev_do_ioctl`) casts its `arg` register directly to an `int`
effect id for this command — there's no userspace struct to read, so no
enter/exit split is needed (unlike `EVIOCSFF`, which needs the exit side to
learn the kernel-assigned id).

- `linux-game-haptics-router-common`: add `eviocrmff_nr() -> u32`, computed
  the same way as the existing `eviocsff_nr()` but for
  `_IOW('E', 0x81, int)` (write direction, nr `0x81`, size
  `size_of::<i32>()` = 4) instead of `EVIOCSFF`'s struct size.
- `linux-game-haptics-router-ebpf/src/main.rs`: `sys_enter_ioctl` gains a
  second `cmd` comparison against `eviocrmff_nr()`. On match, read `tgid`
  from `bpf_get_current_pid_tgid()` (already done for the upload path) and
  the effect id directly from the tracepoint's captured `arg` value (cast
  to `i32`, narrowed to `i16` to match `FfEffect::id`'s existing type) — no
  `bpf_probe_read_user_buf` call needed for this path. Submit an event to
  the same `EVENTS` ring buffer immediately; no `ENTER_SCRATCH` entry, no
  `sys_exit_ioctl` involvement for this command.

## Wire format: tagged `ProbeEvent`

Rather than a second ring buffer (doubles map count and userspace polling
complexity — a second `AsyncFd` to select over — for a fairly marginal size
saving), add a discriminant to the existing `ProbeEvent` struct:

```rust
#[repr(u8)]
pub enum ProbeEventKind {
    Uploaded = 0,
    Erased = 1,
}

#[repr(C)]
pub struct ProbeEvent {
    pub kind: u8,          // ProbeEventKind
    pub tgid: u32,
    pub effect_id: i16,
    pub _pad: u16,
    pub effect: FfEffect,  // zeroed/unused when kind == Erased
}
```

One ring buffer, one `AsyncFd`, one poll loop in `ebpf.rs` (unchanged
structurally) — it branches on `kind` before forwarding to `App` as either
`EffectUploaded` (existing) or a new `EffectErased { tgid, effect_id }`.
The wasted ~20 bytes of unused `FfEffect` payload per erase event is
negligible given ring buffer entries are small and erases are infrequent
relative to uploads.

## `app.rs`: pending-play state

```rust
struct PendingPlay {
    effect_id: i16,
    issued_at: Instant,   // diagnostic only — never used to control timing
}

pending_plays: HashMap<String, PendingPlay>,  // device_id -> at most one entry
```

Behavior, by event:

- **Play arrives, effect found in store:** process immediately via a
  shared helper (translate → throttle check → `schedule_sequence`) — same
  as today's behavior — and remove any pending entry for that device
  (defensive; there shouldn't normally be one if a match was found
  immediately, but a stale entry must never survive).
- **Play arrives, effect not found:** insert into `pending_plays`, keyed by
  `device_id`. `HashMap::insert` naturally overwrites — so a second Play
  for the same device before the first ever resolves drops the first
  pending entry (matches the "only one pending per device, newest wins"
  requirement) with no explicit extra logic needed.
- **Stop arrives:** existing behavior (`purge_effect_id`, `playback.stop`)
  plus: remove any pending entry for that device. Because Play and Stop
  for the same device both flow through that device's single evdev reader
  thread, any Stop `App` processes is guaranteed to have been written
  *after* any Play it already processed for that device — so "Stop clears
  a pending Play" can never fire on a Stop that actually predates the
  pending Play. If a pending entry existed, log its resolution as
  "cleared by stop" along with `Instant::now() - issued_at`.
- **Upload arrives (`EffectUploaded`):** existing `upsert_effect` insert,
  plus: scan `pending_plays` (small — bounded by device count) for any
  device whose pending `effect_id` matches the uploaded one. If found,
  resolve it through the same shared processing helper used for a live
  Play match, remove the pending entry, and log the resolution with
  `Instant::now() - issued_at` (the actual wait duration).
- **Erase arrives (`EffectErased`, new):** existing `purge_effect_id`
  (already tgid-agnostic, matching existing eviction semantics), plus:
  remove any pending entries whose `effect_id` matches (regardless of
  device or tgid — the effect they were waiting for no longer exists). Log
  "cleared by erase" with the elapsed wait, same as the other two outcomes.

All three resolution outcomes (resolved, cleared-by-stop, cleared-by-erase)
get the same elapsed-delay log line, distinguished by outcome — this is
purely observability; nothing here reads that duration to make a decision.

## Error handling

No new failure modes. Worst case, a pending play sits until superseded by
the next Play/Stop for that device — bounded by the number of FF-capable
devices (small), so no unbounded growth and no timeout/cleanup task needed.

## Testing

- `linux-game-haptics-router-common`: unit test for `eviocrmff_nr()`
  mirroring the existing `eviocsff_nr()` test (verify the computed ioctl
  number against the real kernel value for `EVIOCRMFF`).
- `app.rs`: new unit tests covering —
  - Play miss creates a pending entry.
  - A later matching upload resolves it (processes the play, removes the
    entry, logs elapsed delay).
  - A second Play for the same device before resolution drops the first
    pending entry (supersede).
  - Stop clears a pending entry for that device.
  - Erase clears any pending entry matching that effect_id.
  - Play-with-immediate-match still processes immediately (regression
    check — existing behavior unchanged) and clears any stale pending
    entry defensively.
- Manual/e2e validation: re-run `e2e/run.sh` against this fix as a sanity
  check that `ff_rumble`'s first-scenario race no longer reproduces — not
  a blocking requirement of this branch's own test suite, since the e2e
  harness lives in a separate, already-merged branch.
