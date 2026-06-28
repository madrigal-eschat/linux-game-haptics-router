use core::f32::consts::PI;
use haptics_probe_common::{FfEffect, FF_RUMBLE, FF_PERIODIC, FF_CONSTANT, FF_RAMP, Waveform, Envelope};

pub const SAMPLE_INTERVAL_MS: u32 = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct HapticPoint {
    pub dt_ms: u32,
    pub intensity: f32,
}

pub fn translate(effect: &FfEffect) -> Vec<HapticPoint> {
    match effect.kind {
        FF_RUMBLE   => rumble_points(effect),
        FF_PERIODIC => periodic_points(effect),
        FF_CONSTANT => constant_points(effect),
        FF_RAMP     => ramp_points(effect),
        _           => vec![],
    }
}

fn rumble_intensity(strong: u16, weak: u16) -> f32 {
    ((strong as f32 + weak as f32) / 2.0 / 65535.0).clamp(0.0, 1.0)
}

/// Apply envelope scaling to a base intensity at time t_ms within a replay of length_ms.
/// Envelope: linear attack ramp from attack_level to full over attack_length,
///           linear fade ramp from full to fade_level over the last fade_length ms.
fn apply_envelope(base: f32, t_ms: u32, length_ms: u32, env: Envelope) -> f32 {
    let scale = if env.attack_length > 0 && t_ms < env.attack_length as u32 {
        let attack_start = env.attack_level as f32 / 32767.0;
        let progress = t_ms as f32 / env.attack_length as f32;
        attack_start + (1.0 - attack_start) * progress
    } else if env.fade_length > 0 && length_ms > 0 {
        let fade_start_ms = length_ms.saturating_sub(env.fade_length as u32);
        if t_ms >= fade_start_ms {
            let fade_end = env.fade_level as f32 / 32767.0;
            let progress = (t_ms - fade_start_ms) as f32 / env.fade_length as f32;
            1.0 - (1.0 - fade_end) * progress
        } else {
            1.0
        }
    } else {
        1.0
    };
    (base * scale).clamp(0.0, 1.0)
}

fn sample_waveform(waveform: Waveform, t_ms: u32, period_ms: u16,
                   magnitude: i16, offset: i16, _phase: u16) -> f32 {
    if period_ms == 0 { return 0.0; }
    let t = (t_ms % period_ms as u32) as f32 / period_ms as f32;
    let mag = magnitude.abs() as f32 / 32767.0;
    let off = offset as f32 / 32767.0;
    let raw = match waveform {
        Waveform::Sine     => (t * 2.0 * PI).sin(),
        Waveform::Square   => if t < 0.5 { 1.0 } else { -1.0 },
        Waveform::Triangle => if t < 0.5 { 4.0 * t - 1.0 } else { 3.0 - 4.0 * t },
        Waveform::SawUp    => 2.0 * t - 1.0,
        Waveform::SawDown  => 1.0 - 2.0 * t,
        Waveform::Custom   => 0.0,
    };
    ((raw * mag + off) * 0.5 + 0.5).clamp(0.0, 1.0)
}

fn rumble_points(effect: &FfEffect) -> Vec<HapticPoint> {
    let strong = effect.u[0];
    let weak   = effect.u[1];
    let length = effect.replay_length as u32;
    if length == 0 {
        return vec![HapticPoint { dt_ms: 0, intensity: 0.0 }];
    }
    let intensity = rumble_intensity(strong, weak);
    vec![
        HapticPoint { dt_ms: 0, intensity },
        HapticPoint { dt_ms: length, intensity: 0.0 },
    ]
}

fn constant_points(effect: &FfEffect) -> Vec<HapticPoint> {
    let level    = effect.u[0] as i16;
    let length   = effect.replay_length as u32;
    let env = Envelope {
        attack_length: effect.u[1], attack_level: effect.u[2],
        fade_length:   effect.u[3], fade_level:   effect.u[4],
    };
    let base = (level.abs() as f32 / 32767.0).clamp(0.0, 1.0);
    sample_range(0, length, base, length, env)
}

fn ramp_points(effect: &FfEffect) -> Vec<HapticPoint> {
    let start_level = effect.u[0] as i16;
    let end_level   = effect.u[1] as i16;
    let length      = effect.replay_length as u32;
    (0..=length).step_by(SAMPLE_INTERVAL_MS as usize)
        .chain(if length % SAMPLE_INTERVAL_MS == 0 { None } else { Some(length) })
        .map(|t| {
            let progress = if length == 0 { 0.0 } else { t as f32 / length as f32 };
            let level = start_level as f32 + (end_level - start_level) as f32 * progress;
            HapticPoint { dt_ms: t, intensity: (level.abs() / 32767.0).clamp(0.0, 1.0) }
        })
        .chain(std::iter::once(HapticPoint { dt_ms: length, intensity: 0.0 }))
        .collect()
}

fn periodic_points(effect: &FfEffect) -> Vec<HapticPoint> {
    let waveform   = effect.u[0];
    let period_ms  = effect.u[1];
    let magnitude  = effect.u[2] as i16;
    let offset     = effect.u[3] as i16;
    let phase      = effect.u[4];
    let length     = effect.replay_length as u32;
    let env = Envelope {
        attack_length: effect.u[5],
        attack_level:  0,
        fade_length:   effect.u[6],
        fade_level:    0,
    };
    let wf = Waveform::from_u16(waveform).unwrap_or(Waveform::Sine);
    let base_at = |t: u32| sample_waveform(wf, t, period_ms, magnitude, offset, phase);
    sample_range_fn(0, length, base_at, length, env)
}

