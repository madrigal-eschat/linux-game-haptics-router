use std::time::{Duration, Instant};

pub const MIN_INTERVAL_MS: u64 = 10;

pub struct Throttle {
    last_haptic: Option<Instant>,
}

impl Throttle {
    pub fn new() -> Self {
        Self { last_haptic: None }
    }

    pub fn should_emit_haptic(&mut self) -> bool {
        match self.last_haptic {
            None => true,
            Some(t) => t.elapsed() >= Duration::from_millis(MIN_INTERVAL_MS),
        }
    }

    pub fn record_haptic_emitted(&mut self) {
        self.last_haptic = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn first_haptic_always_emitted() {
        let mut t = Throttle::new();
        assert!(t.should_emit_haptic());
    }

    #[test]
    fn second_haptic_blocked_within_10ms() {
        let mut t = Throttle::new();
        t.record_haptic_emitted();
        assert!(!t.should_emit_haptic());
    }

    #[test]
    fn haptic_allowed_after_10ms() {
        let mut t = Throttle::new();
        t.record_haptic_emitted();
        sleep(Duration::from_millis(11));
        assert!(t.should_emit_haptic());
    }

    #[test]
    fn stop_always_bypasses_throttle() {
        let mut t = Throttle::new();
        t.record_haptic_emitted();
        assert!(!t.should_emit_haptic());
    }
}
