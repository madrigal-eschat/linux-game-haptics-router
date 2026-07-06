use evdev::{FFEffectData, FFEffectKind, FFEnvelope, FFReplay, FFTrigger, FFWaveform};

pub struct Scenario {
    pub name: &'static str,
    pub effect: FFEffectData,
    pub expected_end_ms: u16,
}

/// The smoke-set scenarios that each issue exactly one gesture and check
/// exactly one timing pair. Rapid-retrigger and multi-device scenarios are
/// built directly in the e2e-tests orchestrator (Task 6) since they involve
/// more than one gamepad/gesture.
pub fn smoke_scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "ff_rumble",
            effect: FFEffectData {
                direction: 0,
                trigger: FFTrigger::default(),
                replay: FFReplay {
                    length: 300,
                    delay: 0,
                },
                kind: FFEffectKind::Rumble {
                    strong_magnitude: 0xffff,
                    weak_magnitude: 0xffff,
                },
            },
            expected_end_ms: 300,
        },
        Scenario {
            name: "ff_periodic_sine",
            effect: FFEffectData {
                direction: 0,
                trigger: FFTrigger::default(),
                replay: FFReplay {
                    length: 400,
                    delay: 0,
                },
                kind: FFEffectKind::Periodic {
                    waveform: FFWaveform::Sine,
                    period: 100,
                    magnitude: 0x7fff,
                    offset: 0,
                    phase: 0,
                    envelope: FFEnvelope {
                        attack_length: 0,
                        attack_level: 0,
                        fade_length: 0,
                        fade_level: 0,
                    },
                },
            },
            expected_end_ms: 400,
        },
        Scenario {
            name: "ff_constant_with_envelope",
            effect: FFEffectData {
                direction: 0,
                trigger: FFTrigger::default(),
                replay: FFReplay {
                    length: 500,
                    delay: 0,
                },
                kind: FFEffectKind::Constant {
                    level: 0x7fff,
                    envelope: FFEnvelope {
                        attack_length: 100,
                        attack_level: 0,
                        fade_length: 150,
                        fade_level: 0,
                    },
                },
            },
            expected_end_ms: 500,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_scenarios_has_three_distinct_named_cases() {
        let scenarios = smoke_scenarios();
        assert_eq!(scenarios.len(), 3);
        let names: Vec<_> = scenarios.iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["ff_rumble", "ff_periodic_sine", "ff_constant_with_envelope"]);
    }

    #[test]
    fn every_scenario_expected_end_matches_its_replay_length() {
        for s in smoke_scenarios() {
            assert_eq!(s.effect.replay.length, s.expected_end_ms);
        }
    }
}
