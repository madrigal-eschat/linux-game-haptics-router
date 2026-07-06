use crate::translate::HapticPoint;
use anyhow::Result;
use buttplug::device::{ClientDeviceCommandValue, ClientDeviceOutputCommand};
use buttplug::{ButtplugClient, ButtplugWebsocketClientTransport};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// evdev device_id -> target buttplug device indices. `None` (missing key,
/// or explicit null) means broadcast to every connected device.
pub type DeviceMap = HashMap<String, Option<Vec<u32>>>;

/// Owns the buttplug connection and all in-flight playback for every
/// haptic-source device. Rust is now solely responsible for scheduling and
/// sending scalar commands — Python only supplies the scale, websocket
/// address, and device map at startup (and live scale updates over stdin).
pub struct Playback {
    client: ButtplugClient,
    device_map: DeviceMap,
    scale_bits: AtomicU32,
    // device_id -> (generation, task handle). generation lets a task tell,
    // when it finishes or is cancelled, whether it's still the current
    // occupant of this device's slot before removing itself — a superseding
    // retrigger must never have its bookkeeping deleted by the task it replaced.
    tasks: Mutex<HashMap<String, (u64, JoinHandle<()>)>>,
    gen_counter: AtomicU64,
}

const STEP_MS: u32 = 25;

fn resolve_targets(device_map: &DeviceMap, device_id: &str) -> Option<Vec<u32>> {
    device_map.get(device_id).cloned().unwrap_or(None)
}

impl Playback {
    pub async fn connect_with_retry(
        ws_url: &str,
        scale: f32,
        device_map: DeviceMap,
    ) -> Result<Arc<Self>> {
        let mut delay = Duration::from_millis(500);
        const MAX_DELAY: Duration = Duration::from_secs(5);
        let client = loop {
            let c = ButtplugClient::new("haptics-probe");
            let transport = ButtplugWebsocketClientTransport::new_insecure_connector(ws_url);
            let connector: buttplug::connector::ButtplugRemoteClientConnector<
                ButtplugWebsocketClientTransport,
            > = buttplug::connector::ButtplugRemoteClientConnector::new(transport);
            match c.connect(connector).await {
                Ok(()) => break c,
                Err(e) => {
                    log::warn!(
                        "buttplug connect to {} failed: {}, retrying in {:?}",
                        ws_url,
                        e,
                        delay
                    );
                    tokio::time::sleep(delay).await;
                    delay = std::cmp::min(delay * 2, MAX_DELAY);
                }
            }
        };
        log::info!("connected to buttplug server at {}", ws_url);
        Ok(Arc::new(Self {
            client,
            device_map,
            scale_bits: AtomicU32::new(scale.to_bits()),
            tasks: Mutex::new(HashMap::new()),
            gen_counter: AtomicU64::new(0),
        }))
    }

    pub fn set_scale(&self, scale: f32) {
        self.scale_bits.store(scale.to_bits(), Ordering::Relaxed);
    }

    fn scale(&self) -> f32 {
        f32::from_bits(self.scale_bits.load(Ordering::Relaxed))
    }

    /// Always hit each point's exact wall-time boundary; fill the gaps
    /// between boundaries with linearly-interpolated samples every STEP_MS
    /// so ramps read smoothly instead of jumping between sparse points.
    fn interpolate_points(points: &[HapticPoint]) -> Vec<(u32, f32)> {
        let mut schedule = Vec::new();
        let mut prev: Option<(u32, f32)> = None;
        for p in points {
            if let Some((prev_dt, prev_i)) = prev {
                let span = p.dt_ms.saturating_sub(prev_dt);
                if span > 0 {
                    let mut t = prev_dt + STEP_MS;
                    while t < p.dt_ms {
                        let frac = (t - prev_dt) as f32 / span as f32;
                        schedule.push((t, prev_i + (p.intensity - prev_i) * frac));
                        t += STEP_MS;
                    }
                }
            }
            schedule.push((p.dt_ms, p.intensity));
            prev = Some((p.dt_ms, p.intensity));
        }
        schedule
    }

