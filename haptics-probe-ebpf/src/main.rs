#![no_std]
#![no_main]

use aya_bpf::macros::tracepoint;
use aya_bpf::programs::TracePointContext;

/// Tracepoint: sys_enter_ioctl (placeholder)
#[tracepoint]
pub fn sys_enter_ioctl(_ctx: TracePointContext) -> i32 { 0 }

/// Tracepoint: sys_exit_ioctl (placeholder)
#[tracepoint]
pub fn sys_exit_ioctl(_ctx: TracePointContext) -> i32 { 0 }

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! { loop {} }
