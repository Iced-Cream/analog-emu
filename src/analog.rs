//! A model of the Analog Rytm MKII's analog voices, driven by the control frame
//! the OS sends them.
//!
//! Every audio block the OS writes a 984-byte control frame at `0x800063c0` and
//! sends it to the voice hardware (rytm-mods `re/subsystems/voice-controls.md`).
//! On the device the voices come back through the codec input: each receive frame
//! holds MAIN L/R (negated, post-compressor), the eight voices and AUDIO IN L/R.
//! This model reads the frame once per receive frame and produces those voice
//! words, plus a stereo mix for the speaker.
//!
//! So far: the three dual-oscillator voices, BD (physical voice 0), SD (2) and
//! RS/CP (7), one circuit design at three places in the frame; the hihat
//! (CH/OH, voice 4); the toms (BT 1, LT 3, MT/HT 5); cymbal/cowbell (CY/CB, 6). Their two
//! oscillators, levels, FM and ring-mod switches (the dual-oscillator core
//! shared with the Syntakt, `vco.rs`) and pitch and amplitude envelopes
//! come from the frame; everything about how a value sounds is approximate and
//! marked [inferred].

use crate::vco::{Osc2Set, Pair, Wave};

/// Offsets in the control frame (OS 1.73/1.74; RAM did not move between them).
pub const FRAME_ADDR: u32 = 0x8000_63C0;
pub const FRAME_LEN: usize = 984;

const FS: f32 = 48_000.0;
/// Hz per unit of a dual-oscillator tune CV. The OS applies the exponential pitch
/// conversion before writing the CV, so it is taken as linear in frequency; the
/// scale puts BD PLASTIC's default (678) at about 55 Hz [inferred].
const HZ_PER_TUNE: f32 = 55.0 / 678.0;
/// Tune-CV units per pitch-envelope level: the sweep adds to the tune CV, linear
/// in frequency as the CV itself. BD PLASTIC's SWD sets the pitch record's start
/// level from 2 to 15 and SD HARD starts at 100 [measured]; the scale makes BD
/// PLASTIC's full sweep 3 octaves (678 + 15 x 316 = 8 x 678) [inferred].
const TUNE_PER_PITCH_LEVEL: f32 = 678.0 * 7.0 / 15.0;
/// Envelope levels per 6 dB: the amplitude level is taken as logarithmic, as an
/// exponential VCA's control is, so 127 is full scale and 255 levels span 96 dB;
/// a linear ramp in level then decays exponentially [inferred].
pub(crate) const LEVELS_PER_6DB: f32 = 16.0;

/// Analog imperfections, so no two hits are quite alike (after the approach of
/// kitbasher's analog recreations for the Machinedrum, not measured on a Rytm)
/// [inferred depths]: each hit draws a random detune of up to DETUNE_CENTS per
/// oscillator; the dual-oscillator voices' oscillators wander slowly (about
/// DRIFT_CENTS RMS over DRIFT_TAU seconds, independently); the hat and
/// cymbal/cowbell banks, free-running on the device, start a hit that follows
/// silence at random phases. The drums whose phases the trig resets keep that:
/// their attack depends on it. `DTEMU_ANALOG_IDEAL=1` turns all of it off;
/// `DTEMU_ANALOG_SEED=N` picks the random sequence.
const DETUNE_CENTS: f32 = 3.0;
const DRIFT_CENTS: f32 = 1.5;
const DRIFT_TAU: f32 = 0.5;
/// Drift is updated once per this many samples.
const DRIFT_EVERY: u32 = 32;

/// The random source for the imperfections (xorshift32).
struct Wobble {
    s: u32,
    /// 1 normally, 0 for ideal components.
    depth: f32,
}

impl Wobble {
    fn from_env() -> Self {
        let seed = std::env::var("DTEMU_ANALOG_SEED").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0x2545_F491);
        let ideal = std::env::var_os("DTEMU_ANALOG_IDEAL").is_some_and(|v| v != "0");
        Wobble { s: seed.max(1), depth: if ideal { 0.0 } else { 1.0 } }
    }

    /// Uniform in -1..1.
    fn uni(&mut self) -> f32 {
        let mut x = self.s;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.s = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// A frequency ratio of up to +-`cents`.
    fn detune(&mut self, cents: f32) -> f32 {
        let u = self.uni();
        (u * cents * self.depth / 1200.0).exp2()
    }

    /// A random phase, or 0 for ideal components.
    fn phase(&mut self) -> f32 {
        let u = self.uni();
        if self.depth > 0.0 { 0.5 * (u + 1.0) } else { 0.0 }
    }
}

/// An oscillator's slow pitch wander: a random walk pulled back to centre (an
/// Ornstein-Uhlenbeck process in cents).
#[derive(Clone, Copy)]
struct Drift {
    cents: f32,
    ratio: f32,
    n: u32,
}

impl Default for Drift {
    fn default() -> Self {
        Drift { cents: 0.0, ratio: 1.0, n: 0 }
    }
}

impl Drift {
    fn tick(&mut self, w: &mut Wobble) -> f32 {
        self.n += 1;
        if self.n >= DRIFT_EVERY {
            self.n = 0;
            let a = DRIFT_EVERY as f32 / FS / DRIFT_TAU;
            // uni() has variance 1/3; sqrt(3) makes it 1.
            let kick = w.uni() * 3f32.sqrt() * DRIFT_CENTS * (2.0 * a).sqrt();
            self.cents += -a * self.cents + kick * w.depth;
            self.ratio = (self.cents / 1200.0).exp2();
        }
        self.ratio
    }
}

/// Where a dual-oscillator voice lives in the frame [measured, rytm-mods
/// voice-controls.md]: its CV block (TUNE1, TUNE2, LEVEL1, LEVEL2), its four
/// envelope records (a pitch pair, then an amplitude pair) and its seven switch
/// bits by table index (0 trig, 1 osc 1 wave, 2/3/6 osc 2 wave, 4 FM on, 5 ring
/// mod, active low). Osc 1's bit set is its sine, clear its sinesaw; 2/3/6 are
/// the c, b, a bits of osc 2's waveform code (`vco::Wave`) [measured: every
/// waveform and CFG setting of BD SHARP, BD ACOUSTIC, SY DUAL VCO and SY RAW
/// against the Syntakt's codes, 2026-10-13]. No bit means sync.
#[derive(Clone, Copy)]
struct DualSpec {
    voice: usize,
    cv: usize,
    env: usize,
    sw: [usize; 7],
}

