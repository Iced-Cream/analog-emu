//! A model of the Analog Four MKII's four analog voices, driven by the voice
//! frame the OS writes every audio block (docs/A4-PORT.md, section 10).
//!
//! The manual (4.1, appendix E): two analog oscillators with sub oscillators,
//! a digital noise generator, the ladder filter, overdrive, the multimode
//! filter, VCA and pan, all analog, with digital envelopes and LFOs that the OS
//! applies to the control words. The dry path to the outputs is analog; the
//! digital send effects run in parallel: the OS computes them from the
//! voices' signals, which the codec digitises (`Out::send`), and returns them
//! on the codec's output (bus.rs mixes the return into the dry mix).
//!
//! The frame: 100 big-endian words at 0x80003d6c [measured, by turning knobs and
//! playing notes in the emulator, 2026-10-07]:
//!   w 13v + 0..12, voice v (0..3): osc1/osc2 trims (a linear function of the
//!     oscillator's frequency word with a per-oscillator gain and offset from
//!     the calibration table 0x40218f68; [inferred] the ramp-amplitude CV of an
//!     NCO-reset oscillator, not pitch: not modelled), osc1/osc2 shape (PW),
//!     osc1/osc2 level (inverted: 0x7fff off), ladder cutoff, ladder resonance,
//!     overdrive (0x15b4 at 0), multimode cutoff (inverted), multimode resonance, VCA L, VCA R
//!     (inverted: 0x7fff closed, about 0 open);
//!   w 52 + v: noise level (inverted) [inferred];
//!   w 64 + v: switches: osc1 waveform bits 0-1, osc2 bits 4-5, multimode type
//!     bits 8-10 (the Rytm's codes: LP2 0, BP 1, HP1 2, PK 3, HP2 4, BS 5, LP1 6);
//!   w 68 + 4g, oscillator g = 2v + osc: a 32-bit frequency word (w+0 high, w+1
//!     low), an NCO increment: 2^32 / 24.576 MHz = 174.76 units per Hz, then w+2 (0x8000), w+3 the sub oscillator (0 off).
//! The filter, resonance and overdrive words' ranges are measured against the
//! knobs (see the functions); every curve below (levels linear, cutoffs
//! exponential, waveform codes) is [inferred].

pub const FRAME_ADDR: u32 = 0x8000_3D6C;
pub const FRAME_LEN: usize = 200;
/// A voice's effect input at full scale, in 24-bit codec units (half scale
/// [inferred]).
pub const RX_SCALE: f32 = 4_194_304.0;
/// The effects' return against the dry mix [inferred].
pub const RETURN_GAIN: f32 = 1.0;

const FS: f32 = 48_000.0;
/// The frequency word is a 32-bit NCO increment at 24.576 MHz (512 x 48 kHz):
/// 2^32 / 24.576e6 units per Hz. Fitted [measured]: notes 24..84 average
/// 174.72..174.79 units per Hz (the OS adds an LFO on top), within 0.4 cents.
const UNITS_PER_HZ: f32 = 174.7627;

fn word(f: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([f[2 * i], f[2 * i + 1]])
}

/// An inverted level word as a gain 0..1 (0x7fff closed, 0 and below open).
fn inv_gain(w: u16) -> f32 {
    ((32767.0 - w as i16 as f32) / 32767.0).clamp(0.0, 1.0)
}

/// The ladder's cutoff word as Hz. Measured [2026-10-12, FILTERS page, T1]:
/// the word is linear in the FRQ knob, 257.8 per unit, 0 at 20.15 and 0x6c9b
/// at 127.99 (below 20.15 it clamps at 0). The Hz are [inferred]: one knob
/// unit a semitone (3094 per octave) and the top of the knob about 18 kHz.
fn ladder_hz(w: u16) -> f32 {
    (40.0 * (w as f32 / 3094.0).exp2()).min(20_000.0)
}

