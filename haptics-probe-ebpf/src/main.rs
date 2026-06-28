#![no_std]
#![no_main]

use aya_bpf::macros::map;
use aya_bpf::macros::tracepoint;
use aya_bpf::programs::TracePointContext;
use haptics_probe_common::{EnterScratch, FfEffect, ProbeEvent, eviocsff_nr};
use aya_bpf::helpers::bpf_probe_read_user_buf;

/// Per-thread scratch: tgid<<32|pid → EnterScratch
#[map]
static mut ENTER_SCRATCH: aya_bpf::maps::HashMap<u64, EnterScratch> =
    aya_bpf::maps::HashMap::with_max_entries(1024, 0);

/// Effect store: (tgid<<32|effect_id) → FfEffect
#[map]
pub static mut EFFECT_STORE: aya_bpf::maps::HashMap<u64, FfEffect> =
    aya_bpf::maps::HashMap::with_max_entries(4096, 0);

/// Ring buffer for events to userspace
#[map]
static mut EVENTS: aya_bpf::maps::RingBuf = aya_bpf::maps::RingBuf::with_byte_size(256 * 1024, 0);

/// Tracepoint: sys_enter_ioctl
#[tracepoint]
pub fn sys_enter_ioctl(ctx: TracePointContext) -> i32 {
    match try_enter(ctx) {
        Ok(_) => 0,
        Err(_) => 0,
    }
}

fn try_enter(ctx: &TracePointContext) -> Result<(), i64> {
    let cmd: u64 = unsafe { bpf_probe_read_kernel(ctx, 24).map_err(|_| 0i64)? };
    if cmd as u32 != eviocsff_nr() {
        return Ok(());
    }

    let fd: u64 = unsafe { ctx.read_at(16).map_err(|_| 0i64)? };
    let arg: u64 = unsafe { ctx.read_at(32).map_err(|_| 0i64)? };

    let mut effect = FfEffect {
        kind: 0,
        id: 0,
        direction: 0,
        trigger_button: 0,
        trigger_interval: 0,
        replay_length: 0,
        replay_delay: 0,
        u: [0u16; 7],
    };

    let effect_bytes = unsafe {
        core::slice::from_raw_parts_mut(
            &mut effect as *mut FfEffect as *mut u8,
            core::mem::size_of::<FfEffect>(),
        )
    };
    unsafe { bpf_probe_read_user_buf(arg as *const u8, effect_bytes).map_err(|_| 0i64)? };

    let tgid_pid = unsafe { aya_bpf::helpers::bpf_get_current_pid_tgid() };
    let scratch = EnterScratch {
        ff_effect_ptr: arg,
        effect,
    };
    unsafe {
        ENTER_SCRATCH.insert(&tgid_pid, &scratch, 0).map_err(|_| 0i64)?;
    }
    Ok(())
}

/// Tracepoint: sys_exit_ioctl
#[tracepoint]
pub fn sys_exit_ioctl(ctx: TracePointContext) -> i32 {
    match try_exit(ctx) {
        Ok(_) => 0,
        Err(_) => 0,
    }
}

fn try_exit(ctx: &TracePointContext) -> Result<(), i64> {
    let tgid_pid = unsafe { aya_bpf::helpers::bpf_get_current_pid_tgid() };
    let scratch = unsafe { ENTER_SCRATCH.get(&tgid_pid) }.ok_or(0i64)?;

    let mut id_bytes = [0u8; 2];
    unsafe {
        bpf_probe_read_user_buf(
            (scratch.ff_effect_ptr + 2) as *const u8,
            &mut id_bytes,
        ).map_err(|_| 0i64)?
    };
    let effect_id = i16::from_le_bytes(id_bytes);

    let tgid = (tgid_pid >> 32) as u32;
    let mut effect = scratch.effect;
    effect.id = effect_id;

    let store_key = ((tgid as u64) << 32) | (effect_id as u16 as u64);
    unsafe {
        EFFECT_STORE.insert(&store_key, &effect, 0).map_err(|_| 0i64)?;
        ENTER_SCRATCH.remove(&tgid_pid).ok();
    };

    let event = ProbeEvent {
        tgid,
        effect_id,
        _pad: 0,
        effect,
    };
    if let Some(mut entry) = unsafe {
        EVENTS.reserve::<ProbeEvent>(0)
    } {
        entry.write(event);
        entry.submit(0);
    }
    Ok(())
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
