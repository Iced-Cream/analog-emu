//! The Syntakt's analog side (docs/SYNTAKT-PORT.md section 12): the analog
//! voices of tracks 9-12 and the analog FX block, driven by the 500-word SPI
//! control frame CPU1 sends on DSPI1 (eDMA channel 15), and joined to the
//! firmware through the codec frame.
//!
//! The frame map, from turning every knob of every analog machine in the
//! emulator and diffing the frame (section 12) [measured unless noted]:
//!
//! - voice v (0..2 the analog drum voices of tracks 9..11, 3 the cymbal voice
//!   of track 12): VCA level x pan L at word v, R at word 5 + v; filter cutoff
//!   at 10 + v (`(FREQ * 256 - 0x4000) * 2` plus key tracking); resonance at
//!   14 + v; amp envelope records (L, R) at 84 + 4v and 104 + 4v; filter
//!   envelope record at 124 + 4v (its first word the ENV depth); a switch byte
//!   (v0 word 80 high, v1 word 80 low, v2 word 81 high, v3 word 81 low): filter
//!   type (1 LP, 2 BP, 3 HP, 4 notch, 5 low shelf, 6 peak) in bits 4-6 and the
//!   oscillator nibble in bits 0-3 on the drum voices, the type in bits 0-2 and
//!   the banks' mode in bits 4-5 on the cymbal voice; routing to the FX block:
//!   word 82 bit 12 (v0), word 82 bit 3 (v1), word 83 bits 10 and 11 (v2, v3).
//! - drum voice v: oscillator 1 and 2 frequency at 18 + 4v and 19 + 4v (8 units
//!   a Hz), their levels at 20 + 4v and 21 + 4v (0x1fff nominal, higher with
//!   OVER); records at 140 + 16v: pitch 1, pitch 2, amp 1, amp 2.
//! - cymbal voice: two banks of six 32-bit oscillator fields at 308 and 320
//!   (low 22 bits at 43.62 units a Hz, as on the Rytm), colour at 30, the level
//!   into its overdrive at 34, path records at 192, 196, 200 and its amp record
//!   at 204.
//! - analog FX block: VCA L/R at 35/36, filter cutoff L/R at 37/38, resonance
//!   at 39, type in word 82 bits 8-11 (0xe LP4, 0xd LP2, 0xb BP, 0xa HP, 0x8
//!   notch, 0x9 double notch, 0xc LP + notch, 0xf off), amp records 208 (L) and
//!   212 (R), filter envelope records 216 and 220.
//!
//! Codec [measured, CPU1 code and injected test tones]: receive words 0..3 are
//! tracks 9..12 (CPU1 applies its own copy of the amp envelope and feeds them to
//! the delay and reverb sends), 4/5 the FX block's return, 6/7 the inputs; DAC
//! words 4, 1, 0 and 6 carry the digital layers of tracks 9, 10, 11 and 12 (the
//! transients, noise, impulses), 3/5 the digital main mix and 2/7 the digital
//! tracks routed to the FX block. How each part sounds - waveforms, curves,
//! scales - is [inferred].

#![allow(clippy::needless_range_loop)]

use crate::analog::{Record, LEVELS_PER_6DB};

/// The frame's two buffers (0x800051e0 + 0x830 k; the 0x8000AAAA word at
/// +0xC, then 500 data words 0x8001wwww, then 0x08005555) [measured].
pub const FRAME_BUF: u32 = 0x8000_51E0;
pub const FRAME_STRIDE: u32 = 0x830;
/// Offset of data word 0 in a buffer.
pub const FRAME_DATA: u32 = 0x10;
pub const FRAME_WORDS: usize = 500;
/// The frame's eDMA channel (its TCD's source is the buffer being sent).
pub const FRAME_CHAN: usize = 15;

const FS: f32 = 48_000.0;
const PI: f32 = std::f32::consts::PI;
const TAU: f32 = std::f32::consts::TAU;