    fn format_schedule_log(
        device_id: &str,
        points: &[HapticPoint],
        schedule: &[(u32, f32)],
    ) -> String {
        let boundaries = points
            .iter()
            .map(|p| format!("{}ms:{:.2}", p.dt_ms, p.intensity))
            .collect::<Vec<_>>()
            .join(" -> ");
        let keyframes = schedule
            .iter()
            .map(|(t, i)| format!("{}ms:{:.2}", t, i))
            .collect::<Vec<_>>()
            .join(" ");
        let duration_ms = schedule.last().map(|(t, _)| *t).unwrap_or(0);
        format!(
            "play device={:?}: {} boundary point(s), {} keyframe(s) over {}ms\n  boundaries: {}\n  keyframes:  {}",
            device_id, points.len(), schedule.len(), duration_ms, boundaries, keyframes
        )
    }

    /// Sends to every targeted device concurrently — a device stalled on I/O
    /// must not delay delivery to the others sharing this tick.
    async fn send_scalar(client: &ButtplugClient, targets: &Option<Vec<u32>>, intensity: f32) {
        let intensity = intensity.clamp(0.0, 1.0);
        let cmd =
            ClientDeviceOutputCommand::Vibrate(ClientDeviceCommandValue::Percent(intensity as f64));
        let mut sends = tokio::task::JoinSet::new();
        for (idx, dev) in client.devices() {
            if let Some(t) = targets {
                if !t.contains(&idx) {
                    continue;
                }
            }
            let cmd = cmd.clone();
            sends.spawn(async move {
                if let Err(e) = dev.run_output(&cmd).await {
                    log::warn!(
                        "scalar command failed for device {} ({}): {}",
                        idx,
                        dev.name(),
                        e
                    );
                }
            });
        }
        while sends.join_next().await.is_some() {}
    }

    /// Cancel any sequence already running for this device_id and start a
    /// new one from the translated effect points.
    pub async fn schedule_sequence(self: &Arc<Self>, device_id: String, points: Vec<HapticPoint>) {
        let gen = self.gen_counter.fetch_add(1, Ordering::Relaxed) + 1;
        let this = self.clone();
        let task_device_id = device_id.clone();
        let handle = tokio::spawn(async move {
            this.play_sequence(task_device_id, points, gen).await;
        });
        let mut tasks = self.tasks.lock().await;
        if let Some((_, old_handle)) = tasks.insert(device_id, (gen, handle)) {
            old_handle.abort();
        }
    }

    async fn play_sequence(self: Arc<Self>, device_id: String, points: Vec<HapticPoint>, gen: u64) {
        let schedule = Self::interpolate_points(&points);
        log::info!(
            "{}",
            Self::format_schedule_log(&device_id, &points, &schedule)
        );
        let targets = resolve_targets(&self.device_map, &device_id);
        let start = tokio::time::Instant::now();
        for (t_ms, intensity) in &schedule {
            tokio::time::sleep_until(start + Duration::from_millis(*t_ms as u64)).await;
            Self::send_scalar(&self.client, &targets, *intensity * self.scale()).await;
        }
        let mut tasks = self.tasks.lock().await;
        if matches!(tasks.get(&device_id), Some((cur_gen, _)) if *cur_gen == gen) {
            tasks.remove(&device_id);
        }
    }

    pub async fn stop(&self, device_id: &str) {
        let mut tasks = self.tasks.lock().await;
        if let Some((_, handle)) = tasks.remove(device_id) {
            handle.abort();
        }
        drop(tasks);
        let targets = resolve_targets(&self.device_map, device_id);
        Self::send_scalar(&self.client, &targets, 0.0).await;
    }

