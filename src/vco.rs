//! The analog dual-oscillator core shared by the Analog Rytm MKII's BD, SD and
//! RS/CP voices (`analog.rs`) and the Syntakt's analog drum voices
//! (`stanalog.rs`): two VCOs, each with a waveform selector, and three switches
//! between them: a phase reset on the trig, linear FM of oscillator 1 by
//! oscillator 2, and a ring modulator.
//!
//! Switches [measured on both units by sweeping every machine's waveform and
//! modulation settings and diffing the control frame; docs/SYNTAKT-PORT.md
//! section 13, docs/RYTM-PORT.md]:
//! - each oscillator's waveform is a 3-bit code (`Wave::from_bits`, in the
//!   Syntakt's bit order a, b, c; the Rytm sends the same code with its
//!   switch-table indices 2, 3, 6 carrying c, b, a). The Rytm's oscillator 1
//!   has only the c bit (sine or sinesaw);
//! - FM on: oscillator 2's level word becomes the modulation depth (BD FM's FM
//!   A, BD PLASTIC's MOD, a fixed 0x0a0b for SY DUAL VCO's FM settings), and
//!   oscillator 2 is no longer heard;
//! - ring modulation (active low on both units): oscillator 1 times a square
//!   from oscillator 2 (SY RAW's manual: "oscillator 2 ring modulates
//!   oscillator 1 using a square wave, even if you set LEV2 to 0"), with
//!   oscillator 2 still mixed at its level. Both units make BD SHARP's and BD
//!   ACOUSTIC's "square" this way: ring on, oscillator 1's frequency word 0, its
//!   level up, oscillator 2's down. So a stopped oscillator 1 must give a
//!   constant, taken here as +1 [inferred];
//! - no sync: every switch bit of the three voices is accounted for by the
//!   reset, waveform, FM and ring switches, and no machine offers sync.
//!
//! How each of these sounds is [inferred]: the shapes below, the FM law and its
//! scale. FM is linear through the core: the instantaneous frequency is
//! f1 x (1 + depth x osc2), so the deviation follows the note and the index
//! falls as oscillator 2 rises. It is taken as through-zero (the core runs
//! backwards where that sum is negative), which keeps the carrier on the note
//! at any depth; a core that stops at zero instead would bend the pitch up at
//! high depth, which may be what the Syntakt manual means by "Does not pitch
//! track 100% correct" for its FM settings. Depth 1.0 is `FM_UNITS` in the
//! level word, which keeps the Rytm's default BD FM (oscillator 2 about 1.1x
//! oscillator 1) near the index the earlier phase-modulation model gave it. A
//! recording of a real BD FM at a few FM A and FM T settings would settle the
//! scale, the zero crossing and the depth's envelope.

use std::f32::consts::TAU;

/// FM depth: units of oscillator 2's level word per 1.0 of relative frequency
/// deviation [inferred]. The Rytm's and the Syntakt's BD FM write the same
/// word for the same FM A (0x0cc5 at the default) [measured], so one scale
/// serves both.
pub const FM_UNITS: f32 = 4096.0;

/// An oscillator waveform.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wave {
    /// Code 000 (SY CHIP's noise waves turn both oscillators off with it on the
    /// Syntakt; one of the Rytm SY RAW's WAV1 settings sends it) [measured].
    Off,
    Sine,
    AsymSine,
    Triangle,
    SineSaw,
    Saw,
    Square,
}

impl Wave {
    /// The waveform of the bits (a, b, c) [measured: BD SHARP's and SY RAW's
    /// lists of sine, asymmetric sine, triangle, sinesaw, saw and square in
    /// order give 101, 001, 111, 100, 110, 011]. 010 is never sent.
    pub fn from_bits(a: bool, b: bool, c: bool) -> Wave {
        match (a, b, c) {
            (true, false, true) => Wave::Sine,
            (false, false, true) => Wave::AsymSine,
            (true, true, true) => Wave::Triangle,
            (true, false, false) => Wave::SineSaw,
            (true, true, false) => Wave::Saw,
            (false, true, true) => Wave::Square,
            _ => Wave::Off,
        }
    }

