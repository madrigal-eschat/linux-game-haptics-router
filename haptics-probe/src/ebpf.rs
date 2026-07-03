use anyhow::Result;
use aya::maps::RingBuf;
use aya::programs::TracePoint;
use aya::{include_bytes_aligned, Ebpf};
use haptics_probe_common::ProbeEvent;
use tokio::sync::mpsc;

#[derive(Debug)]
pub struct EffectUploaded {
    pub tgid: u32,
    pub effect_id: i16,
    pub effect: haptics_probe_common::FfEffect,
}

/// Load and attach the eBPF program. Returns a receiver for effect-upload events.
pub async fn load_probe() -> Result<(Ebpf, mpsc::Receiver<EffectUploaded>)> {
    let bpf_bytes = include_bytes_aligned!(concat!(env!("OUT_DIR"), "/haptics-probe-ebpf"));

    let mut bpf = Ebpf::load(bpf_bytes)?;

    // Attach tracepoints
    let enter: &mut TracePoint = bpf.program_mut("sys_enter_ioctl").unwrap().try_into()?;
    enter.load()?;
    enter.attach("syscalls", "sys_enter_ioctl")?;

    let exit: &mut TracePoint = bpf.program_mut("sys_exit_ioctl").unwrap().try_into()?;
    exit.load()?;
    exit.attach("syscalls", "sys_exit_ioctl")?;

    let (tx, rx) = mpsc::channel(256);

    // Poll ring buffer in background task (owned map so it outlives this function
    // independent of `bpf`, which the caller keeps alive to hold the attached programs)
    let mut ring = RingBuf::try_from(bpf.take_map("EVENTS").unwrap())?;
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