    pub async fn stop_all(&self) {
        let mut tasks = self.tasks.lock().await;
        for (_, (_, handle)) in tasks.drain() {
            handle.abort();
        }
        drop(tasks);
        if let Err(e) = self.client.stop_all_devices().await {
            log::warn!("stop_all_devices failed: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(dt_ms: u32, intensity: f32) -> HapticPoint {
        HapticPoint { dt_ms, intensity }
    }

    // ── interpolate_points ──

    #[test]
    fn single_point_produces_one_keyframe() {
        let schedule = Playback::interpolate_points(&[pt(0, 0.5)]);
        assert_eq!(schedule, vec![(0, 0.5)]);
    }

    #[test]
    fn two_points_evenly_divisible_span_fills_every_step() {
        // 0..100ms in steps of 25 -> boundaries at 0,25,50,75,100
        let schedule = Playback::interpolate_points(&[pt(0, 1.0), pt(100, 0.0)]);
        let times: Vec<u32> = schedule.iter().map(|(t, _)| *t).collect();
        assert_eq!(times, vec![0, 25, 50, 75, 100]);
        // linear ramp down from 1.0 to 0.0
        for (t, i) in &schedule {
            let expected = 1.0 - (*t as f32 / 100.0);
            assert!(
                (i - expected).abs() < 1e-6,
                "t={t} i={i} expected={expected}"
            );
        }
    }

    #[test]
    fn span_not_a_multiple_of_step_still_hits_exact_end_boundary() {
        // 0..40ms: interpolated steps at 25, then the real boundary at 40
        // (not 50) must still appear exactly, not be skipped or overshot.
        let schedule = Playback::interpolate_points(&[pt(0, 0.0), pt(40, 1.0)]);
        let times: Vec<u32> = schedule.iter().map(|(t, _)| *t).collect();
        assert_eq!(times, vec![0, 25, 40]);
        let last = schedule.last().unwrap();
        assert_eq!(last.0, 40);
        assert!((last.1 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn zero_length_span_between_points_emits_no_interpolated_samples() {
        // two boundaries at the same dt_ms (instantaneous jump) must not
        // divide-by-zero or loop forever.
        let schedule = Playback::interpolate_points(&[pt(0, 0.0), pt(0, 1.0), pt(50, 0.0)]);
        let times: Vec<u32> = schedule.iter().map(|(t, _)| *t).collect();
        assert_eq!(times, vec![0, 0, 25, 50]);
    }

    #[test]
    fn multi_segment_ramp_up_then_down() {
        let schedule = Playback::interpolate_points(&[pt(0, 0.0), pt(50, 1.0), pt(100, 0.0)]);
        let times: Vec<u32> = schedule.iter().map(|(t, _)| *t).collect();
        assert_eq!(times, vec![0, 25, 50, 75, 100]);
        let by_t: HashMap<u32, f32> = schedule.into_iter().collect();
        assert!((by_t[&0] - 0.0).abs() < 1e-6);
        assert!((by_t[&25] - 0.5).abs() < 1e-6);
        assert!((by_t[&50] - 1.0).abs() < 1e-6);
        assert!((by_t[&75] - 0.5).abs() < 1e-6);
        assert!((by_t[&100] - 0.0).abs() < 1e-6);
    }

    #[test]
    fn empty_points_produce_empty_schedule() {
        let schedule = Playback::interpolate_points(&[]);
        assert!(schedule.is_empty());
    }

    // ── format_schedule_log ──

    #[test]
    fn format_schedule_log_includes_boundaries_and_keyframes() {
        let points = vec![pt(0, 0.5), pt(100, 0.0)];
        let schedule = Playback::interpolate_points(&points);
        let log = Playback::format_schedule_log("dev-a", &points, &schedule);
        assert!(log.contains("dev-a"));
        assert!(log.contains("2 boundary point(s)"));
        assert!(log.contains(&format!("{} keyframe(s)", schedule.len())));
        assert!(log.contains("0ms:0.50 -> 100ms:0.00"));
        assert!(log.contains("100ms:0.00"));
    }

    #[test]
    fn format_schedule_log_empty_schedule_reports_zero_duration() {
        let log = Playback::format_schedule_log("dev-a", &[], &[]);
        assert!(log.contains("over 0ms"));
    }

    // ── resolve_targets (device_map semantics) ──

    #[test]
    fn missing_device_id_means_broadcast_to_all() {
        let map: DeviceMap = HashMap::new();
        assert_eq!(resolve_targets(&map, "unknown"), None);
    }

    #[test]
    fn explicit_null_means_broadcast_to_all() {
        let mut map: DeviceMap = HashMap::new();
        map.insert("dev-a".to_string(), None);
        assert_eq!(resolve_targets(&map, "dev-a"), None);
    }

    #[test]
    fn explicit_list_targets_only_those_indices() {
        let mut map: DeviceMap = HashMap::new();
        map.insert("dev-a".to_string(), Some(vec![1, 2]));
        assert_eq!(resolve_targets(&map, "dev-a"), Some(vec![1, 2]));
    }

    #[test]
    fn device_map_deserializes_from_json() {
        let json = r#"{"dev-a": [0, 1], "dev-b": null}"#;
        let map: DeviceMap = serde_json::from_str(json).unwrap();
        assert_eq!(map.get("dev-a").unwrap(), &Some(vec![0, 1]));
        assert_eq!(map.get("dev-b").unwrap(), &None);
    }
}