/// RX word scale: 24-bit, right-justified.
const RX_FULL: f32 = 8_388_607.0;
/// Oscillator frequency words: 8 units a Hz [measured: note 60 at TUNE 0 is
/// 0x082c = 2092, 261.6 Hz; an octave doubles the word].
const OSC_UNITS_PER_HZ: f32 = 8.0;
/// The cymbal banks: as the Rytm's (CB CLASSIC's 23531 = 539.5 Hz).
const BANK_UNITS_PER_HZ: f32 = 43.62;
/// A pitch record level adds this much to its oscillator, linear in Hz (the
/// Rytm model's sweep scale, which this hardware's records share) [inferred].
const HZ_PER_PITCH_LEVEL: f32 = 25.67;
/// The level of a voice and of the FX block at the analog outputs, set so a
/// default voice peaks near the digital tracks (about -13 dBFS) [inferred].
const OUT_GAIN: f32 = 0.25;
/// The receive words' level against the analog outputs: the voice as it
/// leaves its filter, at the level it has at the outputs [inferred]. CPU1
/// feeds them to the delay and reverb sends (a 440 Hz tone at -20 dBFS on
/// word 0 comes back in the main pair at about -41 dB RMS) [measured].
const RX_GAIN: f32 = 1.0;
/// The digital layer words (DAC) of tracks 9..12 [measured].
const LAYER_DAC: [usize; 4] = [4, 1, 0, 6];
/// The switch byte of each voice: (word, shift) [measured].
const SWITCH: [(usize, u32); 4] = [(80, 8), (80, 0), (81, 8), (81, 0)];
/// The FX routing bit of each voice: (word, bit) [measured].
const ROUTE: [(usize, u32); 4] = [(82, 12), (82, 3), (83, 10), (83, 11)];

fn w(img: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([img[2 * i], img[2 * i + 1]])
}

/// One envelope record as last sent, parsed once a frame.
#[derive(Clone, Copy, Default)]
struct Rec {
    start: f32,
    target: f32,
    step: f32,
    mode: u8,
    w0: u16,
    rate0: bool,
}

impl Rec {
    /// The record at frame word `i`; `unsigned` levels for the pitch records.
    fn at(img: &[u8], i: usize, unsigned: bool) -> Rec {
        let r = &img[2 * i..2 * i + 8];
        let p = Record::parse(r);
        let (start, target) = if unsigned { (r[2] as f32, r[3] as f32) } else { (p.start, p.target) };
        Rec { start, target, step: p.step, mode: r[4] >> 4, w0: w(img, i), rate0: u32::from_be_bytes([r[4], r[5], r[6], r[7]]) & 0xF_FFFF == 0 }
    }
}

/// An envelope following its record, after the Rytm model's `SegEnv`: a
/// segment starts when the record's mode turns to 0xF or its first word to the
/// start command 0x4000 (each lasts one frame); mode 3, and mode 0 without a
/// rate, hold [measured records; meaning inferred].
#[derive(Clone, Copy)]
struct StEnv {
    level: f32,
    hold: bool,
    mode: u8,
    w0: u16,
}

impl Default for StEnv {
    fn default() -> Self {
        StEnv { level: -128.0, hold: false, mode: 0, w0: 0 }
    }
}

impl StEnv {
    /// A new frame's record: start a segment, or change how it runs.
    fn update(&mut self, r: &Rec, cmd: bool) {
        if (r.mode == 0xF && self.mode != 0xF) || (cmd && r.w0 == 0x4000 && self.w0 != 0x4000) {
            self.level = r.start;
            self.hold = false;
        }
        self.mode = r.mode;
        self.w0 = r.w0;
        match r.mode {
            0x3 => self.hold = r.rate0 || (r.target - self.level).abs() < 1e-3,
            0x0 if r.rate0 => self.hold = true,
            0x9 | 0xB | 0xF => self.hold = false,
            _ => {}
        }
    }

    fn tick(&mut self, r: &Rec) {
        if self.hold {
            return;
        }
        let d = r.target - self.level;
        if d.abs() <= r.step {
            self.level = r.target;
        } else {
            self.level += r.step * d.signum();
        }
    }

    /// The level as a gain: 127 full scale, 6 dB a 16 levels, -128 silent.
    fn gain(&self) -> f32 {
        if self.level <= -127.5 {
            0.0
        } else {
            ((self.level - 127.0) / LEVELS_PER_6DB).exp2()
        }
    }
}

