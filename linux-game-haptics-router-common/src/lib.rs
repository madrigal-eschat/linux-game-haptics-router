#![cfg_attr(not(feature = "user"), no_std)]
// `bpf_target_arch` is a cfg aya's build sets when cross-compiling this crate
// into the eBPF program (see the KERNEL_FF_EFFECT_SIZE guard below) — it's
// not one rustc knows about ahead of time, so the unexpected-cfg lint would
// otherwise flag every reference to it.
#![allow(unexpected_cfgs)]

/// Waveform types matching Linux FF_* constants
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u16)]
pub enum Waveform {
    Square = 0x58,
    Triangle = 0x59,
    Sine = 0x5a,
    SawUp = 0x5b,
    SawDown = 0x5c,
    Custom = 0x5d,
}

impl Waveform {
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            0x58 => Some(Self::Square),
            0x59 => Some(Self::Triangle),
            0x5a => Some(Self::Sine),
            0x5b => Some(Self::SawUp),
            0x5c => Some(Self::SawDown),
            0x5d => Some(Self::Custom),
            _ => None,
        }
    }
}

/// Envelope applied to periodic/constant/ramp effects
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct Envelope {
    pub attack_length: u16,
    pub attack_level: u16,
    pub fade_length: u16,
    pub fade_level: u16,
}

/// Captured effect data — stored in eBPF map, read by userspace
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct FfEffect {
    pub kind: u16,
    pub id: i16,
    pub direction: u16,
    // trigger (4 bytes)
    pub trigger_button: u16,
    pub trigger_interval: u16,
    // replay (4 bytes)
    pub replay_length: u16,
    pub replay_delay: u16,
    // union — largest variant is periodic (14 bytes)
    pub u: [u16; 7], // raw union bytes as u16 words
}

// FF type constants
pub const FF_RUMBLE: u16 = 0x50;
pub const FF_PERIODIC: u16 = 0x51;
pub const FF_CONSTANT: u16 = 0x52;
pub const FF_RAMP: u16 = 0x57;

/// Scratch entry: saved pointer + effect bytes before the kernel writes back the id.
/// Stored per-thread (keyed by tgid<<32|pid) from enter until exit.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct EnterScratch {
    pub ff_effect_ptr: u64, // userspace pointer passed to EVIOCSFF
    pub effect: FfEffect,
}

// Guarded to the two LP64 targets this has actually been verified against
// (x86_64 via strace: real value is 0x40304580 for size=48). Porting to any
// other target requires re-deriving struct ff_effect's size for that
// target's ABI before trusting this constant.
//
// This crate builds two ways: natively for the userspace ("user" feature),
// where `target_arch` is the real host arch, and as a `no_std` BPF program
// cross-compiled to the virtual `bpfel-unknown-none` target, where
// `target_arch` is "bpf" — aya instead sets its own `bpf_target_arch` cfg to
// the actual host arch being targeted. Check whichever one applies.
#[cfg(all(
    feature = "user",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
compile_error!(
    "KERNEL_FF_EFFECT_SIZE=48 has only been verified for x86_64/aarch64 (LP64, natural \
     alignment) — re-derive struct ff_effect's real size for this target before adding it \
     to this cfg allowlist."
);
#[cfg(all(
    not(feature = "user"),
    not(any(bpf_target_arch = "x86_64", bpf_target_arch = "aarch64"))
))]
compile_error!(
    "KERNEL_FF_EFFECT_SIZE=48 has only been verified for x86_64/aarch64 (LP64, natural \
     alignment) — re-derive struct ff_effect's real size for this bpf_target_arch before \
     adding it to this cfg allowlist."
);

