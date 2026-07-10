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