/// A topology-preserving state-variable filter.
#[derive(Clone, Copy, Default)]
struct Svf {
    low: f32,
    band: f32,
}

impl Svf {
    /// (low, band, high) for cutoff `fc` and damping `k` (2 = none).
    fn run(&mut self, x: f32, fc: f32, k: f32) -> (f32, f32, f32) {
        let g = (PI * fc.clamp(10.0, FS * 0.45) / FS).tan();
        let a1 = 1.0 / (1.0 + g * (g + k));
        let (a2, a3) = (g * a1, g * g * a1);
        let v3 = x - self.low;
        let band = a1 * self.band + a2 * v3;
        let low = self.low + a2 * self.band + a3 * v3;
        self.band = 2.0 * band - self.band;
        self.low = 2.0 * low - self.low;
        if !self.low.is_finite() || !self.band.is_finite() {
            *self = Svf::default();
            return (0.0, 0.0, 0.0);
        }
        // Let a decayed state reach exact silence.
        if self.low.abs() < 1e-12 && self.band.abs() < 1e-12 && x == 0.0 {
            *self = Svf::default();
        }
        (low, band, x - k * band - low)
    }

    fn quiet(&self) -> bool {
        self.low == 0.0 && self.band == 0.0
    }
}

/// The cutoff in Hz of a cutoff CV (`(FREQ * 256 - 0x4000) * 2`, signed): a
/// semitone a FREQ step, 440 Hz at 60, as the Rytm's filter law [inferred].
fn cutoff_hz(cv: f32) -> f32 {
    let frq = cv / 512.0 + 64.0;
    (440.0 * ((frq - 60.0) / 12.0).exp2()).clamp(10.0, 22_000.0)
}

/// One oscillator waveform from a 2-bit code of the oscillator nibble:
/// 1 sine, 3 triangle, 2 saw, 0 sine [inferred from BD HARD's WAV knob, which
/// moves the audible oscillator's code from 1 to 3 at its triangle].
fn wave(ph: f32, code: u16) -> f32 {
    match code & 3 {
        3 => 1.0 - 4.0 * (ph - 0.5).abs(),
        2 => 2.0 * ph - 1.0,
        _ => (TAU * ph).sin(),
    }
}

/// An analog drum voice's source: two oscillators with a pitch and an amp
/// envelope each.
#[derive(Clone, Copy, Default)]
struct Drum {
    ph: [f32; 2],
    pitch: [StEnv; 2],
    amp: [StEnv; 2],
    recs: [Rec; 4],
}

impl Drum {
    fn update(&mut self, img: &[u8], v: usize, trig: bool) {
        for k in 0..4 {
            self.recs[k] = Rec::at(img, 140 + 16 * v + 4 * k, k < 2);
        }
        for k in 0..2 {
            self.pitch[k].update(&self.recs[k], false);
            self.amp[k].update(&self.recs[2 + k], false);
        }
        if trig {
            self.ph = [0.0; 2]; // reset by the trig [inferred]
        }
    }

    fn sample(&mut self, img: &[u8], v: usize, sw: u16) -> f32 {
        for k in 0..2 {
            self.pitch[k].tick(&self.recs[k]);
            self.amp[k].tick(&self.recs[2 + k]);
        }
        let a = [self.amp[0].gain(), self.amp[1].gain()];
        if a == [0.0, 0.0] {
            return 0.0;
        }
        let mut out = 0.0;
        for k in 0..2 {
            let hz = w(img, 18 + 4 * v + k) as f32 / OSC_UNITS_PER_HZ + self.pitch[k].level.max(0.0) * HZ_PER_PITCH_LEVEL;
            let o = wave(self.ph[k], sw >> (2 * k));
            self.ph[k] = (self.ph[k] + hz.min(FS * 0.45) / FS).fract();
            // Levels: 0x1fff nominal, 0x7fff with OVER at its top [measured];
            // nominal is taken as the overdrive's full input, about 3x the
            // digital layer's (UT NOISE's VCA word is 3x a drum's) [inferred].
            let l = w(img, 20 + 4 * v + k) as f32 / 8191.0;
            out += o * l * a[k];
        }
        out
    }
}

