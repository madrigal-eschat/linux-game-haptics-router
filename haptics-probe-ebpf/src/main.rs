#![no_std]
#![no_main]

use aya_ebpf::bpf_printk;
use aya_ebpf::helpers::bpf_probe_read_user_buf;
use aya_ebpf::macros::map;
use aya_ebpf::macros::tracepoint;
use aya_ebpf::programs::TracePointContext;
use haptics_probe_common::{eviocsff_nr, EnterScratch, FfEffect, ProbeEvent};

/// Per-thread scratch: tgid<<32|pid → EnterScratch.
/// LRU because a killed/aborted thread never reaches sys_exit_ioctl to
/// remove its entry — without eviction this leaks one slot per such thread
/// until the map fills and inserts start silently failing.
#[map]
static mut ENTER_SCRATCH: aya_ebpf::maps::LruHashMap<u64, EnterScratch> =
    aya_ebpf::maps::LruHashMap::with_max_entries(1024, 0);

/// Effect store: (tgid<<32|effect_id) → FfEffect.
/// LRU for the same reason: nothing removes an entry when its owning
/// process exits or the effect is freed via EVIOCRMFF, so long-running
/// sessions across many games would otherwise exhaust a plain HashMap.
#[map]
pub static mut EFFECT_STORE: aya_ebpf::maps::LruHashMap<u64, FfEffect> =
    aya_ebpf::maps::LruHashMap::with_max_entries(4096, 0);

/// Ring buffer for events to userspace
#[map]
static mut EVENTS: aya_ebpf::maps::RingBuf = aya_ebpf::maps::RingBuf::with_byte_size(256 * 1024, 0);

/// Tracepoint: sys_enter_ioctl
#[tracepoint]
pub fn sys_enter_ioctl(ctx: TracePointContext) -> i32 {
    match try_enter(&ctx) {
        Ok(_) => 0,
        Err(_) => 0,
    }
}

fn try_enter(ctx: &TracePointContext) -> Result<(), i64> {
    let cmd: u64 = unsafe { ctx.read_at(24).map_err(|_| 0i64)? };
    if cmd as u32 != eviocsff_nr() {
        return Ok(());
    }
    unsafe { bpf_printk!(c"haptics-probe: EVIOCSFF cmd matched (0x%x)", cmd as u32) };

    let arg: u64 = unsafe { ctx.read_at(32).map_err(|_| 0i64)? };

    // Raw kernel `struct ff_effect` layout (x86_64):
    //   0: type (u16)        2: id (s16)           4: direction (u16)
    //   6: trigger.button (u16)   8: trigger.interval (u16)
    //  10: replay.length (u16)  12: replay.delay (u16)
    //  14: (2 bytes padding — union requires 8-byte alignment for its
    //       embedded custom_data pointer)
    //  16: union u (only need its first 14 bytes; every effect type we
    //      translate — rumble/constant/ramp/periodic — packs its relevant
    //      fields within that span)
    // We read 30 raw bytes and pick fields out at their real kernel
    // offsets, rather than memcpy'ing onto our differently-shaped FfEffect.
    let mut raw = [0u8; 30];
    if let Err(e) = unsafe { bpf_probe_read_user_buf(arg as *const u8, &mut raw) } {
        unsafe {
            bpf_printk!(
                c"haptics-probe: probe_read_user_buf FAILED arg=0x%lx err=%d",
                arg,
                e as i64
            )
        };
        return Err(0);
    }

    let u16_at = |off: usize| u16::from_ne_bytes([raw[off], raw[off + 1]]);

    let effect = FfEffect {
        kind: u16_at(0),
        id: 0, // filled in on sys_exit_ioctl once the kernel assigns it
        direction: u16_at(4),
        trigger_button: u16_at(6),
        trigger_interval: u16_at(8),
        replay_length: u16_at(10),
        replay_delay: u16_at(12),
        u: [
            u16_at(16),
            u16_at(18),
            u16_at(20),
            u16_at(22),
            u16_at(24),
            u16_at(26),
            u16_at(28),
        ],
    };

    let tgid_pid = unsafe { aya_ebpf::helpers::bpf_get_current_pid_tgid() };
    let scratch = EnterScratch {
        ff_effect_ptr: arg,
        effect,
    };
    unsafe {
        ENTER_SCRATCH
            .insert(&tgid_pid, &scratch, 0)
            .map_err(|_| 0i64)?;
    }
    unsafe {
        bpf_printk!(
            c"haptics-probe: ENTER_SCRATCH stored, kind=%d replay_length=%d",
            effect.kind as i64,
            effect.replay_length as i64
        )
    };
    Ok(())
}

/// Tracepoint: sys_exit_ioctl
#[tracepoint]
pub fn sys_exit_ioctl(ctx: TracePointContext) -> i32 {
    match try_exit(&ctx) {
        Ok(_) => 0,
        Err(_) => 0,
    }
}

fn try_exit(ctx: &TracePointContext) -> Result<(), i64> {
    let tgid_pid = unsafe { aya_ebpf::helpers::bpf_get_current_pid_tgid() };
    let scratch = match unsafe { ENTER_SCRATCH.get(&tgid_pid) } {
        Some(s) => s,
        None => return Ok(()), // no matching enter for this thread — not an EVIOCSFF exit
    };

    let mut id_bytes = [0u8; 2];
    if let Err(e) =
        unsafe { bpf_probe_read_user_buf((scratch.ff_effect_ptr + 2) as *const u8, &mut id_bytes) }
    {
        unsafe {
            bpf_printk!(
                c"haptics-probe: exit probe_read_user_buf(id) FAILED err=%d",
                e as i64
            )
        };
        return Err(0);
    }
    let effect_id = i16::from_le_bytes(id_bytes);

    let tgid = (tgid_pid >> 32) as u32;
    let mut effect = scratch.effect;
    effect.id = effect_id;

    let store_key = ((tgid as u64) << 32) | (effect_id as u16 as u64);
    unsafe {
        EFFECT_STORE
            .insert(&store_key, &effect, 0)
            .map_err(|_| 0i64)?;
        ENTER_SCRATCH.remove(&tgid_pid).ok();
    };
    unsafe {
        bpf_printk!(
            c"haptics-probe: EFFECT_STORE inserted tgid=%d effect_id=%d",
            tgid as i64,
            effect_id as i64
        )
    };

    let event = ProbeEvent {
        tgid,
        effect_id,
        _pad: 0,
        effect,
    };
    if let Some(mut entry) = unsafe { EVENTS.reserve::<ProbeEvent>(0) } {
        entry.write(event);
        entry.submit(0);
    }
    Ok(())
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