/// Real size of the kernel's `struct ff_effect` under the LP64 data model
/// (48 bytes: the `union { ... }` member holds a `__s16 __user *custom_data`
/// pointer inside `ff_periodic_effect`, forcing 8-byte union alignment/
/// padding). Natural alignment for LP64 (8-byte pointers, 4-byte `u32`,
/// 2-byte fields, no arch-specific packing) is identical on x86_64 and
/// aarch64, so this one constant is correct for both — it is NOT an
/// x86_64-only number. This is also NOT the same as `size_of::<FfEffect>()`
/// — `FfEffect` above is our own compact capture struct, not a copy of the
/// kernel layout. EVIOCSFF's ioctl number encodes the *kernel's* struct
/// size, so we must use the kernel's real size here or the computed command
/// number won't match what userspace actually issues.
const KERNEL_FF_EFFECT_SIZE: u32 = 48;

// Linux ioctl number encoding (see uapi `asm-generic/ioctl.h`): a 32-bit
// value packed as `dir:2 | size:14 | type:8 | nr:8`, built by the kernel's
// `_IOC`/`_IOW` macros. The four pieces below name every field that goes
// into that packing so `EVIOCSFF_NR`/`EVIOCRMFF_NR` don't spell out raw
// hex/shift magic.

/// `_IOC_WRITE` direction bit (bit 30): userspace writes data to the
/// kernel via this ioctl (both EVIOCSFF and EVIOCRMFF do).
const IOC_DIR_WRITE: u32 = 1u32 << 30;

/// Mask for the 14-bit `size` field (bits 16..30) that `_IOC` packs the
/// ioctl's argument size into.
const IOC_SIZE_MASK: u32 = 0x3fff;

/// `_IOC_TYPE` magic byte (bits 8..16) both evdev FF ioctls share: ASCII
/// `'E'`, the "type" the kernel groups all evdev ioctls under.
const IOC_TYPE_EVDEV: u32 = 0x45;

/// `_IOC_NR` sequence byte (bits 0..8) the kernel assigned to `EVIOCSFF`
/// in `uapi/linux/input.h`.
const EVIOCSFF_NR_BYTE: u32 = 0x80;

/// `_IOC_NR` sequence byte (bits 0..8) the kernel assigned to `EVIOCRMFF`
/// in `uapi/linux/input.h`.
const EVIOCRMFF_NR_BYTE: u32 = 0x81;

/// `EVIOCRMFF`'s argument is a plain `int` (the effect id itself, not a
/// pointer — see `uapi/linux/input.h`'s `#define EVIOCRMFF _IOW('E', 0x81,
/// int)`), so its encoded size is `sizeof(c_int)`, not a struct size.
const EVIOCRMFF_ARG_SIZE: u32 = 4;

/// EVIOCSFF ioctl number.
/// #define EVIOCSFF _IOC(_IOC_WRITE, 'E', 0x80, sizeof(struct ff_effect))
/// = (1<<30) | (size<<16) | ('E'<<8) | 0x80
/// (verified against a live strace: real value is 0x40304580 for size=48 —
/// confirms 'E'=0x45 and size=48 were already right; only the nr byte
/// (0x80, not 0x52) was wrong)
pub const EVIOCSFF_NR: u32 = IOC_DIR_WRITE
    | ((KERNEL_FF_EFFECT_SIZE & IOC_SIZE_MASK) << 16)
    | (IOC_TYPE_EVDEV << 8)
    | EVIOCSFF_NR_BYTE;

/// EVIOCRMFF ioctl number.
/// #define EVIOCRMFF _IOW('E', 0x81, int)
/// = (1<<30) | (size_of::<i32>()<<16) | ('E'<<8) | 0x81
pub const EVIOCRMFF_NR: u32 = IOC_DIR_WRITE
    | ((EVIOCRMFF_ARG_SIZE & IOC_SIZE_MASK) << 16)
    | (IOC_TYPE_EVDEV << 8)
    | EVIOCRMFF_NR_BYTE;

/// `ProbeEvent.kind` discriminant: this event is a freshly-uploaded effect
/// (the existing, original event shape — `effect` is meaningful).
pub const PROBE_EVENT_KIND_UPLOADED: u8 = 0;
/// `ProbeEvent.kind` discriminant: this event is an erased effect (freed via
/// `EVIOCRMFF`) — `effect` is unused/zeroed, only `tgid`/`effect_id` matter.
pub const PROBE_EVENT_KIND_ERASED: u8 = 1;