fn sample_range(start: u32, end: u32, base: f32, length: u32, env: Envelope) -> Vec<HapticPoint> {
    sample_range_fn(start, end, |_| base, length, env)
}

fn sample_range_fn<F: Fn(u32) -> f32>(
    start: u32, end: u32, base_fn: F, length: u32, env: Envelope,
) -> Vec<HapticPoint> {
    let mut pts: Vec<HapticPoint> = (start..end)
        .step_by(SAMPLE_INTERVAL_MS as usize)
        .map(|t| HapticPoint {
            dt_ms: t,
            intensity: apply_envelope(base_fn(t), t, length, env),
        })
        .collect();
    pts.push(HapticPoint { dt_ms: end, intensity: 0.0 });
    pts
}

#[cfg(test)]
mod tests {
    use super::*;
    use haptics_probe_common::FF_RUMBLE;

    fn rumble_effect(strong: u16, weak: u16, length_ms: u16) -> FfEffect {
        let mut e = FfEffect {
            kind: FF_RUMBLE, id: 0, direction: 0,
            trigger_button: 0, trigger_interval: 0,
            replay_length: length_ms, replay_delay: 0,
            u: [0u16; 7],
        };
        e.u[0] = strong;
        e.u[1] = weak;
        e
    }

    fn periodic_effect(waveform: u16, magnitude: i16, period_ms: u16,
                       length_ms: u16, env: Envelope) -> FfEffect {
        let mut e = FfEffect {
            kind: FF_PERIODIC, id: 0, direction: 0,
            trigger_button: 0, trigger_interval: 0,
            replay_length: length_ms, replay_delay: 0,
            u: [0u16; 7],
        };
        e.u[0] = waveform;
        e.u[1] = period_ms;
        e.u[2] = magnitude as u16;
        e.u[3] = 0;
        e.u[4] = 0;
        e.u[5] = env.attack_length;
        e.u[6] = env.fade_length;
        e
    }

    // ── rumble ──

    #[test]
    fn rumble_full_strength_produces_start_and_stop_points() {
        let e = rumble_effect(0xFFFF, 0xFFFF, 500);
        let pts = translate(&e);
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[0].dt_ms, 0);
        assert!((pts[0].intensity - 1.0).abs() < 0.01);
        assert_eq!(pts[1].dt_ms, 500);
        assert_eq!(pts[1].intensity, 0.0);
    }

    #[test]
    fn rumble_half_strength_produces_half_intensity() {
        let e = rumble_effect(0x8000, 0x8000, 200);
        let pts = translate(&e);
        assert!((pts[0].intensity - 0.5).abs() < 0.01);
    }

    #[test]
    fn rumble_combines_strong_and_weak_as_average() {
        let e = rumble_effect(0xFFFF, 0, 100);
        let pts = translate(&e);
        assert!((pts[0].intensity - 0.5).abs() < 0.01);
    }

    #[test]
    fn rumble_zero_length_produces_single_zero_point() {
        let e = rumble_effect(0xFFFF, 0xFFFF, 0);
        let pts = translate(&e);
        assert_eq!(pts.len(), 1);
        assert_eq!(pts[0].intensity, 0.0);
    }

    // ── periodic ──

    #[test]
    fn periodic_sine_samples_at_20ms_intervals() {
        let e = periodic_effect(0x5a, 0x7FFF, 100, 200, Envelope::default());
        let pts = translate(&e);
        assert_eq!(pts.len(), 11);
        assert_eq!(pts[0].dt_ms, 0);
        assert_eq!(pts.last().unwrap().dt_ms, 200);
        assert_eq!(pts.last().unwrap().intensity, 0.0);
    }

    #[test]
    fn periodic_sine_peak_near_quarter_period() {
        let e = periodic_effect(0x5a, 0x7FFF, 40, 200, Envelope::default());
        let pts = translate(&e);
        let pt = pts.iter().find(|p| p.dt_ms == 20).unwrap();
        assert!(pt.intensity > 0.8);
    }

    #[test]
    fn periodic_square_is_binary() {
        let e = periodic_effect(0x58, 0x7FFF, 100, 200, Envelope::default());
        let pts = translate(&e);
        for p in &pts[..pts.len()-1] {
            assert!(p.intensity == 0.0 || (p.intensity - 1.0).abs() < 0.01);
        }
    }

    #[test]
    fn envelope_attack_ramps_up() {
        let env = Envelope { attack_length: 100, attack_level: 0,
                             fade_length: 0, fade_level: 0x7FFF };
        let e = periodic_effect(0x5a, 0x7FFF, 20, 200, env);
        let pts = translate(&e);
        assert!(pts[0].intensity < 0.1);
        let p100 = pts.iter().find(|p| p.dt_ms == 100).unwrap();
        assert!(p100.intensity > 0.8);
    }

    // ── constant ──

    #[test]
    fn constant_produces_flat_points() {
        let mut e = FfEffect {
            kind: FF_CONSTANT, id: 0, direction: 0,
            trigger_button: 0, trigger_interval: 0,
            replay_length: 100, replay_delay: 0,
            u: [0; 7],
        };
        e.u[0] = 0x7FFF;
        let pts = translate(&e);
        let sustain: Vec<_> = pts.iter().filter(|p| p.dt_ms > 0 && p.dt_ms < 100).collect();
        for p in sustain {
            assert!((p.intensity - 1.0).abs() < 0.01);
        }
    }

    // ── unknown type ──

    #[test]
    fn unknown_ff_type_returns_empty() {
        let e = FfEffect { kind: 0xFF, id: 0, direction: 0,
                           trigger_button: 0, trigger_interval: 0,
                           replay_length: 100, replay_delay: 0, u: [0; 7] };
        assert!(translate(&e).is_empty());
    }
}
