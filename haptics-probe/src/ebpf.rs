use anyhow::Result;
use aya::maps::{HashMap as AyaHashMap, RingBuf};
use aya::programs::TracePoint;
use aya::{include_bytes_aligned, Bpf};
use haptics_probe_common::ProbeEvent;
use tokio::sync::mpsc;

#[derive(Debug)]
pub struct EffectUploaded {
    pub tgid: u32,
    pub effect_id: i16,
    pub effect: haptics_probe_common::FfEffect,
}

/// Load and attach the eBPF program. Returns a receiver for effect-upload events.
pub async fn load_probe() -> Result<(Bpf, mpsc::Receiver<EffectUploaded>)> {
    #[cfg(debug_assertions)]
    let bpf_bytes = include_bytes_aligned!(
        "../../target/bpfel-unknown-none/debug/haptics-probe-ebpf"
    );
    #[cfg(not(debug_assertions))]
    let bpf_bytes = include_bytes_aligned!(
        "../../target/bpfel-unknown-none/release/haptics-probe-ebpf"
    );

    let mut bpf = Bpf::load(bpf_bytes)?;

    // Attach tracepoints
    let enter: &mut TracePoint = bpf.program_mut("sys_enter_ioctl").unwrap().try_into()?;
    enter.load()?;
    enter.attach("syscalls", "sys_enter_ioctl")?;

    let exit: &mut TracePoint = bpf.program_mut("sys_exit_ioctl").unwrap().try_into()?;
    exit.load()?;
    exit.attach("syscalls", "sys_exit_ioctl")?;

    let (tx, rx) = mpsc::channel(256);

    // Poll ring buffer in background task
    let mut ring: RingBuf<_> = bpf.map_mut("EVENTS").unwrap().try_into()?;
    tokio::spawn(async move {
        loop {
            tokio::task::yield_now().await;
            while let Some(item) = ring.next() {
                let bytes: &[u8] = &item;
                if bytes.len() < std::mem::size_of::<ProbeEvent>() { continue; }
                let event: ProbeEvent = unsafe {
                    std::ptr::read_unaligned(bytes.as_ptr() as *const ProbeEvent)
                };
                let _ = tx.try_send(EffectUploaded {
                    tgid: event.tgid,
                    effect_id: event.effect_id,
                    effect: event.effect,
                });
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        }
    });

    Ok((bpf, rx))
}