/// Event emitted from eBPF ring buffer to userspace. `kind` distinguishes
/// an upload (`PROBE_EVENT_KIND_UPLOADED`, `effect` meaningful) from an
/// erase (`PROBE_EVENT_KIND_ERASED`, `effect` zeroed/unused).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ProbeEvent {
    pub kind: u8,
    /// Process group ID of the process that uploaded (or erased) the effect
    pub tgid: u32,
    /// Assigned effect id
    pub effect_id: i16,
    pub _pad: u16,
    pub effect: FfEffect,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waveform_from_u16_round_trips_known_values() {
        assert_eq!(Waveform::from_u16(0x58), Some(Waveform::Square));
        assert_eq!(Waveform::from_u16(0x59), Some(Waveform::Triangle));
        assert_eq!(Waveform::from_u16(0x5a), Some(Waveform::Sine));
        assert_eq!(Waveform::from_u16(0x5b), Some(Waveform::SawUp));
        assert_eq!(Waveform::from_u16(0x5c), Some(Waveform::SawDown));
        assert_eq!(Waveform::from_u16(0x5d), Some(Waveform::Custom));
    }

    #[test]
    fn waveform_from_u16_rejects_unknown_values() {
        assert_eq!(Waveform::from_u16(0), None);
        assert_eq!(Waveform::from_u16(0x57), None); // FF_RAMP, not a waveform
        assert_eq!(Waveform::from_u16(0x5e), None);
        assert_eq!(Waveform::from_u16(u16::MAX), None);
    }

    #[test]
    fn envelope_default_is_zeroed() {
        let e = Envelope::default();
        assert_eq!(e.attack_length, 0);
        assert_eq!(e.attack_level, 0);
        assert_eq!(e.fade_length, 0);
        assert_eq!(e.fade_level, 0);
    }

    // Verified against a live strace of a game issuing EVIOCSFF: real value
    // is 0x40304580 for a 48-byte struct ff_effect (see KERNEL_FF_EFFECT_SIZE
    // doc comment) — this pins that against regressions in the bit-packing.
    #[test]
    fn eviocsff_nr_matches_strace_verified_value() {
        assert_eq!(EVIOCSFF_NR, 0x4030_4580);
    }

    #[test]
    fn ff_type_constants_match_linux_uapi_input_event_codes_h() {
        assert_eq!(FF_RUMBLE, 0x50);
        assert_eq!(FF_PERIODIC, 0x51);
        assert_eq!(FF_CONSTANT, 0x52);
        assert_eq!(FF_RAMP, 0x57);
    }

    #[test]
    fn ff_effect_default_is_zeroed() {
        let e = FfEffect::default();
        assert_eq!(e.kind, 0);
        assert_eq!(e.id, 0);
        assert_eq!(e.direction, 0);
        assert_eq!(e.trigger_button, 0);
        assert_eq!(e.trigger_interval, 0);
        assert_eq!(e.replay_length, 0);
        assert_eq!(e.replay_delay, 0);
        assert_eq!(e.u, [0u16; 7]);
    }

    // Derived from the kernel uapi macro `#define EVIOCRMFF _IOW('E', 0x81, int)`
    // rather than a live strace (unlike eviocsff_nr_matches_strace_verified_value,
    // which was strace-verified against a real game) — re-derive against a live
    // strace of an EVIOCRMFF call if this ever needs re-verifying.
    #[test]
    fn eviocrmff_nr_matches_the_ioc_write_e_0x81_int_macro_definition() {
        assert_eq!(EVIOCRMFF_NR, 0x4004_4581);
    }

    #[test]
    fn probe_event_kind_constants_are_distinct() {
        assert_ne!(PROBE_EVENT_KIND_UPLOADED, PROBE_EVENT_KIND_ERASED);
    }
}
