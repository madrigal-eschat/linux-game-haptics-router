use crate::device::{self, DeviceInfo, FfEvent};
use crate::ebpf::EffectUploaded;
use crate::playback::PlaybackOps;
use crate::throttle::Throttle;
use crate::translate;
use linux_game_haptics_router_common::FfEffect;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

/// Owns all daemon state: the effect table learned from the eBPF probe, the
/// set of evdev devices we've already spawned readers for, and the haptic
/// throttle. Playback itself still owns the buttplug connection.
pub struct App {
    playback: Arc<dyn PlaybackOps>,
    effect_store: HashMap<(u32, i16), FfEffect>,
    pending_plays: HashMap<String, PendingPlay>,
    known_devices: HashMap<String, String>, // device_id -> path
    throttle: Throttle,
    ff_tx: mpsc::Sender<(String, FfEvent)>,
}

/// A Play that arrived before its effect was found in `effect_store`. Held
/// (not dropped) until resolved by a matching upload, a Stop for the same
/// device, or an erase of the same effect id — whichever comes first. At
/// most one per device: a later Play for the same device before this one
/// resolves simply overwrites it (see `insert_pending`).
struct PendingPlay {
    effect_id: i16,
    issued_at: Instant,
}

impl App {
    pub fn new(playback: Arc<dyn PlaybackOps>, ff_tx: mpsc::Sender<(String, FfEvent)>) -> Self {
        Self {
            playback,
            effect_store: HashMap::new(),
            pending_plays: HashMap::new(),
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

        for (device_id, pending) in
            take_all_pending_matching_effect(&mut self.pending_plays, uploaded.effect_id)
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
        log::info!(
            "effect_store: erasing tgid={} effect_id={}",
            tgid,
            effect_id
        );
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
                if let Some(pending) = take_pending_for_device(&mut self.pending_plays, &device_id)
                {
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
                        insert_pending(
                            &mut self.pending_plays,
                            device_id,
                            effect_id,
                            Instant::now(),
                        );
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

/// Record a new pending play for `device_id`, overwriting (dropping)
/// whatever was already pending for that device — there is only ever at
/// most one pending play per device.
fn insert_pending(
    pending: &mut HashMap<String, PendingPlay>,
    device_id: String,
    effect_id: i16,
    issued_at: Instant,
) {
    pending.insert(
        device_id,
        PendingPlay {
            effect_id,
            issued_at,
        },
    );
}

/// Remove and return the pending play for a specific device, if any.
fn take_pending_for_device(
    pending: &mut HashMap<String, PendingPlay>,
    device_id: &str,
) -> Option<PendingPlay> {
    pending.remove(device_id)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::PlaybackOps;
    use crate::translate::HapticPoint;
    use async_trait::async_trait;
    use tokio::sync::Mutex as TokioMutex;

    #[derive(Default)]
    struct FakePlayback {
        scheduled: TokioMutex<Vec<(String, usize)>>,
        stopped: TokioMutex<Vec<String>>,
        stopped_all: TokioMutex<bool>,
    }

    #[async_trait]
    impl PlaybackOps for FakePlayback {
        async fn schedule_sequence(&self, device_id: String, points: Vec<HapticPoint>) {
            self.scheduled.lock().await.push((device_id, points.len()));
        }
        async fn stop(&self, device_id: &str) {
            self.stopped.lock().await.push(device_id.to_string());
        }
        async fn stop_all(&self) {
            *self.stopped_all.lock().await = true;
        }
    }

    fn test_app(fake: Arc<FakePlayback>) -> App {
        let (ff_tx, _ff_rx) = mpsc::channel(1);
        App::new(fake, ff_tx)
    }

    // FF_RUMBLE with nonzero magnitude — unlike dummy_effect() (kind: 0),
    // translate::translate() produces points for this one, so process_play
    // actually reaches schedule_sequence. `id` must match the effect_id a
    // test plays, since Play lookup matches on the stored effect's own `id`
    // field (see handle_ff_event), not the effect_store map key.
    fn rumble_effect(id: i16) -> FfEffect {
        FfEffect {
            kind: linux_game_haptics_router_common::FF_RUMBLE,
            id,
            direction: 0,
            trigger_button: 0,
            trigger_interval: 0,
            replay_length: 100,
            replay_delay: 0,
            u: [0xffffu16, 0xffff, 0, 0, 0, 0, 0],
        }
    }

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

    fn pending_play(effect_id: i16) -> PendingPlay {
        PendingPlay {
            effect_id,
            issued_at: std::time::Instant::now(),
        }
    }

    #[test]
    fn insert_pending_overwrites_existing_for_same_device() {
        let mut pending = HashMap::new();
        insert_pending(
            &mut pending,
            "dev-a".to_string(),
            5,
            std::time::Instant::now(),
        );
        insert_pending(
            &mut pending,
            "dev-a".to_string(),
            9,
            std::time::Instant::now(),
        );
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

    #[tokio::test]
    async fn play_miss_creates_pending_entry_and_does_not_schedule() {
        let fake = Arc::new(FakePlayback::default());
        let mut app = test_app(fake.clone());
        app.handle_ff_event("dev-a".to_string(), FfEvent::Play { effect_id: 5 })
            .await;
        assert_eq!(app.pending_plays.len(), 1);
        assert!(fake.scheduled.lock().await.is_empty());
    }

    #[tokio::test]
    async fn upload_resolves_pending_play_and_schedules() {
        let fake = Arc::new(FakePlayback::default());
        let mut app = test_app(fake.clone());
        app.handle_ff_event("dev-a".to_string(), FfEvent::Play { effect_id: 5 })
            .await;
        app.handle_effect_uploaded(EffectUploaded {
            tgid: 1,
            effect_id: 5,
            effect: rumble_effect(5),
        })
        .await;
        assert!(app.pending_plays.is_empty());
        let scheduled = fake.scheduled.lock().await;
        assert_eq!(scheduled.len(), 1);
        assert_eq!(scheduled[0].0, "dev-a");
    }

    #[tokio::test]
    async fn upload_resolves_every_device_pending_on_the_same_effect_id() {
        // Regression test for the fix to handle_effect_uploaded: it must
        // resolve every device waiting on this effect_id, not just one.
        let fake = Arc::new(FakePlayback::default());
        let mut app = test_app(fake.clone());
        app.handle_ff_event("dev-a".to_string(), FfEvent::Play { effect_id: 5 })
            .await;
        app.handle_ff_event("dev-b".to_string(), FfEvent::Play { effect_id: 5 })
            .await;
        app.handle_effect_uploaded(EffectUploaded {
            tgid: 1,
            effect_id: 5,
            effect: rumble_effect(5),
        })
        .await;
        assert!(app.pending_plays.is_empty());
        let scheduled = fake.scheduled.lock().await;
        assert_eq!(scheduled.len(), 2);
        let devices: std::collections::HashSet<_> =
            scheduled.iter().map(|(d, _)| d.clone()).collect();
        assert!(devices.contains("dev-a"));
        assert!(devices.contains("dev-b"));
    }

    #[tokio::test]
    async fn stop_clears_pending_play_for_that_device() {
        let fake = Arc::new(FakePlayback::default());
        let mut app = test_app(fake.clone());
        app.handle_ff_event("dev-a".to_string(), FfEvent::Play { effect_id: 5 })
            .await;
        app.handle_ff_event("dev-a".to_string(), FfEvent::Stop { effect_id: 5 })
            .await;
        assert!(app.pending_plays.is_empty());
        // an upload arriving after the stop must not schedule anything —
        // the pending play was cleared, not merely forgotten-but-still-live
        app.handle_effect_uploaded(EffectUploaded {
            tgid: 1,
            effect_id: 5,
            effect: rumble_effect(5),
        })
        .await;
        assert!(fake.scheduled.lock().await.is_empty());
        assert_eq!(fake.stopped.lock().await.as_slice(), ["dev-a"]);
    }

    #[tokio::test]
    async fn erase_clears_pending_play_for_that_effect_id() {
        let fake = Arc::new(FakePlayback::default());
        let mut app = test_app(fake.clone());
        app.handle_ff_event("dev-a".to_string(), FfEvent::Play { effect_id: 5 })
            .await;
        app.handle_effect_erased(1, 5).await;
        assert!(app.pending_plays.is_empty());
    }

    #[tokio::test]
    async fn play_with_immediate_match_processes_without_pending() {
        let fake = Arc::new(FakePlayback::default());
        let mut app = test_app(fake.clone());
        app.handle_effect_uploaded(EffectUploaded {
            tgid: 1,
            effect_id: 5,
            effect: rumble_effect(5),
        })
        .await;
        app.handle_ff_event("dev-a".to_string(), FfEvent::Play { effect_id: 5 })
            .await;
        assert!(app.pending_plays.is_empty());
        assert_eq!(fake.scheduled.lock().await.len(), 1);
    }
}