/// (record word, centre Hz, high-pass?) of the cymbal voice's paths, after the
/// Rytm's cymbal model: each a band of the metal at its record's level, taken
/// as linear (CB CLASSIC's strike runs 20 -> -120 in about 6 ms, as the Rytm
/// CB's record 6) [measured records; filters inferred].
const CYM_PATHS: [(usize, f32, bool); 3] = [(192, 3400.0, false), (196, 7000.0, false), (200, 9000.0, true)];
/// The body's record (204): every cymbal machine starts it at 127 and its
/// decay follows DEC [measured]; taken as the voice's amp, logarithmic like the
/// VCA's, over the paths and the metal through a high-pass at 6 kHz, or a
/// band-pass at 1.2 kHz with two oscillators running (the cowbells) [inferred].
const CYM_BODY: usize = 204;

/// The cymbal voice's source: two banks of six squares through three filtered
/// paths and the body.
#[derive(Clone, Copy, Default)]
struct Cymbal {
    ph: [f32; 12],
    paths: [StEnv; 4],
    recs: [Rec; 4],
    svf: [Svf; 4],
}

impl Cymbal {
    fn update(&mut self, img: &[u8]) {
        for k in 0..4 {
            self.recs[k] = Rec::at(img, if k < 3 { CYM_PATHS[k].0 } else { CYM_BODY }, false);
            self.paths[k].update(&self.recs[k], false);
        }
    }

    fn sample(&mut self, img: &[u8]) -> f32 {
        for k in 0..4 {
            self.paths[k].tick(&self.recs[k]);
        }
        let body = self.paths[3].gain();
        if body == 0.0 && self.paths[..3].iter().all(|p| p.level <= 0.0) && self.svf.iter().all(Svf::quiet) {
            return 0.0;
        }
        let (mut metal, mut running) = (0.0, 0);
        for (k, ph) in self.ph.iter_mut().enumerate() {
            let o = 308 + 2 * k;
            let v = ((w(img, o) as u32) << 16 | w(img, o + 1) as u32) & 0x3F_FFFF;
            if v == 0 {
                continue;
            }
            running += 1;
            *ph = (*ph + (v as f32 / BANK_UNITS_PER_HZ).min(FS * 0.45) / FS).fract();
            metal += if *ph < 0.5 { 1.0 } else { -1.0 };
        }
        if running == 0 {
            return 0.0;
        }
        metal /= running as f32;
        let cowbell = running <= 2;
        // Colour (NCOL): shifts the paths, up to an octave and a half [inferred].
        let colour = w(img, 30) as i16 as f32 / 32768.0;
        let mut out = 0.0;
        for (p, &(_, hz, high)) in CYM_PATHS.iter().enumerate() {
            let lv = self.paths[p].level;
            let (fc, high) = if cowbell { (1200.0, false) } else { (hz * (0.75 * colour).exp2(), high) };
            let (_, band, hp) = self.svf[p].run(metal, fc, 0.8);
            out += if high { hp } else { band } * lv.max(0.0) / 127.0;
        }
        let (fc, high) = if cowbell { (1200.0, false) } else { (6000.0 * (0.75 * colour).exp2(), true) };
        let (_, band, hp) = self.svf[3].run(metal, fc, 0.8);
        out = (out + if high { hp } else { band }) * body;
        // The level into its overdrive (34): 0x1256 at CH CLASSIC's defaults.
        out * (w(img, 34) as f32 / 4096.0)
    }
}

/// A voice's analog filter and VCA.
#[derive(Clone, Copy, Default)]
struct Chain {
    svf: Svf,
    /// The output's coupling (a 5 Hz high-pass): its last input and output.
    dc: (f32, f32),
    flt: StEnv,
    flt_rec: Rec,
    vca: [StEnv; 2],
    vca_rec: [Rec; 2],
}

