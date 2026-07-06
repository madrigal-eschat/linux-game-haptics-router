use crate::device::{self, DeviceInfo, FfEvent};
use crate::ebpf::EffectUploaded;
use crate::playback::Playback;
use crate::throttle::Throttle;
use crate::translate;
use linux_game_haptics_router_common::FfEffect;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Owns all daemon state: the effect table learned from the eBPF probe, the
/// set of evdev devices we've already spawned readers for, and the haptic
/// throttle. Playback itself still owns the buttplug connection.
pub struct App {
    playback: Arc<Playback>,
    effect_store: HashMap<(u32, i16), FfEffect>,
    known_devices: HashMap<String, String>, // device_id -> path
    throttle: Throttle,
    ff_tx: mpsc::Sender<(String, FfEvent)>,
}

impl App {
    pub fn new(playback: Arc<Playback>, ff_tx: mpsc::Sender<(String, FfEvent)>) -> Self {
        Self {
            playback,
            effect_store: HashMap::new(),
            known_devices: HashMap::new(),
            throttle: Throttle::new(),
            ff_tx,
        }
    }

    /// Spawn a background reader for a newly discovered device and remember
    /// its path so future rescans can tell it apart from a reconnect.
    pub fn spawn_reader(&mut self, info: &DeviceInfo) {
        spawn_device_reader(info, &self.ff_tx);
        self.known_devices
            .insert(info.device_id.clone(), info.path.clone());
    }

    /// Re-list FF devices and spawn readers for any that are new or that
    /// reappeared at a different /dev/input path after a reconnect.
    pub fn rescan_devices(&mut self) {
        let Ok(current) = device::list_ff_devices() else {
            return;
        };
        for info in &current {
            let is_new = match self.known_devices.get(&info.device_id) {
                None => true,
                Some(known_path) => known_path != &info.path,
            };
            if is_new {
                log::info!(
                    "rescan: device {} ({}) at {}",
                    info.device_id,
                    info.name,
                    info.path
                );
                self.spawn_reader(info);
            }
        }
    }

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

fn spawn_device_reader(info: &DeviceInfo, ff_tx: &mpsc::Sender<(String, FfEvent)>) {
    log::info!(
        "device added: {} ({}) at {}",
        info.device_id,
        info.name,
        info.path
    );
    let tx = ff_tx.clone();
    let path = info.path.clone();
    let device_id = info.device_id.clone();
    tokio::task::spawn_blocking(move || {
        let mut backoff = std::time::Duration::from_millis(200);
        const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(5);
        loop {
            match evdev::Device::open(&path) {
                Ok(mut dev) => {
                    backoff = std::time::Duration::from_millis(200);
                    loop {
                        match device::next_ff_event(&mut dev) {
                            Ok(ev) => {
                                let _ = tx.blocking_send((device_id.clone(), ev));
                            }
                            Err(e) => {
                                log::warn!(
                                    "evdev read error on {}: {}, will retry reopening",
                                    device_id,
                                    e
                                );
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    log::warn!("failed to open {}: {}, will retry", path, e);
                }
            }
            std::thread::sleep(backoff);
            backoff = std::cmp::min(backoff * 2, MAX_BACKOFF);
        }
    });
}

/// Remove any entry for `effect_id`, regardless of which tgid owns it.
fn purge_effect_id(store: &mut HashMap<(u32, i16), FfEffect>, effect_id: i16) {
    store.retain(|(_, id), _| *id != effect_id);
}

/// A numeric effect_id is only meaningful for the most recent upload; the
/// kernel reuses ids across processes/sessions and stale entries from prior
/// tgids are never otherwise cleaned up (no Stop write arrives for effects
/// that finish naturally or are removed via EVIOCRMFF), so purge any other
/// tgid's entry for this id before inserting the new one.
fn upsert_effect(
    store: &mut HashMap<(u32, i16), FfEffect>,
    tgid: u32,
    effect_id: i16,
    effect: FfEffect,
) {
    purge_effect_id(store, effect_id);
    store.insert((tgid, effect_id), effect);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_effect() -> FfEffect {
        FfEffect {
            kind: 0,
            id: 0,
            direction: 0,
            trigger_button: 0,
            trigger_interval: 0,
            replay_length: 0,
            replay_delay: 0,
            u: [0u16; 7],
        }
    }

    #[test]
    fn upsert_replaces_same_tgid_and_id() {
        let mut store = HashMap::new();
        upsert_effect(&mut store, 1, 5, dummy_effect());
        upsert_effect(&mut store, 1, 5, dummy_effect());
        assert_eq!(store.len(), 1);
        assert!(store.contains_key(&(1, 5)));
    }

    #[test]
    fn upsert_evicts_other_tgid_with_same_effect_id() {
        let mut store = HashMap::new();
        upsert_effect(&mut store, 1, 5, dummy_effect());
        upsert_effect(&mut store, 2, 5, dummy_effect());
        assert_eq!(store.len(), 1);
        assert!(!store.contains_key(&(1, 5)));
        assert!(store.contains_key(&(2, 5)));
    }

    #[test]
    fn upsert_keeps_other_effect_ids_for_same_tgid() {
        let mut store = HashMap::new();
        upsert_effect(&mut store, 1, 5, dummy_effect());
        upsert_effect(&mut store, 1, 6, dummy_effect());
        upsert_effect(&mut store, 1, 5, dummy_effect());
        assert_eq!(store.len(), 2);
        assert!(store.contains_key(&(1, 5)));
        assert!(store.contains_key(&(1, 6)));
    }

    #[test]
    fn purge_effect_id_removes_regardless_of_tgid() {
        let mut store = HashMap::new();
        store.insert((1, 5), dummy_effect());
        store.insert((2, 7), dummy_effect());
        purge_effect_id(&mut store, 5);
        assert_eq!(store.len(), 1);
        assert!(store.contains_key(&(2, 7)));
    }
}