const BD: DualSpec = DualSpec { voice: 0, cv: 90, env: 360, sw: [12, 44, 8, 47, 46, 45, 40] };
const SD: DualSpec = DualSpec { voice: 2, cv: 98, env: 392, sw: [0, 32, 36, 35, 34, 33, 56] };
const RS: DualSpec = DualSpec { voice: 7, cv: 106, env: 424, sw: [28, 4, 24, 7, 6, 5, 60] };

/// VCA L and R per physical voice: track level x pan (x the AMP page's
/// envelope), at +10 + 2v and +26 + 2v [measured].
const VCA_L: usize = 10;
const VCA_R: usize = 26;

fn word(f: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([f[o], f[o + 1]])
}

fn bit(f: &[u8], n: usize) -> bool {
    f[160 + (n >> 3)] >> (n & 7) & 1 != 0
}

/// One 8-byte envelope record [measured layout; level and mode meanings inferred].
/// Its first word is a command, not a gain: 0x4000 starts the segment at its start
/// level, 0 continues from the current level toward the target (BD and SD each
/// write 0x4000 once when a hit begins, then 0 while it runs) [measured].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Record {
    pub(crate) start_cmd: bool,
    pub(crate) start: f32,
    pub(crate) target: f32,
    /// Levels per sample.
    pub(crate) step: f32,
}

impl Record {
    pub(crate) fn parse(r: &[u8]) -> Record {
        let w = u32::from_be_bytes([r[4], r[5], r[6], r[7]]);
        let exp = (w >> 16) & 0xF;
        let mant = w & 0xFFFF;
        // The OS's decoder (0x4011e780): step = (0x7fffffff - (mant << 14)) >> exp,
        // in levels << 23 per 1/16 block, and a 1/16 block is 2 samples.
        let step = ((0x7FFF_FFFFu32 - (mant << 14)) >> exp) as f32 / (1u32 << 23) as f32 / 2.0;
        Record {
            start_cmd: r[0] | r[1] != 0,
            start: r[2] as i8 as f32,
            target: r[3] as i8 as f32,
            step,
        }
    }
}

/// An envelope segment playing toward its target.
#[derive(Clone, Copy, Default)]
struct Env {
    level: f32,
}

impl Env {
    fn tick(&mut self, r: &Record) {
        let d = r.target - self.level;
        if d.abs() <= r.step {
            self.level = r.target;
        } else {
            self.level += r.step * d.signum();
        }
    }
}

struct DualOsc {
    spec: DualSpec,
    /// The pitch and amplitude records as last seen: a changed record carrying
    /// the start command starts its segment.
    last_p: [u8; 8],
    last_a: [u8; 8],
    ph1: f32,
    ph2: f32,
    pitch: Env,
    amp: Env,
    /// Oscillator 2's amp record (the pair's second, +24): the FM depth's
    /// envelope (BD FM's FMD) [inferred from the Syntakt's BD FM, whose FM D
    /// moves the same record].
    last_m: [u8; 8],
    amp2: Env,
    trig: bool,
    /// This hit's detune and the slow drift, per oscillator.
    det: [f32; 2],
    drift: [Drift; 2],
}

impl DualOsc {
    /// Silent until the first trig: the amplitude envelope rests at its floor.
    fn new(spec: DualSpec) -> Self {
        DualOsc { spec, last_p: [0; 8], last_a: [0; 8], ph1: 0.0, ph2: 0.0, pitch: Env::default(), amp: Env { level: -128.0 }, last_m: [0; 8], amp2: Env { level: -128.0 }, trig: false, det: [1.0; 2], drift: [Drift::default(); 2] }
    }
}

impl DualOsc {
    /// One sample of the voice, -1..1-ish.
    fn sample(&mut self, f: &[u8], w: &mut Wobble) -> f32 {
        let s = self.spec;
        let trig = bit(f, s.sw[0]);
        let pr = Record::parse(&f[s.env..]);
        let ar = Record::parse(&f[s.env + 16..]);
        if trig && !self.trig {
            self.ph1 = 0.0;
            self.ph2 = 0.0;
            self.det = [w.detune(DETUNE_CENTS), w.detune(DETUNE_CENTS)];
        }
        self.trig = trig;
        let (rp, ra) = (&f[s.env..s.env + 8], &f[s.env + 16..s.env + 24]);
        if rp != self.last_p {
            if pr.start_cmd {
                self.pitch.level = pr.start;
            }
            self.last_p.copy_from_slice(rp);
        }
        if ra != self.last_a {
            if ar.start_cmd {
                self.amp.level = ar.start;
            }
            self.last_a.copy_from_slice(ra);
        }
        self.pitch.tick(&pr);
        self.amp.tick(&ar);
        let sweep = self.pitch.level.max(0.0) * TUNE_PER_PITCH_LEVEL;
        let f1 = (word(f, s.cv) as f32 + sweep) * HZ_PER_TUNE * self.det[0] * self.drift[0].tick(w);
        let f2 = (word(f, s.cv + 2) as f32 + sweep) * HZ_PER_TUNE * self.det[1] * self.drift[1].tick(w);
        let l1 = word(f, s.cv + 4) as f32 / 32767.0;
        let l2 = word(f, s.cv + 6) as f32 / 32767.0;
        let rm = &f[s.env + 24..s.env + 32];
        if rm != self.last_m {
            let r = Record::parse(rm);
            if r.start_cmd {
                self.amp2.level = r.start;
            }
            self.last_m.copy_from_slice(rm);
        }
        self.amp2.tick(&Record::parse(rm));
        let fm = bit(f, s.sw[4]);
        let ring = !bit(f, s.sw[5]);
        let gain = if self.amp.level <= -127.5 { 0.0 } else { ((self.amp.level - 127.0) / LEVELS_PER_6DB).exp2() };
        if fm || ring {
            // The shared dual-oscillator core (vco.rs): FM, ring modulation and
            // the full waveform codes. Oscillator 1 has the c bit only (index
            // 1: sine or sinesaw); oscillator 2's code is (a, b, c) = indices
            // (6, 3, 2) [measured: the Syntakt's codes for the same settings].
            let set = Osc2Set {
                hz: [f1, f2],
                stopped: [word(f, s.cv) == 0, word(f, s.cv + 2) == 0],
                wave: [Wave::from_bits(true, false, bit(f, s.sw[1])), Wave::from_bits(bit(f, s.sw[6]), bit(f, s.sw[3]), bit(f, s.sw[2]))],
                level: [l1, l2],
                depth_word: word(f, s.cv + 6) as f32 * (self.amp2.level / 127.0).clamp(0.0, 1.0),
                fm,
                ring,
            };
            let mut p = Pair { ph: [self.ph1, self.ph2] };
            let y = p.sample(&set, FS);
            [self.ph1, self.ph2] = p.ph;
            return y * gain;
        }
        let tau = std::f32::consts::TAU;
        let o2 = (tau * self.ph2).sin();
        // Without FM or ring modulation: the model as it was before the
        // switches were decoded (osc 1 a sine, "sinesaw" with index 1 set,
        // though the codes say index 1 set is the sine; kept so these
        // machines sound as before).
        let mut o1 = (tau * self.ph1).sin();
        if bit(f, s.sw[1]) {
            // "sinesaw": a sine with some saw added [inferred shape].
            o1 = 0.7 * o1 + 0.3 * (2.0 * self.ph1 - 1.0);
        }
        self.ph1 = (self.ph1 + f1 / FS).fract();
        self.ph2 = (self.ph2 + f2 / FS).fract();
        let osc = o1 * l1 + o2 * l2;
        // 127 is full scale, -128 the floor (silent); logarithmic between.
        osc * gain
    }
}