impl Chain {
    fn update(&mut self, img: &[u8], v: usize) {
        self.flt_rec = Rec::at(img, 124 + 4 * v, false);
        self.flt.update(&self.flt_rec, false);
        for k in 0..2 {
            self.vca_rec[k] = Rec::at(img, 84 + 20 * k + 4 * v, false);
            self.vca[k].update(&self.vca_rec[k], true);
        }
    }

    /// The filter on `x`: what the codec digitises for the track.
    fn filter(&mut self, img: &[u8], v: usize, x: f32, ty: u16) -> f32 {
        self.flt.tick(&self.flt_rec);
        if x == 0.0 && self.svf.quiet() && self.dc == (0.0, 0.0) {
            return 0.0;
        }
        let depth = w(img, 124 + 4 * v) as i16 as f32;
        let cv = w(img, 10 + v) as i16 as f32 + depth * (self.flt.level.max(0.0) / 127.0);
        let fc = cutoff_hz(cv);
        // Resonance: 0 at RESO 0, about 0x7000 at 127 [measured].
        let res = (w(img, 14 + v) as f32 / 28672.0).clamp(0.0, 1.0);
        let k = 2.0 * (1.0 - 0.97 * res);
        let (low, band, high) = self.svf.run(x, fc, k);
        let y = match ty {
            1 => low,
            2 => band,
            3 => high,
            4 => low + high,
            5 => x + 2.0 * res * low,
            6 => x + band * k * 2.0 * res,
            7 => x + 2.0 * res * high,
            _ => x,
        };
        // AC coupling at the output [inferred]; a decayed tail ends at exact 0.
        let out = y - self.dc.0 + 0.9993 * self.dc.1;
        self.dc = if y == 0.0 && out.abs() < 1e-9 { (0.0, 0.0) } else { (y, out) };
        out
    }

    /// The VCA's L/R gains: its records times the level x pan words.
    fn vca(&mut self, img: &[u8], v: usize) -> (f32, f32) {
        self.vca[0].tick(&self.vca_rec[0]);
        self.vca[1].tick(&self.vca_rec[1]);
        // Linear in the knobs (0x3ea at VOL 110, pan centre, OVER 0, LEV 51;
        // 0x0ba1 at LEV 98, OVER 57) [measured]; 2048 taken as unity [inferred].
        (self.vca[0].gain() * w(img, v) as f32 / 2048.0, self.vca[1].gain() * w(img, 5 + v) as f32 / 2048.0)
    }
}

/// The analog FX block: drive, a stereo multimode filter with its envelope,
/// and an amp [manual 5.4: drive, multimode filter, amplifier].
#[derive(Clone, Copy, Default)]
struct FxBlock {
    svf: [[Svf; 2]; 2],
    amp: [StEnv; 2],
    amp_rec: [Rec; 2],
    flt: [StEnv; 2],
    flt_rec: [Rec; 2],
}

impl FxBlock {
    fn update(&mut self, img: &[u8]) {
        for k in 0..2 {
            self.amp_rec[k] = Rec::at(img, 208 + 4 * k, false);
            self.amp[k].update(&self.amp_rec[k], true);
            self.flt_rec[k] = Rec::at(img, 216 + 4 * k, false);
            self.flt[k].update(&self.flt_rec[k], false);
        }
    }