/// The multimode filter's cutoff word as Hz. Measured: inverted and linear in
/// the FRQ knob, 144.5 per unit, 0x4a38 at 0 and 0x01f6 at 127.99. The Hz are
/// [inferred] as for the ladder: a semitone a unit (1734 per octave), about
/// 20 kHz at the top. Signed: modulation may push it past either end.
fn multimode_hz(w: u16) -> f32 {
    (20_000.0 * (-(w as i16 as f32 - 502.0) / 1734.0).exp2()).clamp(10.0, 20_000.0)
}

/// The polyBLEP residual for a discontinuity at phase 0 (t, dt in cycles).
fn blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

#[derive(Clone, Copy, Default)]
struct Osc {
    ph: f32,
    sub: f32,
    tri: f32,
}

impl Osc {
    /// One sample: `wave` 0 saw, 1 transistor pulse, 2 pulse, 3 triangle
    /// [inferred codes, the manual's order]; `pw` 0..1 (0.5 the middle).
    fn sample(&mut self, hz: f32, wave: u16, pw: f32, sub: u16) -> f32 {
        let dt = (hz / FS).clamp(0.0, 0.45);
        let pw = pw.clamp(0.02, 0.98);
        let t = self.ph;
        let saw = 2.0 * t - 1.0 - blep(t, dt);
        let pulse = {
            let mut p = if t < pw { 1.0 } else { -1.0 };
            p += blep(t, dt);
            let t2 = (t - pw + 1.0).fract();
            p -= blep(t2, dt);
            p
        };
        let y = match wave & 3 {
            0 => saw,
            1 => 0.6 * pulse + 0.4 * saw, // a pulse with one sloping side
            2 => pulse,
            _ => {
                // An integrated square less its mean (2 pw - 1), leaky to stay
                // centred: a triangle, skewed by PW. Integrating the pulse
                // itself left a DC of 4 dt (2 pw - 1) / 0.001 (-17 at 440 Hz
                // and PW 0.27: the A4 MKI's default kit), which drove the
                // ladder's tanh to a rail; at PW 0.5 the two are the same.
                self.tri = 0.999 * self.tri + 4.0 * dt * (pulse - (2.0 * pw - 1.0));
                self.tri
            }
        };
        let mut out = y;
        if sub != 0 {
            let div = if sub == 2 { 4.0 } else { 2.0 };
            self.sub = (self.sub + dt / div).fract();
            out += 0.6 * if self.sub < 0.5 { 1.0 } else { -1.0 };
        }
        self.ph = (t + dt).fract();
        out
    }
}

#[derive(Clone, Copy, Default)]
struct Voice {
    osc: [Osc; 2],
    ladder: [f32; 4],
    low: f32,
    band: f32,
    lp1: f32,
    /// Smoothed gains: VCA L, VCA R, osc1, osc2, noise.
    g: [f32; 5],
}

/// The multimode filter's type codes (shared with the Rytm's multimode filter).
fn mm(code: u16, x: f32, low: f32, band: f32, high: f32, lp1: f32, k: f32) -> f32 {
    match code & 7 {
        0 => low,
        1 => band,
        2 => x - lp1,
        3 => x + band * k,
        4 => high,
        5 => low + high,
        _ => lp1,
    }
}

pub struct A4Voices {
    v: [Voice; 4],
    noise: u32,
    /// Where the OS keeps the frame: `FRAME_ADDR` on the A4 MKII, 0x800032b0
    /// on the A4 MKI OS 1.56 (`0x400f64be` hands it to the frame builder
    /// `0x40061b08`, which packs the same 100 words) [measured].
    pub frame_addr: u32,
}

/// One codec frame's worth of the model.
pub struct Out {
    /// The dry stereo mix (the analog path to the outputs), -1..1.
    pub mix: (f32, f32),
    /// Each voice's signal before its VCA, -1..1: what the codec digitises for
    /// the effects (RX slots 0..3; docs/A4-PORT.md section 12).
    pub send: [f32; 4],
}

impl Default for A4Voices {
    fn default() -> Self {
        A4Voices { v: [Voice::default(); 4], noise: 0x1234_5678, frame_addr: FRAME_ADDR }
    }
}