    /// The waveform at phase `ph` (0..1): every shape crosses zero rising at
    /// phase 0, where the trig resets the core [inferred shapes].
    pub fn at(self, ph: f32) -> f32 {
        match self {
            Wave::Off => 0.0,
            Wave::Sine => (TAU * ph).sin(),
            // A sine whose first half-cycle takes 35 % of the period.
            Wave::AsymSine => {
                let x = if ph < 0.35 { ph / 0.7 } else { 0.5 + (ph - 0.35) / 1.3 };
                (TAU * x).sin()
            }
            Wave::Triangle => {
                let x = (ph + 0.25).fract();
                1.0 - 4.0 * (x - 0.5).abs()
            }
            // The Rytm model's "sinesaw": a sine with some saw added.
            Wave::SineSaw => 0.7 * (TAU * ph).sin() + 0.3 * (2.0 * ph - 1.0),
            Wave::Saw => 2.0 * (ph + 0.5).fract() - 1.0,
            Wave::Square => {
                if ph < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
        }
    }
}

/// One audio frame's settings of a dual-oscillator voice, decoded from the
/// control frame by its owner.
#[derive(Clone, Copy, Debug)]
pub struct Osc2Set {
    /// Each oscillator's frequency in Hz (sweeps, detune and drift included).
    pub hz: [f32; 2],
    /// Each oscillator's frequency word is 0: the core is stopped.
    pub stopped: [bool; 2],
    pub wave: [Wave; 2],
    /// Oscillator levels, 1.0 nominal.
    pub level: [f32; 2],
    /// Oscillator 2's level word, raw (the FM depth when FM is on).
    pub depth_word: f32,
    pub fm: bool,
    pub ring: bool,
}

/// The pair's state: the two cores' phases.
#[derive(Clone, Copy, Default, Debug)]
pub struct Pair {
    pub ph: [f32; 2],
}

impl Pair {
    pub fn reset(&mut self) {
        self.ph = [0.0; 2];
    }

    /// One sample at `fs`: oscillator 1 (FM'd, ring-modulated) times its level,
    /// plus oscillator 2 times its level unless it is the modulator.
    pub fn sample(&mut self, s: &Osc2Set, fs: f32) -> f32 {
        let o2 = if s.stopped[1] { 1.0 } else { s.wave[1].at(self.ph[1]) };
        let mut o1 = if s.stopped[0] { 1.0 } else { s.wave[0].at(self.ph[0]) };
        if s.ring {
            // The square from oscillator 2's core, whatever its waveform.
            o1 *= if s.stopped[1] || self.ph[1] < 0.5 { 1.0 } else { -1.0 };
        }
        let mut f1 = s.hz[0].min(fs * 0.45);
        if s.fm {
            f1 *= 1.0 + s.depth_word / FM_UNITS * o2;
        }
        if !s.stopped[0] {
            self.ph[0] = (self.ph[0] + f1 / fs).rem_euclid(1.0);
        }
        if !s.stopped[1] {
            self.ph[1] = (self.ph[1] + s.hz[1].min(fs * 0.45) / fs).fract();
        }
        o1 * s.level[0] + if s.fm { 0.0 } else { o2 * s.level[1] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f32 = 48_000.0;

    fn set(fm: bool, ring: bool, depth: f32) -> Osc2Set {
        Osc2Set { hz: [200.0, 300.0], stopped: [false; 2], wave: [Wave::Sine; 2], level: [1.0, 0.0], depth_word: depth, fm, ring }
    }

    /// Magnitude of the DFT bin at `hz` over one second of `x`.
    fn bin(x: &[f32], hz: f32) -> f32 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (n, v) in x.iter().enumerate() {
            let a = (TAU * hz * n as f32 / FS) as f64;
            re += *v as f64 * a.cos();
            im += *v as f64 * a.sin();
        }
        ((re * re + im * im).sqrt() / x.len() as f64 * 2.0) as f32
    }

    fn render(s: &Osc2Set) -> Vec<f32> {
        let mut p = Pair::default();
        (0..FS as usize).map(|_| p.sample(s, FS)).collect()
    }

    #[test]
    fn codes() {
        let all: Vec<Wave> = (0..8).map(|c| Wave::from_bits(c & 4 != 0, c & 2 != 0, c & 1 != 0)).collect();
        assert_eq!(all, [Wave::Off, Wave::AsymSine, Wave::Off, Wave::Square, Wave::SineSaw, Wave::Sine, Wave::Saw, Wave::Triangle]);
        for w in all {
            for k in 0..100 {
                assert!(w.at(k as f32 / 100.0).abs() <= 1.0001);
            }
        }
    }

    #[test]
    fn fm_sidebands_grow_with_depth() {
        let side = |d: f32| {
            let x = render(&set(true, false, d));
            bin(&x, 500.0) + bin(&x, 800.0)
        };
        let (a, b, c) = (side(0.0), side(1000.0), side(4000.0));
        assert!(a < 1e-3 && b > 0.05 && c > b, "{a} {b} {c}");
        // The carrier stays at 200 Hz.
        let x = render(&set(true, false, 1000.0));
        assert!(bin(&x, 200.0) > 0.5);
    }

    #[test]
    fn ring_gives_sum_and_difference() {
        let x = render(&set(false, true, 0.0));
        // sine(200) x square(300): 300 +- 200 = 100 and 500, no 200.
        assert!(bin(&x, 100.0) > 0.3 && bin(&x, 500.0) > 0.3, "{} {}", bin(&x, 100.0), bin(&x, 500.0));
        assert!(bin(&x, 200.0) < 0.01);
    }

    #[test]
    fn stopped_osc1_ring_is_a_square() {
        let mut s = set(false, true, 0.0);
        s.stopped[0] = true;
        let x = render(&s);
        assert!((bin(&x, 300.0) - 4.0 / std::f32::consts::PI).abs() < 0.05);
    }
}