    fn process(&mut self, img: &[u8], x: [f32; 2]) -> [f32; 2] {
        for k in 0..2 {
            self.amp[k].tick(&self.amp_rec[k]);
            self.flt[k].tick(&self.flt_rec[k]);
        }
        if x == [0.0, 0.0] && self.svf.iter().flatten().all(Svf::quiet) {
            return [0.0, 0.0];
        }
        let ty = (w(img, 82) >> 8) & 0xF;
        // Resonance word: 0xfa71 at RESO 0 and 0x02c1 at 127 with LP4; the
        // other types shift it (0xeae9, 0xbff1 for the notch) [measured]; one
        // range for all [inferred].
        let base = match ty {
            0xE => -1423.0,
            0x8 => -16399.0,
            _ => -5399.0,
        };
        let res = ((w(img, 39) as i16 as f32 - base) / 2128.0).clamp(0.0, 1.0);
        let k = 2.0 * (1.0 - 0.95 * res);
        let mut y = [0.0f32; 2];
        for c in 0..2 {
            // Drive: a soft clip [inferred; DRIVE moves only the output gain
            // words in the frame, so the drive itself is on the inputs].
            let d = x[c].tanh();
            let depth = w(img, 216 + 4 * c) as i16 as f32;
            let cv = w(img, 37 + c) as i16 as f32 + depth * (self.flt[c].level.max(0.0) / 127.0);
            let fc = cutoff_hz(cv);
            let (low, band, high) = self.svf[c][0].run(d, fc, if ty == 0xE { 2.0 } else { k });
            y[c] = match ty {
                0xE => self.svf[c][1].run(low, fc, k).0,
                0xD => low,
                0xB => band,
                0xA => high,
                0x8 => low + high,
                0x9 => {
                    let n = low + high;
                    let (l2, _, h2) = self.svf[c][1].run(n, fc * 1.5, k);
                    l2 + h2
                }
                0xC => low + 0.5 * (low + high),
                _ => d,
            };
            // The VCA words: 0x08a4 at DRIVE 1.00, VOL 110 [measured], taken
            // as unity.
            y[c] *= self.amp[c].gain() * w(img, 35 + c) as f32 / 2212.0;
        }
        y
    }
}

pub struct StAnalog {
    /// The frame as big-endian u16s (the data halves), rebuilt when channel
    /// 15 sends a new buffer.
    img: Vec<u8>,
    sent: u32,
    drums: [Drum; 3],
    cym: Cymbal,
    chains: [Chain; 4],
    fx: FxBlock,
    /// The last frame seen (data words), for the trace.
    last: Vec<u16>,
    frames: u64,
    /// DTEMU_ST_FRAME_TRACE=1: print the frame's changed words per new frame.
    trace: bool,
    /// DTEMU_ST_FRAME_DUMP=FILE: every 32nd codec frame's 500 data words (u16
    /// big-endian, 1000 bytes a record) appended to FILE.
    dump: Option<std::fs::File>,
    /// DTEMU_ST_RX_TONE=k[:AMP]: a 440 Hz test sine on receive word k
    /// (AMP of full scale, default 0.25).
    tone: Option<(usize, f32)>,
    phase: f32,
}

impl Default for StAnalog {
    fn default() -> Self {
        let tone = std::env::var("DTEMU_ST_RX_TONE").ok().and_then(|v| {
            let mut p = v.split(':');
            let k = p.next()?.parse().ok()?;
            Some((k, p.next().and_then(|a| a.parse().ok()).unwrap_or(0.25)))
        });
        StAnalog {
            img: vec![0; 2 * FRAME_WORDS],
            sent: 0,
            drums: [Drum::default(); 3],
            cym: Cymbal::default(),
            chains: [Chain::default(); 4],
            fx: FxBlock::default(),
            last: vec![0; FRAME_WORDS],
            frames: 0,
            trace: std::env::var_os("DTEMU_ST_FRAME_TRACE").is_some(),
            dump: std::env::var_os("DTEMU_ST_FRAME_DUMP").and_then(|p| std::fs::File::create(p).ok()),
            tone,
            phase: 0.0,
        }
    }
}

/// One codec frame of the analog side.
pub struct Out {
    /// The receive frame's 8 words (24-bit).
    pub rx: [i32; 8],
    /// The analog voices and the FX block at the main outputs (L, R), -1..1;
    /// the DAC's main pair joins them in `Machine::speaker_pair`.
    pub mix: (f32, f32),
}

fn to24(x: f32) -> i32 {
    (x.clamp(-1.0, 1.0) * RX_FULL) as i32
}

impl StAnalog {
    /// Which buffer the frame is in: the one eDMA channel 15's source points into.
    pub fn frame_base(tcd15_saddr: u32) -> u32 {
        let k = tcd15_saddr.wrapping_sub(FRAME_BUF) / FRAME_STRIDE;
        FRAME_BUF + FRAME_STRIDE * k.min(1) + FRAME_DATA
    }