/// The hihat (physical voice 4) [rytm-mods voice-controls.md; traced in the
/// emulator, 2026-10-06]. Six 32-bit fields at +616..+639 hold a metal
/// oscillator bank: their low 22 bits stand in the TR-808's ratios
/// (1 : 1.483 : 1.805 : 2.546 : 2.634 : 3.903, CH CLASSIC's defaults) [measured],
/// so they are taken as frequencies, 43.62 units per Hz, which makes those
/// defaults the 808's 205.3..800 Hz [inferred]. +130 is the colour CV
/// (`(COL - 0x4000) * 2`, CH CLASSIC) [measured], taken here as a high-pass
/// cutoff [inferred]. +140 is the hat VCA CV [measured]. Five envelope records
/// at +456..+495: a hit sets record 4's mode nibble to 0xF once (CH also
/// starts record 2, OH record 3) and the decay rate follows (about 90 ms for CH
/// CLASSIC, 0.5 s for OH CLASSIC at default DEC) [measured]; record 4 is used
/// as the hat's amplitude envelope [inferred].
const HAT_VOICE: usize = 4;
const HAT_BANK: usize = 616;
const HAT_COLOUR: usize = 130;
const HAT_VCA: usize = 140;
const HAT_ENV: usize = 488;
const HAT_UNITS_PER_HZ: f32 = 43.62;

struct Hat {
    ph: [f32; 6],
    det: [f32; 6],
    env: Env,
    mode: u8,
    hp: (f32, f32),
}

impl Hat {
    fn new() -> Self {
        Hat { ph: [0.0; 6], det: [1.0; 6], env: Env { level: -128.0 }, mode: 0, hp: (0.0, 0.0) }
    }

    fn sample(&mut self, f: &[u8], w: &mut Wobble) -> f32 {
        let r = &f[HAT_ENV..HAT_ENV + 8];
        let rec = Record::parse(r);
        let mode = r[4] >> 4;
        if mode == 0xF && self.mode != 0xF {
            if self.env.level <= -127.5 {
                // After silence: the free-running bank is wherever it is.
                for ph in self.ph.iter_mut() {
                    *ph = w.phase();
                }
            }
            for d in self.det.iter_mut() {
                *d = w.detune(DETUNE_CENTS);
            }
            self.env.level = rec.start; // the hit
        }
        self.mode = mode;
        if word(f, HAT_ENV) == 0 {
            self.env.level = -128.0; // record parked: silent (choked)
        } else {
            self.env.tick(&rec);
        }
        if self.env.level <= -127.5 {
            return 0.0;
        }
        // The metal bank: six square waves.
        let mut metal = 0.0;
        for (k, ph) in self.ph.iter_mut().enumerate() {
            let o = HAT_BANK + 4 * k;
            let v = u32::from_be_bytes([f[o], f[o + 1], f[o + 2], f[o + 3]]) & 0x3F_FFFF;
            let hz = v as f32 / HAT_UNITS_PER_HZ * self.det[k];
            *ph = (*ph + hz / FS).fract();
            metal += if *ph < 0.5 { 1.0 } else { -1.0 };
        }
        metal /= 6.0;
        // Colour: a 2-pole high-pass (state-variable), 6 kHz at 0, an octave and
        // a half per full scale [inferred].
        let colour = word(f, HAT_COLOUR) as i16 as f32 / 32768.0;
        let fc = (6000.0 * (1.5 * colour).exp2()).clamp(500.0, 16_000.0);
        let g = 2.0 * (std::f32::consts::PI * fc / FS).sin();
        let (low, band) = &mut self.hp;
        let high = metal - *low - 1.2 * *band;
        *band += g * high;
        *low += g * *band;
        let vca = word(f, HAT_VCA) as f32 / 32767.0;
        let gain = ((self.env.level - 127.0) / LEVELS_PER_6DB).exp2();
        high * vca * gain
    }
}

