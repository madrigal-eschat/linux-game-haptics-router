use crate::protocol_server::ReceivedCommand;
use std::fmt;
use std::time::{Duration, Instant};

pub const LATENCY_BOUND: Duration = Duration::from_millis(150);

#[derive(Debug)]
pub struct TimingError(pub String);

impl fmt::Display for TimingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for TimingError {}

pub fn expected_end_time(issued_at: Instant, replay_length_ms: u16) -> Instant {
    issued_at + Duration::from_millis(replay_length_ms as u64)
}

pub fn assert_command_within_bound(
    issued_at: Instant,
    commands: &[ReceivedCommand],
) -> Result<&ReceivedCommand, TimingError> {
    commands
        .iter()
        .find(|c| c.at.saturating_duration_since(issued_at) <= LATENCY_BOUND)
        .ok_or_else(|| {
            let actual = commands
                .first()
                .map(|c| format!("{:?}", c.at.saturating_duration_since(issued_at)))
                .unwrap_or_else(|| "no command received at all".to_string());
            TimingError(format!(
                "no command arrived within {:?} of issue (actual: {})",
                LATENCY_BOUND, actual
            ))
        })
}

pub fn assert_final_zero_within_bound(
    expected_end: Instant,
    commands: &[ReceivedCommand],
) -> Result<(), TimingError> {
    let last = commands
        .last()
        .ok_or_else(|| TimingError("no commands received; cannot check final stop".to_string()))?;
    if last.value != 0 {
        return Err(TimingError(format!(
            "final command value was {}, expected 0 (stop)",
            last.value
        )));
    }
    let delta = if last.at >= expected_end {
        last.at - expected_end
    } else {
        expected_end - last.at
    };
    if delta > LATENCY_BOUND {
        return Err(TimingError(format!(
            "final stop command arrived {:?} from expected end time (bound {:?})",
            delta, LATENCY_BOUND
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(value: i32, at: Instant) -> ReceivedCommand {
        ReceivedCommand {
            device_index: 0,
            feature_index: 0,
            value,
            at,
        }
    }

    #[test]
    fn command_within_bound_is_found() {
        let issued = Instant::now();
        let commands = vec![cmd(50, issued + Duration::from_millis(50))];
        assert!(assert_command_within_bound(issued, &commands).is_ok());
    }

    #[test]
    fn command_outside_bound_is_rejected() {
        let issued = Instant::now();
        let commands = vec![cmd(50, issued + Duration::from_millis(200))];
        let err = assert_command_within_bound(issued, &commands).unwrap_err();
        assert!(err.0.contains("150ms"));
    }

    #[test]
    fn no_commands_at_all_is_rejected() {
        let issued = Instant::now();
        let err = assert_command_within_bound(issued, &[]).unwrap_err();
        assert!(err.0.contains("no command received at all"));
    }

    #[test]
    fn final_zero_within_bound_passes() {
        let issued = Instant::now();
        let expected_end = expected_end_time(issued, 500);
        let commands = vec![
            cmd(80, issued + Duration::from_millis(10)),
            cmd(0, expected_end + Duration::from_millis(20)),
        ];
        assert!(assert_final_zero_within_bound(expected_end, &commands).is_ok());
    }

    #[test]
    fn final_command_nonzero_is_rejected() {
        let issued = Instant::now();
        let expected_end = expected_end_time(issued, 500);
        let commands = vec![cmd(80, expected_end)];
        let err = assert_final_zero_within_bound(expected_end, &commands).unwrap_err();
        assert!(err.0.contains("expected 0"));
    }

    #[test]
    fn final_zero_outside_bound_is_rejected() {
        let issued = Instant::now();
        let expected_end = expected_end_time(issued, 500);
        let commands = vec![cmd(0, expected_end + Duration::from_millis(300))];
        let err = assert_final_zero_within_bound(expected_end, &commands).unwrap_err();
        assert!(err.0.contains("bound"));
    }

    #[test]
    fn expected_end_time_adds_replay_length_ms() {
        let issued = Instant::now();
        let end = expected_end_time(issued, 500);
        assert_eq!(end - issued, Duration::from_millis(500));
    }
}