    /// A new frame went out: take its words and update the envelopes.
    fn new_frame(&mut self, f: &[u8]) {
        for i in 0..FRAME_WORDS {
            self.img[2 * i] = f[4 * i + 2];
            self.img[2 * i + 1] = f[4 * i + 3];
        }
        let img = &self.img;
        for v in 0..4 {
            let was = self.chains[v].vca[0].w0;
            self.chains[v].update(img, v);
            let trig = was != 0x4000 && self.chains[v].vca[0].w0 == 0x4000;
            if v < 3 {
                self.drums[v].update(img, v, trig);
            }
        }
        self.cym.update(img);
        self.fx.update(img);
        if self.trace {
            let mut ch = Vec::new();
            for i in 0..FRAME_WORDS {
                let x = w(img, i);
                if x != self.last[i] {
                    ch.push(format!("{i}:{:04x}>{x:04x}", self.last[i]));
                    self.last[i] = x;
                }
            }
            if !ch.is_empty() {
                eprintln!("stframe {} {}", self.frames, ch.join(" "));
            }
        }
    }

    /// One codec frame. `f` is the frame being sent (500 words of 4 bytes,
    /// big-endian: 0x8001 then the data half), `sent` channel 15's source
    /// address (a new value is a new frame), `dac` the last complete DAC frame.
    pub fn frame(&mut self, f: &[u8], sent: u32, dac: &[i32; 16]) -> Out {
        self.frames += 1;
        if sent != self.sent || self.frames == 1 {
            self.sent = sent;
            self.new_frame(f);
        }
        if self.frames.is_multiple_of(32) {
            if let Some(d) = self.dump.as_mut() {
                use std::io::Write;
                let _ = d.write_all(&self.img);
            }
        }
        let img = &self.img;
        let s = |x: i32| x as f32 / 8_388_608.0;
        let mut rx = [0i32; 8];
        let mut main = [0.0f32; 2];
        let mut fx_in = [s(dac[7]), s(dac[2])];
        for v in 0..4 {
            let sw = (w(img, SWITCH[v].0) >> SWITCH[v].1) & 0xFF;
            let src = if v < 3 { self.drums[v].sample(img, v, sw & 0xF) } else { self.cym.sample(img) } + s(dac[LAYER_DAC[v]]);
            // Each voice's overdrive: a soft clip; OVER raises the levels into
            // it and lowers the VCA words after it [measured words; curve inferred].
            let x = if src == 0.0 { 0.0 } else { src.tanh() };
            // Filter type: bits 4-6 of a drum voice's switch byte, 0-2 of the
            // cymbal's (whose bits 4-5 select its banks' mode) [measured].
            let ty = if v < 3 { (sw >> 4) & 7 } else { sw & 7 };
            let y = self.chains[v].filter(img, v, x, ty) * OUT_GAIN;
            let (gl, gr) = self.chains[v].vca(img, v);
            rx[v] = to24(y * RX_GAIN);
            if y == 0.0 {
                continue;
            }
            let (r, b) = ROUTE[v];
            let bus = if (w(img, r) >> b) & 1 != 0 { &mut fx_in } else { &mut main };
            bus[0] += y * gl;
            bus[1] += y * gr;
        }
        let fx = self.fx.process(img, fx_in);
        // The FX return comes back negated (CPU1 negates receive words 4..7,
        // 0x400962ac) [measured code; the inverting stage inferred].
        rx[4] = to24(-fx[0] * OUT_GAIN * RX_GAIN);
        rx[5] = to24(-fx[1] * OUT_GAIN * RX_GAIN);
        if let Ok(m) = std::env::var("DTEMU_ST_RX_MASK") {
            let m = u32::from_str_radix(&m, 16).unwrap_or(0xFF);
            for (k, x) in rx.iter_mut().enumerate() {
                if m >> k & 1 == 0 {
                    *x = 0;
                }
            }
        }
        if let Some((k, amp)) = self.tone {
            self.phase = (self.phase + 440.0 / FS).fract();
            if k < 8 {
                rx[k] = ((self.phase * TAU).sin() * amp * RX_FULL) as i32;
            }
        }
        Out { rx, mix: (main[0] + fx[0] * OUT_GAIN, main[1] + fx[1] * OUT_GAIN) }
    }
}