/// Cymbal/cowbell (physical voice 6) [traced in the emulator, 2026-10-06].
/// +640..+663: a second six-field metal bank; CY CLASSIC loads the hat's 808
/// ratios, CB CLASSIC only the first two fields, 539.5 and 796.7 Hz at the hat's
/// 43.62 units/Hz: the TR-808 cowbell's 540 and 800 Hz, which confirms that scale
/// [measured values]. +142 is CY's colour, +158 the VCA. Records +496..+559: a CY
/// hit starts records 2, 3 and 4 (mode 0xF; starts 66, -93, 115, one fast and two
/// slow rates) and record 7 (a slow master); a CB hit starts record 6 (a short
/// strike) and record 7 [measured]. Modelled after the 808's cymbal and cowbell:
/// the bank's squares through one filtered path per record (2: band-pass 3.4 kHz,
/// 3: band-pass 7 kHz, 4: high-pass 9 kHz, 6: band-pass 1.2 kHz, 7: the body,
/// band-pass 1.2 kHz for the cowbell or high-pass 6 kHz for the cymbal), each
/// with its record's envelope (linear in level), summed, times the VCA. CB TEST
/// names its envelopes 5ENV and 8ENV: record 6 is a 6 ms strike, record 7 the
/// longer body [inferred].
const CYM_VOICE: usize = 6;
const CYM_BANK: usize = 640;
const CYM_COLOUR: usize = 142;
const CYM_VCA: usize = 158;
const CYM_ENV: usize = 496;
/// (record, centre Hz, high-pass?) per path.
const CYM_PATHS: [(usize, f32, bool); 5] = [(2, 3400.0, false), (3, 7000.0, false), (4, 9000.0, true), (6, 1200.0, false), (7, 6000.0, true)];

struct Cym {
    ph: [f32; 6],
    det: [f32; 6],
    env: [Env; 8],
    mode: [u8; 8],
    svf: [(f32, f32); 5],
}

impl Cym {
    fn new() -> Self {
        Cym { ph: [0.0; 6], det: [1.0; 6], env: [Env { level: -128.0 }; 8], mode: [0; 8], svf: [(0.0, 0.0); 5] }
    }

    fn sample(&mut self, f: &[u8], w: &mut Wobble) -> f32 {
        let silent = self.env.iter().all(|e| e.level <= -127.5);
        let mut hit = false;
        for r in 0..8 {
            let b = &f[CYM_ENV + 8 * r..CYM_ENV + 8 * r + 8];
            let rec = Record::parse(b);
            let mode = b[4] >> 4;
            if mode == 0xF && self.mode[r] != 0xF {
                self.env[r].level = rec.start;
                hit = true;
            }
            self.mode[r] = mode;
            if word(f, CYM_ENV + 8 * r) == 0 {
                self.env[r].level = -128.0;
            } else {
                self.env[r].tick(&rec);
            }
        }
        if hit {
            if silent {
                for ph in self.ph.iter_mut() {
                    *ph = w.phase();
                }
            }
            for d in self.det.iter_mut() {
                *d = w.detune(DETUNE_CENTS);
            }
        }
        if self.env.iter().all(|e| e.level <= -127.5) {
            return 0.0;
        }
        let mut metal = 0.0;
        let mut running = 0;
        for (k, ph) in self.ph.iter_mut().enumerate() {
            let o = CYM_BANK + 4 * k;
            let v = u32::from_be_bytes([f[o], f[o + 1], f[o + 2], f[o + 3]]) & 0x3F_FFFF;
            if v == 0 {
                continue; // an oscillator the machine leaves off (CB uses two)
            }
            running += 1;
            *ph = (*ph + v as f32 / HAT_UNITS_PER_HZ * self.det[k] / FS).fract();
            metal += if *ph < 0.5 { 1.0 } else { -1.0 };
        }
        metal /= running.max(1) as f32;
        let colour = word(f, CYM_COLOUR) as i16 as f32 / 32768.0;
        let mut out = 0.0;
        let cowbell = running <= 2;
        for (p, &(r, hz, high)) in CYM_PATHS.iter().enumerate() {
            let lv = self.env[r].level;
            if lv <= -127.5 {
                continue;
            }
            // Record 7, the body: the cowbell's band-pass with two oscillators
            // running, the cymbal's high-pass with six.
            let (hz, high) = if r == 7 && cowbell { (1200.0, false) } else { (hz, high) };
            let fc = if r == 6 || (r == 7 && cowbell) { hz } else { hz * (0.75 * colour).exp2() };
            let g = 2.0 * (std::f32::consts::PI * fc.min(16_000.0) / FS).sin();
            let (low, band) = &mut self.svf[p];
            let hp = metal - *low - 0.8 * *band;
            *band += g * hp;
            *low += g * *band;
            let y = if high { hp } else { *band };
            // Path levels are taken as linear amounts (CB's strike starts at 20,
            // a CY band at -93: logarithmic, those would be inaudible) [inferred].
            out += y * (lv / 127.0).max(0.0);
        }
        let vca = word(f, CYM_VCA) as f32 / 32767.0;
        out * vca
    }
}

/// The synthetic tom law the emulator's calibration record describes
/// (`Machine::rytm_calibration`): the OS's fit value y = TOM_Y_PER_HZ * f, and
/// f = TOM_HZ_PER_X * x, where the tune CV is x^2 / 2048 [synthetic].
pub const TOM_Y_PER_HZ: f32 = 1142.8;
pub const TOM_HZ_PER_X: f32 = 0.1;

/// A tom (BT voice 1, LT 3, MT/HT 5): the digital exciter its DAC word carries
/// (8, 9, 10) [rytm-mods voice-controls.md] rings a resonator [inferred: the
/// manual's "analog tom" with a firmware-written pulse]. Tune CV (unsigned) is
/// x^2/2048 in the synthetic calibration's terms, so f = TOM_HZ_PER_X *
/// sqrt(2048 CV) [synthetic]; MT/HT's tune falls over a hit, the pitch sweep
/// [measured]. The decay CV sets the ring time, 30 ms at 0 to 1.5 s at full
/// scale, exponentially [inferred].
#[derive(Clone, Copy)]
struct TomSpec {
    voice: usize,
    tune: usize,
    decay: usize,
    dac: usize,
}

const TOMS: [TomSpec; 3] = [
    TomSpec { voice: 1, tune: 118, decay: 120, dac: 8 },
    TomSpec { voice: 3, tune: 122, decay: 124, dac: 9 },
    TomSpec { voice: 5, tune: 126, decay: 128, dac: 10 },
];