impl A4Voices {
    /// One codec frame: the dry mix and the voices' effect inputs.
    pub fn frame(&mut self, f: &[u8]) -> Out {
        let mut mix = (0.0f32, 0.0f32);
        let mut send = [0.0f32; 4];
        for vi in 0..4 {
            let b = 13 * vi;
            let sw = word(f, 64 + vi);
            let target = [
                inv_gain(word(f, b + 11)),
                inv_gain(word(f, b + 12)),
                inv_gain(word(f, b + 4)),
                inv_gain(word(f, b + 5)),
                inv_gain(word(f, 52 + vi)),
            ];
            // White noise (the A4's noise generator is digital).
            self.noise ^= self.noise << 13;
            self.noise ^= self.noise >> 17;
            self.noise ^= self.noise << 5;
            let nz = self.noise as i32 as f32 / 2_147_483_648.0;
            let v = &mut self.v[vi];
            for (g, t) in v.g.iter_mut().zip(target) {
                *g += (t - *g) * 0.02; // about 1 ms
            }
            if v.g[0] < 1e-4 && v.g[1] < 1e-4 && target[0] == 0.0 && target[1] == 0.0 {
                continue; // VCA closed: silent
            }
            let mut src = 0.0;
            for k in 0..2 {
                let g = 68 + 4 * (2 * vi + k);
                let fw = (word(f, g) as u32) << 16 | word(f, g + 1) as u32;
                let hz = fw as f32 / UNITS_PER_HZ;
                let wave = (sw >> (4 * k)) & 3;
                let pw = word(f, b + 2 + k) as f32 / 65535.0;
                src += v.g[2 + k] * v.osc[k].sample(hz, wave, pw, word(f, g + 3));
            }
            src += v.g[4] * nz;
            src *= 0.35;
            // Ladder: four one-pole stages, resonance as feedback from the last.
            let fc = ladder_hz(word(f, b + 6));
            // Resonance: 0x1580 at RES 0, 0xde90 at 127 [measured]; 4 is the
            // feedback where the model self-oscillates [inferred].
            let res = ((word(f, b + 7) as f32 - 5504.0) / 51472.0).clamp(0.0, 1.0) * 4.0;
            let a = 1.0 - (-std::f32::consts::TAU * fc.min(FS * 0.45) / FS).exp();
            let mut x = (src - res * v.ladder[3]).tanh();
            for s in v.ladder.iter_mut() {
                *s += a * (x - *s);
                x = *s;
            }
            // Overdrive between the filters: w8 is the OVR knob, 0x15b4 at 0 and
            // 0xea4b at 127 [measured]; the curve is [inferred].
            let drive = ((word(f, b + 8) as f32 - 5556.0) / 54423.0).clamp(0.0, 1.0);
            let g = 1.0 + 15.0 * drive;
            let x = (v.ladder[3] * g).tanh() / g.sqrt();
            // Multimode: a state-variable filter.
            let fc2 = multimode_hz(word(f, b + 9));
            // Resonance: 0 at RES 0, 0x3f80 at 127 [measured].
            let q = (word(f, b + 10) as f32 / 16256.0).clamp(0.0, 1.0);
            let k = 2.0 * (1.0 - 0.95 * q);
            let gg = (std::f32::consts::PI * fc2.min(FS * 0.45) / FS).tan();
            let a1 = 1.0 / (1.0 + gg * (gg + k));
            let (a2, a3) = (gg * a1, gg * gg * a1);
            let v3 = x - v.low;
            let band = a1 * v.band + a2 * v3;
            let low = v.low + a2 * v.band + a3 * v3;
            v.band = 2.0 * band - v.band;
            v.low = 2.0 * low - v.low;
            let high = x - k * band - low;
            v.lp1 += (1.0 - (-std::f32::consts::TAU * fc2 / FS).exp()) * (x - v.lp1);
            let mut y = mm(sw >> 8, x, low, band, high, v.lp1, k);
            if !y.is_finite() {
                *v = Voice::default();
                y = 0.0;
            }
            send[vi] = y.clamp(-1.0, 1.0);
            mix.0 += y * v.g[0] * 0.5;
            mix.1 += y * v.g[1] * 0.5;
        }
        Out { mix, send }
    }
}
