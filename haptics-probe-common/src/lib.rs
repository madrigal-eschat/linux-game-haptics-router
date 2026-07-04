#![cfg_attr(not(feature = "user"), no_std)]

/// Waveform types matching Linux FF_* constants
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u16)]
pub enum Waveform {
    Square    = 0x58,
    Triangle  = 0x59,
    Sine      = 0x5a,
    SawUp     = 0x5b,
    SawDown   = 0x5c,
    Custom    = 0x5d,
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
            _    => None,
        }
    }
}

/// Envelope applied to periodic/constant/ramp effects
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct Envelope {
    pub attack_length: u16,
    pub attack_level:  u16,
    pub fade_length:   u16,
    pub fade_level:    u16,
}

/// Captured effect data — stored in eBPF map, read by userspace
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct FfEffect {
    pub kind:      u16,
    pub id:        i16,
    pub direction: u16,
    // trigger (4 bytes)
    pub trigger_button:   u16,
    pub trigger_interval: u16,
    // replay (4 bytes)
    pub replay_length: u16,
    pub replay_delay:  u16,
    // union — largest variant is periodic (14 bytes)
    pub u: [u16; 7],  // raw union bytes as u16 words
}

// FF type constants
pub const FF_RUMBLE:   u16 = 0x50;
pub const FF_PERIODIC: u16 = 0x51;
pub const FF_CONSTANT: u16 = 0x52;
pub const FF_RAMP:     u16 = 0x57;

/// Scratch entry: saved pointer + effect bytes before the kernel writes back the id.
/// Stored per-thread (keyed by tgid<<32|pid) from enter until exit.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct EnterScratch {
    pub ff_effect_ptr: u64,  // userspace pointer passed to EVIOCSFF
    pub effect: FfEffect,
}

/// Real size of the kernel's `struct ff_effect` on x86_64 (48 bytes: the
/// `union { ... }` member holds a `__s16 __user *custom_data` pointer inside
/// `ff_periodic_effect`, forcing 8-byte union alignment/padding). This is
/// NOT the same as `size_of::<FfEffect>()` — `FfEffect` above is our own
/// compact capture struct, not a copy of the kernel layout. EVIOCSFF's
/// ioctl number encodes the *kernel's* struct size, so we must use the
/// kernel's real size here or the computed command number won't match what
/// userspace actually issues.
const KERNEL_FF_EFFECT_SIZE: u32 = 48;

/// Compute EVIOCSFF ioctl number at compile time.
/// #define EVIOCSFF _IOC(_IOC_WRITE, 'E', 0x80, sizeof(struct ff_effect))
/// = (1<<30) | (size<<16) | ('E'<<8) | 0x80
/// (verified against a live strace: real value is 0x40304580 for size=48 —
/// confirms 'E'=0x45 and size=48 were already right; only the nr byte
/// (0x80, not 0x52) was wrong)
pub const fn eviocsff_nr() -> u32 {
    (1u32 << 30) | ((KERNEL_FF_EFFECT_SIZE & 0x3fff) << 16) | (0x45u32 << 8) | 0x80u32
}

/// Event emitted from eBPF ring buffer to userspace
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ProbeEvent {
    /// Process group ID of the process that uploaded the effect
    pub tgid: u32,
    /// Assigned effect id
    pub effect_id: i16,
    pub _pad: u16,
    pub effect: FfEffect,
}