struct Tom {
    spec: TomSpec,
    y1: f32,
    y2: f32,
    /// This hit's detune; samples since the exciter last moved (a hit is the
    /// exciter starting after TOM_REST of them) [inferred].
    det: f32,
    quiet: u32,
}

const TOM_REST: u32 = 64;

impl Tom {
    fn sample(&mut self, f: &[u8], dac: &[i32; 16], w: &mut Wobble) -> f32 {
        let s = self.spec;
        let x = dac[s.dac] as f32 / 8_388_608.0;
        if dac[s.dac] == 0 {
            self.quiet = self.quiet.saturating_add(1);
        } else {
            if self.quiet >= TOM_REST {
                self.det = w.detune(DETUNE_CENTS);
            }
            self.quiet = 0;
        }
        let cv = word(f, s.tune) as f32;
        let hz = TOM_HZ_PER_X * (2048.0 * cv).sqrt() * self.det;
        if cv >= 65535.0 || hz < 10.0 {
            // Parked (the silence helper writes 0xffff) or no pitch: let it die.
            self.y1 *= 0.99;
            self.y2 *= 0.99;
            return self.y1;
        }
        let d = word(f, s.decay) as i16 as f32 / 32767.0;
        let tau = 0.03 * (d.max(0.0) * 5.64).exp2();
        let r = (-1.0 / (tau * FS)).exp();
        let w = std::f32::consts::TAU * hz.min(FS / 4.0) / FS;
        // Input scaled by (1 - r) so the ring's peak does not grow with its length.
        let y = 2.0 * r * w.cos() * self.y1 - r * r * self.y2 + x * (1.0 - r);
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// The analog side of one receive frame.
pub struct FrameOut {
    /// The raw receive frame's 12 words, 24-bit: voices 0..7, AUDIO IN L/R,
    /// MAIN L/R.
    pub rx: [i32; 12],
    /// The modelled analog voices mixed for the speaker, -1..1.
    pub mix: (f32, f32),
}

pub struct Analog {
    /// Where the OS keeps the control frame and the FX track's parameters:
    /// `FRAME_ADDR` / `FX_PARAMS` on the Rytm MKII; 0x80004ce0 / 0x800066e6 on
    /// the Rytm MKI OS 1.74 (the code that writes the frame's word 0 from them,
    /// 0x40119ece, matches the MKII's 0x40121306) [measured].
    pub frame_addr: u32,
    pub fx_addr: u32,
    duals: [DualOsc; 3],
    hat: Hat,
    toms: [Tom; 3],
    cym: Cym,
    chains: [Chain; 8],
    comp: Comp,
    wobble: Wobble,
    frames: u64,
    /// DTEMU_FRAME_TRACE=OFF,OFF,... (byte offsets, decimal): those 16-bit
    /// words, printed whenever one changes, with the receive-frame count.
    trace: Option<(Vec<usize>, Vec<u16>)>,
}

impl Default for Analog {
    fn default() -> Self {
        let trace = std::env::var("DTEMU_FRAME_TRACE").ok().map(|v| {
            let offs: Vec<usize> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
            let n = offs.len();
            (offs, vec![0xFFFF; n])
        });
        Analog { frame_addr: FRAME_ADDR, fx_addr: FX_PARAMS, duals: [DualOsc::new(BD), DualOsc::new(SD), DualOsc::new(RS)], hat: Hat::new(), toms: TOMS.map(|spec| Tom { spec, y1: 0.0, y2: 0.0, det: 1.0, quiet: TOM_REST }), cym: Cym::new(), chains: [Chain::default(); 8], comp: Comp::default(), wobble: Wobble::from_env(), frames: 0, trace }
    }
}

/// The codec's raw receive frame, as the SSI delivers it to the DMA ring at
/// 0x80003000 [measured, the copy routine 0x4010e944]: words 0..7 are the
/// physical voices in order, 8/9 AUDIO IN L/R, 10/11 MAIN L/R. The audio
/// interrupt copies them into the documented buffer at 0x80007440 (MAIN, the
/// voices by slot `0x401ef1d0`, AUDIO IN), negating every word, and the pads'
/// activity glow reads raw word v as voice v (0x401214c4). MAIN goes in
/// un-negated here, so that copy holds it negated, as rytm-mods measured.
const RX_AUDIO_IN: usize = 8;
const RX_MAIN: usize = 10;

fn to24(x: f32) -> i32 {
    (x.clamp(-1.0, 1.0) * 8_388_607.0) as i32
}

/// Each voice's own analog chain after its source [rytm-mods voice-controls.md
/// and audio-engine.md: generator (+ sample layer), overdrive, multimode filter,
/// amp, pan]. Fields per physical voice v [measured]: overdrive +42+2v
/// (`OVR * 2`), filter frequency +58+2v (`(FRQ - 0x4000) * 2`), resonance
/// +74+2v (8.8), filter envelope record +296+8v (its first word the ENV depth,
/// `(ENV - 0x4000) * 2`), amp envelope records +168+8v (L) and +232+8v (R),
/// level x pan in the VCA words +10/+26. Filter type: a 3-bit code per voice in
/// the switch bytes, located by turning TYP on each track in the emulator
/// (2026-10-06): LP2 0, LP1 6, BP 1, HP1 2, HP2 4, BS 5, PK 3 [measured].
/// The curves (cutoff and resonance scales, drive, envelope shapes) are
/// [inferred].
const OVR: usize = 42;
const FRQ: usize = 58;
const RES: usize = 74;
const AMP_ENV: usize = 168;
const FLT_ENV: usize = 296;
/// (byte, lowest bit) of each voice's filter-type field.
const FLT_TYPE_AT: [(usize, u8); 8] = [(161, 1), (161, 5), (164, 5), (160, 1), (167, 1), (163, 5), (167, 5), (163, 1)];

#[derive(Clone, Copy, PartialEq)]
enum FltType {
    Lp2,
    Lp1,
    Bp,
    Hp1,
    Hp2,
    Bs,
    Pk,
}

fn flt_type(f: &[u8], v: usize) -> FltType {
    let (byte, lo) = FLT_TYPE_AT[v];
    match (f[byte] >> lo) & 7 {
        6 => FltType::Lp1,
        1 => FltType::Bp,
        2 => FltType::Hp1,
        4 => FltType::Hp2,
        5 => FltType::Bs,
        3 => FltType::Pk,
        _ => FltType::Lp2,
    }
}

/// An envelope driven by its 8-byte record. A changed record (start, target or
/// rate) takes effect at once; it jumps to its start level only when it says so:
/// for the amp envelope the first word's start command (0x4000; 0 continues from
/// the current level, as the decay after the hold does) [measured: AMP DEC's
/// decay record is `0000 0080 b01a ..`, start byte 0, while the level is 127];
/// for the filter envelope, whose first word is its depth, a start-type mode
/// nibble (9 or 0xF) [inferred]. The firmware re-sends an unchanged record
/// every block.
#[derive(Clone, Copy, Default)]
struct SegEnv {
    key: (u8, u8, u32),
    level: f32,
    /// Holding (mode 3) until a ramp mode (B, 9, F) comes; mode 0 continues
    /// whichever it was.
    hold: bool,
    /// The last record carried the amp's start command (it lasts two blocks).
    cmd: bool,
}

impl SegEnv {
    /// `cmd_word`: the record's first word is a start command (amp), not a depth.
    fn tick(&mut self, r: &[u8], cmd_word: bool) {
        let key = (r[2], r[3], u32::from_be_bytes([r[4], r[5], r[6], r[7]]) & 0xF_FFFF);
        let rec = Record::parse(r);
        // The amp starts on its start command's first block, also when the
        // record is otherwise the one already playing: a trig during a held note
        // re-sends the same attack (`4000 007f b0..`) and must restart it
        // [measured 2026-10-08: keyed on changes only, every note after the
        // first skipped its attack]. The filter starts on a changed record of a
        // start type.
        let cmd = cmd_word && r[0] | r[1] != 0;
        if cmd_word {
            if cmd && !self.cmd {
                self.level = rec.start;
            }
            self.cmd = cmd;
            self.key = key;
        } else if key != self.key {
            self.key = key;
            if matches!(r[4] >> 4, 0x9 | 0xF) {
                self.level = rec.start;
            }
        }
        // Mode 3 holds the level wherever it is, and mode 0 (the alternate the
        // firmware writes every other block) continues it: with AMP DEC at
        // infinite the sustain is `.. 007f 3000 0000` / `.. 007f 0000 0000` and a
        // pad release turns the target to 0 with the rate field 0, which as a
        // ramp would be an instant drop [measured records; meaning inferred].
        // A hold that carries a rate (AMP ATK above 0: `4000 007f b00b e4f4`, then
        // `0000 007f 300b e4f4`) is the attack still ramping to its target: it
        // holds only once there. With rate 0 it holds at once [measured records,
        // 2026-10-08; with it frozen, any attack above 0 left the voice silent].
        match r[4] >> 4 {
            0x3 => self.hold = key.2 & 0xF_FFFF == 0 || (rec.target - self.level).abs() < 1e-3,
            // Mode 0 continues whatever was going, but one without a rate (the
            // end of the hold time, `0000 0000 0000 0000`) stays where the level
            // is - also when a long attack has not reached the top yet.
            0x0 if key.2 & 0xF_FFFF == 0 => self.hold = true,
            0x9 | 0xB | 0xF => self.hold = false,
            _ => {}
        }
        if !self.hold {
            Env { level: self.level }.tick_into(&rec, &mut self.level);
        }
    }
}

impl Env {
    fn tick_into(mut self, r: &Record, out: &mut f32) {
        self.tick(r);
        *out = self.level;
    }
}

/// DTEMU_AMP_TRACE=V prints voice V's amp envelope record whenever it changes.
fn amp_trace() -> Option<usize> {
    static T: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var("DTEMU_AMP_TRACE").ok().and_then(|x| x.parse().ok()))
}

#[derive(Clone, Copy, Default)]
struct Chain {
    /// DTEMU_AMP_TRACE=V: the amp record last printed for voice V.
    traced: [u8; 8],
    amp: SegEnv,
    flt: SegEnv,
    low: f32,
    band: f32,
    lp1: f32,
}

impl Chain {
    fn process(&mut self, f: &[u8], v: usize, x: f32) -> f32 {
        if amp_trace() == Some(v) {
            let r = &f[AMP_ENV + 8 * v..AMP_ENV + 8 * v + 8];
            if r != self.traced {
                self.traced.copy_from_slice(r);
                eprintln!("amp v{v}: {} level {:.1}", r.iter().map(|b| format!("{b:02x}")).collect::<String>(), self.amp.level);
            }
        }
        // Overdrive: OVR 0 is clean, full scale about 18 dB of drive into a soft clip.
        let ovr = (word(f, OVR + 2 * v) as f32 / 65536.0).clamp(0.0, 1.0);
        let drive = 1.0 + 8.0 * ovr * ovr;
        let x = if drive > 1.01 { (x * drive).tanh() / drive.tanh().max(0.3) } else { x };
        // Filter: cutoff from FRQ plus the envelope (depth x level/127). The
        // word is (FRQ*256 - 0x4000)*2, so FRQ = word/512 + 64; one semitone a
        // step with 440 Hz at 60 (13.8 Hz at 0, 21.1 kHz at 127), measured on
        // the self-oscillating LP2 filter [rytm-mods 0008-filter-hz]; the
        // envelope is taken to move it on the same scale [inferred].
        self.flt.tick(&f[FLT_ENV + 8 * v..FLT_ENV + 8 * v + 8], false);
        let depth = word(f, FLT_ENV + 8 * v) as i16 as f32;
        let cv = word(f, FRQ + 2 * v) as i16 as f32 + depth * (self.flt.level.max(0.0) / 127.0);
        let frq = cv / 512.0 + 64.0;
        let fc = (440.0 * ((frq - 60.0) / 12.0).exp2()).clamp(10.0, 22_000.0);
        let res = (word(f, RES + 2 * v) as f32 / (127.0 * 256.0)).clamp(0.0, 1.0);
        // A topology-preserving state-variable filter (stable to Nyquist):
        // `low` and `band` hold its two integrator states.
        let g = (std::f32::consts::PI * fc.min(FS * 0.45) / FS).tan();
        let k = 2.0 * (1.0 - 0.97 * res);
        let a1 = 1.0 / (1.0 + g * (g + k));
        let (a2, a3) = (g * a1, g * g * a1);
        let v3 = x - self.low;
        let band = a1 * self.band + a2 * v3;
        let low = self.low + a2 * self.band + a3 * v3;
        self.band = 2.0 * band - self.band;
        self.low = 2.0 * low - self.low;
        let high = x - k * band - low;
        let (band, low_out) = (band, low);
        let a = 1.0 - (-std::f32::consts::TAU * fc / FS).exp();
        self.lp1 += a * (x - self.lp1);
        let y = match flt_type(f, v) {
            FltType::Lp2 => low_out,
            FltType::Lp1 => self.lp1,
            FltType::Bp => band,
            FltType::Hp1 => x - self.lp1,
            FltType::Hp2 => high,
            FltType::Bs => low_out + high,
            FltType::Pk => x + band * k * 2.0 * res,
        };
        let y = if y.is_finite() { y } else { self.low = 0.0; self.band = 0.0; 0.0 };
        // Amp envelope (the L copy): 127 is full scale, logarithmic below, 0
        // and under silent.
        self.amp.tick(&f[AMP_ENV + 8 * v..AMP_ENV + 8 * v + 8], true);
        let lv = self.amp.level;
        let gain = if lv <= 0.0 { 0.0 } else { ((lv - 127.0) / LEVELS_PER_6DB).exp2() };
        y * gain
    }
}

/// DAC words 12/13 and 14/15: the FX returns, written negated, joining the
/// analog mix before and after the master distortion [rytm-mods; which is
/// which inferred]. Word +0 of the control frame is the FX track's distortion
/// amount (`0x8000fbce * 2`) [measured].
/// The FX track's live parameters [rytm-mods compressor-hardware.md, OS 1.73]:
/// distortion amount at +0, its symmetry +2, then the compressor THR +6, ATK +8,
/// REL +10, RAT +12, SEQ +14, MUP +16, MIX +18, VOL +20, each 8.8.
pub const FX_PARAMS: u32 = 0x8000_FBCE;

/// Control-frame byte +166: the route-to-main mask, MSB first for physical
/// voices 7,6,5,4,0,1,2,3; a set bit takes the voice off the main bus (its
/// sends and receive word stay) [rytm-mods compressor-hardware.md].
const ROUTE: usize = 166;
const ROUTE_BIT: [u8; 8] = [3, 2, 1, 0, 4, 5, 6, 7];

/// The master compressor, analog on the real unit, modelled on rytm-mods'
/// hardware measurements (compressor-hardware.md, 2026-09): threshold about
/// 0.9 dB per THR step with no reduction from THR 105 (taken as 0 dBFS there);
/// a soft knee (20 dB wide here); stored RAT 0..3 about 10, 6, 3.5 and 2.5 : 1;
/// a peak detector on max(|L|, |R|) through the SEQ sidechain filter; one-pole
/// attack at the labelled time and releases of 0.45 x the label (A1/A2 taken
/// as 0.4/0.6 s); MUP +0.6 dB a step to 48, then to +40.3 dB at 127; the wet
/// path 6.9 dB under the dry at MUP 0 (absolute make-up anchored: default MUP
/// 47 = unity wet); MIX a linear crossfade; VOL from the measured table. The compressor's own control-frame words (+2 THR, +4 RAT,
/// +6/+8 wet and dry gains, byte 162 ATK/REL/SEQ, found by turning each knob)
/// are not decoded; this reads the parameters instead [inferred model].
#[derive(Default)]
struct Comp {
    env: f32,
    sc: (f32, f32),
}

const COMP_ATK_MS: [f32; 7] = [0.03, 0.1, 0.3, 1.0, 3.0, 10.0, 30.0];
const COMP_REL_S: [f32; 8] = [0.1, 0.2, 0.4, 0.6, 1.0, 2.0, 0.4, 0.6];
const COMP_RATIO: [f32; 4] = [10.0, 6.0, 3.5, 2.5];
/// VOL -> dry gain (dB re VOL 127) and wet minus dry (dB), measured.
const COMP_VOL: [(f32, f32, f32); 9] = [
    (40.0, -10.85, -16.57), (52.0, -8.30, -14.18), (64.0, -6.38, -12.35), (76.0, -4.85, -10.86),
    (88.0, -3.44, -9.59), (100.0, -2.20, -8.42), (112.0, -1.13, -7.38), (124.0, -0.21, -6.34), (127.0, 0.0, -6.26),
];

fn db(x: f32) -> f32 {
    (10.0f32).powf(x / 20.0)
}

impl Comp {
    fn process(&mut self, p: &[u8], l: f32, r: f32) -> (f32, f32) {
        let prm = |i: usize| p[6 + 2 * i] as f32; // integer part of each 8.8 word
        let (thr, atk, rel, rat, seq, mup, mix, vol) = (prm(0), prm(1), prm(2), prm(3), prm(4), prm(5), prm(6), prm(7));
        // Sidechain: SEQ 0 OFF, 1 LPF, 2 HPF, 3 HIT (a high-frequency weighting).
        let x = l.abs().max(r.abs());
        let a_lo = 1.0 - (-std::f32::consts::TAU * 400.0 / FS).exp();
        let a_hi = 1.0 - (-std::f32::consts::TAU * 2500.0 / FS).exp();
        let s = l.abs().max(r.abs()) * (l.signum().max(r.signum()));
        self.sc.0 += a_lo * (s - self.sc.0);
        self.sc.1 += a_hi * (s - self.sc.1);
        let det = match seq as u8 {
            1 => self.sc.0.abs() * 1.6,
            2 => (s - self.sc.0).abs(),
            3 => (s - self.sc.1).abs() * 1.6,
            _ => x,
        };
        let ta = COMP_ATK_MS[(atk as usize).min(6)] / 1000.0;
        let tr = 0.45 * COMP_REL_S[(rel as usize).min(7)];
        let coef = |t: f32| (-1.0 / (t.max(1e-5) * FS)).exp();
        let c = if det > self.env { coef(ta) } else { coef(tr) };
        self.env = det + c * (self.env - det);
        // Static curve, soft knee.
        let lvl = 20.0 * self.env.max(1e-6).log10();
        let t_db = -0.9 * (105.0 - thr);
        let ratio = COMP_RATIO[(rat as usize).min(3)];
        let over = lvl - t_db;
        let knee = 20.0;
        let soft = if over <= -knee / 2.0 { 0.0 } else if over >= knee / 2.0 { over } else { (over + knee / 2.0).powi(2) / (2.0 * knee) };
        let gr = soft * (1.0 - 1.0 / ratio);
        let mup_db = |m: f32| if m <= 48.0 { 0.6 * m } else { 28.8 + (m - 48.0) * (40.3 - 28.8) / 79.0 };
        // Absolute make-up is not measured: anchored so the default MUP (47)
        // with no reduction leaves the wet path at unity [inferred].
        let makeup = mup_db(mup) - mup_db(47.0);
        let (dry_db, wet_rel) = {
            let mut prev = COMP_VOL[0];
            let mut out = (prev.1, prev.2);
            if vol < prev.0 {
                out = (prev.1 - (prev.0 - vol) * 0.2, prev.2);
            }
            for &e in &COMP_VOL[1..] {
                if vol <= e.0 && vol >= prev.0 {
                    let k = (vol - prev.0) / (e.0 - prev.0);
                    out = (prev.1 + k * (e.1 - prev.1), prev.2 + k * (e.2 - prev.2));
                }
                prev = e;
            }
            out
        };
        let dry = db(dry_db);
        let wet = db(dry_db - gr + makeup);
        let _ = wet_rel; // the measured wet-minus-dry offset is folded into the anchor
        let m = (mix / 127.0).clamp(0.0, 1.0);
        let g = dry * (1.0 - m) + wet * m;
        (l * g, r * g)
    }
}

/// The external inputs for one frame (-1..1 at the host's full scale).
/// AUDIO IN reaches the firmware only, through the receive frame's words 8/9
/// (sampling, USB). EXT IN is analog and joins the mix after the distortion and
/// before the compressor [rytm-mods compressor-hardware.md, measured].
#[derive(Clone, Copy, Default)]
pub struct Inputs {
    pub audio: (f32, f32),
    pub ext: (f32, f32),
}

const DAC_PRE: usize = 12;
const DAC_POST: usize = 14;
const DIST: usize = 0;

impl Analog {
    /// `dac`: the last complete DAC frame (digital layers 0..7, tom exciters
    /// 8..10, FX returns 12..15).
    /// `fx`: the FX track's live parameters (`FX_PARAMS`, 24 bytes).
    pub fn frame(&mut self, f: &[u8], dac: &[i32; 16], fx: &[u8], inp: Inputs) -> FrameOut {
        let mut rx = [0i32; 12];
        let mut src = [0.0f32; 8];
        let w = &mut self.wobble;
        for d in self.duals.iter_mut() {
            src[d.spec.voice] = d.sample(f, w);
        }
        for t in self.toms.iter_mut() {
            src[t.spec.voice] = t.sample(f, dac, w);
        }
        src[CYM_VOICE] = self.cym.sample(f, w);
        src[HAT_VOICE] = self.hat.sample(f, w);
        let mut mix = (0.0f32, 0.0f32);
        for v in 0..8 {
            // The voice's sample/noise layer joins its analog source.
            let x = src[v] + dac[v] as f32 / 8_388_608.0 * 0.5;
            let y = self.chains[v].process(f, v, x);
            rx[v] = to24(y * 0.5);
            if f[ROUTE] >> ROUTE_BIT[v] & 1 != 0 {
                continue; // ROUTE TO MAIN off
            }
            mix.0 += y * word(f, VCA_L + 2 * v) as f32 / 32767.0;
            mix.1 += y * word(f, VCA_R + 2 * v) as f32 / 32767.0;
        }
        let ret = |i: usize| -(dac[i] as f32) / 8_388_608.0;
        mix.0 += ret(DAC_PRE);
        mix.1 += ret(DAC_PRE + 1);
        // Master distortion [inferred curve].
        let dist = (word(f, DIST) as f32 / 65536.0).clamp(0.0, 1.0);
        if dist > 0.0 {
            let k = 1.0 + 15.0 * dist;
            let n = k.tanh();
            mix = ((mix.0 * k).tanh() / n, (mix.1 * k).tanh() / n);
        }
        mix.0 += ret(DAC_POST);
        mix.1 += ret(DAC_POST + 1);
        mix.0 += inp.ext.0;
        mix.1 += inp.ext.1;
        mix = self.comp.process(fx, mix.0, mix.1);
        rx[RX_AUDIO_IN] = to24(inp.audio.0 * 0.5);
        rx[RX_AUDIO_IN + 1] = to24(inp.audio.1 * 0.5);
        self.frames += 1;
        if let Some((offs, last)) = &mut self.trace {
            let now: Vec<u16> = offs.iter().map(|&o| word(f, o)).collect();
            if now != *last {
                let s: Vec<String> = offs.iter().zip(&now).map(|(o, v)| format!("{o}:{v:04x}")).collect();
                eprintln!("frame {:>8} {}", self.frames, s.join(" "));
                *last = now;
            }
        }
        if self.frames.is_multiple_of(96) && std::env::var_os("DTEMU_ANALOG_TRACE").is_some() {
            let d = &self.duals[0];
            let ar = Record::parse(&f[d.spec.env + 16..]);
            eprintln!(
                "analog {:>8} BD trig {} pitch {:6.1} amp {:6.1} (target {:4} step {:.4}) f1 {:6.1} | SD amp {:6.1} RS amp {:6.1}",
                self.frames, bit(f, d.spec.sw[0]) as u8, d.pitch.level, d.amp.level, ar.target, ar.step,
                word(f, d.spec.cv) as f32 * HZ_PER_TUNE, self.duals[1].amp.level, self.duals[2].amp.level
            );
        }
        rx[RX_MAIN] = to24(mix.0 * 0.5);
        rx[RX_MAIN + 1] = to24(mix.1 * 0.5);
        FrameOut { rx, mix }
    }
}
