//! The Syntakt's second CPU ("DSP") and the link to it (docs/SYNTAKT-PORT.md).
//!
//! The Syntakt has two MCF5441x ColdFires. CPU1 runs MAIN OS (the machine
//! this emulator runs); CPU2 renders the eight digital voices from section 7
//! of the firmware. They share a 64 KB dual-port RAM in an FPGA on FlexBus
//! CS0 of both (CPU1 `0x10000000`, CPU2 `0`) and two lines [measured, OS
//! 1.42 code on both sides]:
//!
//! - **CPU1 -> DSP ("kick")**: CPU1 toggles GPIO port A bit 4 (`0x40082f6a`:
//!   PPDSDR_A `0xEC094018` = 0x10, then PCLRR_A `0xEC094024` = 0xEF). The DSP
//!   takes every edge on its DTIN0 (DTIM0 in capture mode, DTMR `0xc1`,
//!   vector 96): during boot its sync routine (`0x40000b20`) waits for one;
//!   once running each edge starts an audio block (ISR `0x40000934`).
//!   CPU1's audio interrupt kicks once a block (`0x400a2092`).
//! - **DSP -> CPU1 ("irq")**: the DSP toggles a GPIO (`0x4000090e`); CPU1
//!   takes each edge as EPORT IRQ4 (both edges, EPPAR `|= 0x300`; vector 68,
//!   ISR `0x40083034`, which clears EPFR bit 4).
//!
//! The boot (CPU1 task `0x40082c04`): CPU1 streams `FF x 8` and section 6
//! (the DSP's serial-boot loader) with its byte 1 set to `0F`, 1756 bytes,
//! out of DSPI2 in slave mode by eDMA channel 29 to the DSP's boot port
//! [measured: the bytes captured here]; the done interrupt (`0x400831d6`) re-arms
//! channel 29 with `AA, u32 length` and section 7. Then the mailbox
//! handshake, CPU1's state at `0x41bb32b0`, the DSP's progress code at +8:
//!
//! | step | DSP (section 7, `0x40000db4`) | CPU1 (ISR `0x40083034`) |
//! |---|---|---|
//! | 1 | memory test of +0/+4, then `'HO' 'HA'` and its version at +0/+2/+4 (`'NO' 'NO'` on failure), irq | s0: kick, s1 (once the section 6 stream is done) |
//! | 2 | woken: +0 must read `0x4230` (`'B0'`), else back to 1 | s1: `HO HA` seen, version kept (`0x4029aacc`); task `0x40083288` tests +0/+4 for 2 s, writes +2 = 1, +0 = `'B0'`, kicks |
//! | 3 | +4 = `0xA5A5`, irq | s2: +4 kept, +2 = 2, +0xC = 0, kick, s3 |
//! | 4 | per kick while +2 = 2: copy 2 KB from +0x800 (block +0xC), irq | s3: task `0x40082f90` copies the next block of the blob at `0x4d84f2c0` (64 card slots read by `0x40082334`), kicks; after the last, s4 |
//! | 5 | +2 = 3 seen: voice init, irq | s4: +2 = 3, kick, s5 |
//! | 6 | the block ISR: 432 B of parameters from +0, 1 KB of audio to +0x1B0, irq | s5: +4 was `0xA5A5`: DSP state `0x41bbf448` = 6 (ok), running (`0x43bdd824`); s6 counts |
//!
//! [`DspLink`] is that link as CPU1 sees it: the mailbox RAM, the two lines,
//! the boot stream. The far side is either a second CPU running section 7
//! (stage 2, the default: `Machine::dsp`, its peripherals' link end in
//! [`DspSide`]) or [`StandIn`], a script of section 7's main flow that
//! answers the handshake and then each block with silence (stage 1;
//! `DTEMU_SYNTAKT_DSP=stub`).

use serde::{Deserialize, Serialize};

pub const MBOX_BASE: u32 = 0x1000_0000;
pub const MBOX_SIZE: u32 = 0x1_0000;
/// eDMA channel of CPU1's boot stream (DSPI2 slave transmit).
pub const BOOT_CHAN: usize = 29;
/// The DSP's audio block in the mailbox: 1 KB rendered at +0x1B0, read by
/// CPU1's eDMA channel 47 into SRAM `0x80003d50` (`0x400a47fa`) [measured].
pub const AUDIO_OFF: usize = 0x1B0;
pub const AUDIO_LEN: usize = 0x400;

/// Instructions from a kick to the DSP's answer [inferred: the sync routine
/// answers at once; a block's render takes longer, which nothing here needs].
const LATENCY: u64 = 4_000;
/// From the end of the section 6 stream to the DSP's first `HO HA`: its
/// loader takes section 7 in, it clears 64 MB of BSS, configures its FPGA and
/// tests the mailbox (ten reads 100000 loops apart) [inferred: about 50 ms].
const BOOT_TIME: u64 = 10_000_000;
/// A real DSP: from the end of the section 6 stream to section 7's entry,
/// while the loader takes section 7 in [inferred: about 5 ms; the loader is
/// not run].
const LOAD_TIME: u64 = 1_000_000;
/// The DSP's own mailbox test before each `HO HA` [inferred: about 5 ms].
const MEMTEST_TIME: u64 = 1_000_000;

/// Section 7 (the DSP program) of Syntakt OS 1.42 [measured: sha256 of the
/// section as `fwinfo` extracts it].
pub const DSP_142_SHA256: &str = "daf6451cf9587c0b628e901b7bb6b25f4e2633d448c534c0c181dd35ec783bc2";
/// Where section 7 is linked and loaded (its word 0 is the entry).
pub const DSP_LOAD: u32 = 0x4000_0400;
/// The DSP's DDR: its BSS reaches 0x4404f980, so more than 64 MB
/// (`0x40000512`) [measured]; 128 MB as CPU1's [inferred].
pub const DSP_DDR_MB: usize = 128;

/// Guest addresses in section 7, per build (all [measured] by disassembly of
/// the image whose hash this carries).
#[derive(Clone, Debug)]
pub struct DspProfile {
    pub sha256: String,
    /// Word 0 of the section: `0x40001070`.
    pub entry: u32,
    /// Spins that only an interrupt can end, run as wait-for-interrupt:
    /// `0x4000106e` (`bra.b *`, the main loop after init: everything runs in
    /// the block ISR) and `0x40000b34` (the sync routine's wait for a kick,
    /// `0x40000b20`: the DTIM0 ISR `0x40000442` clears `0x4404f750`).
    pub idle_spins: Vec<u32>,
}

impl DspProfile {
    pub fn for_image(img: &[u8]) -> Result<DspProfile, String> {
        use sha2::{Digest, Sha256};
        let h: String = Sha256::digest(img).iter().map(|b| format!("{b:02x}")).collect();
        match h.as_str() {
            DSP_142_SHA256 => Ok(DspProfile { sha256: h, entry: 0x4000_1070, idle_spins: vec![0x4000_106E, 0x4000_0B34] }),
            _ => Err(format!("DSP program (section 7) sha256 {h} has no profile here; known: Syntakt OS 1.42")),
        }
    }
}

/// The DSP's end of the link, in the DSP's own `Io` (`Io::dsp_side`): the
/// mailbox at its FlexBus CS0 (address 0; the RAM itself is moved in from
/// `DspLink::mbox` for each slice the DSP runs), the kick edges due on its
/// DTIN0, and the IRQ edges it made on its port G bit 2 [measured: section
/// 7 `0x4000090e` toggles PPDSDR_G `0xEC09401E` = 4 / PCLRR_G `0xEC09402A` =
/// 0xFB; FPGA configuration `0x40000706` reads INIT_B on PPDSDR_E bit 0 and
/// DONE on PPDSDR_F bit 7].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DspSide {
    /// Empty between the DSP's slices (the RAM is `DspLink::mbox` then).
    #[serde(skip)]
    pub mbox: Vec<u8>,
    /// Clocks of CPU1's kick edges not yet taken by DTIM0.
    pub kicks: std::collections::VecDeque<u64>,
    /// The IRQ line (port G bit 2) as last driven, and its edges' clocks.
    pub irq_line: bool,
    pub irq_edges: Vec<u64>,
    pub kicks_taken: u64,
    /// Rendered blocks, kept while `Some` (`Machine::dsp_capture`): each as
    /// frames of 8 words, tracks 1..8.
    #[serde(skip)]
    pub capture: Option<Vec<i32>>,
    pub blocks: u64,
}

/// The DSP's audio block: 8 tracks x 32 samples, track-major (track t's at
/// +128 t), signed 32-bit words; section 7's block ISR renders track t into
/// SRAM `0x80003840 + 128 t` (`0x40000a78..0x40000aa2`) and eDMA channel 47
/// moves the 1 KB to the mailbox at +0x1B0 (`0x40000ac6`) [measured].
pub const TRACKS: usize = 8;
pub const BLOCK_FRAMES: usize = 32;

impl DspSide {
    pub fn read(&self, a: u32, size: u32) -> u32 {
        (0..size as usize).fold(0, |v, k| v << 8 | self.mbox.get((a as usize + k) & 0xFFFF).copied().unwrap_or(0) as u32)
    }

    pub fn write(&mut self, a: u32, size: u32, v: u32) {
        for k in 0..size as usize {
            if let Some(b) = self.mbox.get_mut((a as usize + k) & 0xFFFF) {
                *b = (v >> (8 * (size as usize - 1 - k))) as u8;
            }
        }
        // The block's last word: the block is complete.
        let end = (AUDIO_OFF + AUDIO_LEN - 4) as u32;
        if a <= end && end < a + size {
            self.blocks += 1;
            if let Some(c) = self.capture.as_mut() {
                let w = |t: usize, f: usize| {
                    let o = AUDIO_OFF + 4 * (BLOCK_FRAMES * t + f);
                    i32::from_be_bytes(self.mbox[o..o + 4].try_into().unwrap())
                };
                for f in 0..BLOCK_FRAMES {
                    c.extend((0..TRACKS).map(|t| w(t, f)));
                }
            }
        }
    }

    /// The DSP drove its IRQ line.
    pub fn set_irq_line(&mut self, high: bool, now: u64) {
        if high != self.irq_line {
            self.irq_line = high;
            self.irq_edges.push(now);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Phase {
    /// In reset, or not yet streamed to.
    #[default]
    Off,
    /// The boot stream is open (eDMA channel 29 is served).
    Streaming,
    /// Loaded; says `HO HA` at `at`.
    Hello,
    /// `HO HA` said; waits for a kick that finds `'B0'` at +0.
    WaitB0,
    /// `0xA5A5` said; takes data blocks until +2 = 3.
    Blocks,
    /// Voices set up; renders a block per kick.
    Running,
}

/// A scripted DSP: section 7's main flow, as far as CPU1 can see it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StandIn {
    pub phase: Phase,
    /// The clock of a pending step (`Hello`'s answer).
    pub at: Option<u64>,
    /// eDMA channel 29's completed major loops when the stream opened.
    stream_majors: u64,
    /// The DSP's version word (section 7 at `0x4004005a`).
    pub version: u16,
    /// Data blocks taken, and audio blocks answered.
    pub blocks: u64,
    pub frames: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DspLink {
    /// The FPGA's dual-port RAM.
    pub mbox: Vec<u8>,
    /// CPU1's kick line (port A bit 4) as last written.
    pub kick_line: bool,
    /// Edges seen on the kick line.
    pub kicks: u64,
    /// IRQ edges to CPU1 waiting for their clock.
    pub irq_at: Option<u64>,
    pub irqs: u64,
    /// The bytes CPU1 streamed to the DSP's boot port (DSPI2 PUSHR).
    #[serde(skip)]
    pub boot_stream: Vec<u8>,
    pub dsp: StandIn,
    /// A real second CPU answers (`Machine::dsp`), not the stand-in: kicks
    /// are queued for it (`kick_times`), and the end of the section 6 stream
    /// asks for its start (`boot_at`).
    pub real: bool,
    pub kick_times: Vec<u64>,
    /// The clock at which the DSP leaves reset with its program loaded.
    pub boot_at: Option<u64>,
}

impl DspLink {
    pub fn new(version: u16) -> DspLink {
        DspLink {
            mbox: vec![0; MBOX_SIZE as usize],
            kick_line: false,
            kicks: 0,
            irq_at: None,
            irqs: 0,
            boot_stream: Vec::new(),
            dsp: StandIn { version, ..Default::default() },
            real: false,
            kick_times: Vec::new(),
            boot_at: None,
        }
    }

    pub fn read(&self, a: u32, size: u32) -> u32 {
        let o = (a - MBOX_BASE) as usize;
        (0..size as usize).fold(0, |v, k| v << 8 | self.mbox[(o + k) & 0xFFFF] as u32)
    }

    pub fn write(&mut self, a: u32, size: u32, v: u32) {
        let o = (a - MBOX_BASE) as usize;
        for k in 0..size as usize {
            self.mbox[(o + k) & 0xFFFF] = (v >> (8 * (size as usize - 1 - k))) as u8;
        }
    }

    fn w16(&self, o: usize) -> u16 {
        u16::from_be_bytes([self.mbox[o], self.mbox[o + 1]])
    }

    fn set16(&mut self, o: usize, v: u16) {
        self.mbox[o..o + 2].copy_from_slice(&v.to_be_bytes());
    }

    /// The DSP raises its IRQ line (an edge) `delay` from `now`.
    pub fn raise(&mut self, now: u64, delay: u64) {
        let t = now + delay;
        self.irq_at = Some(self.irq_at.map_or(t, |a| a.min(t)));
    }

    /// CPU1 opened the boot stream (DSPI2 MCR written out of HALT): the DSP
    /// leaves reset and clocks it in. `majors`: channel 29's count now.
    pub fn stream_open(&mut self, majors: u64) {
        self.boot_stream.clear();
        self.dsp = StandIn { version: self.dsp.version, phase: Phase::Streaming, stream_majors: majors, ..Default::default() };
        self.irq_at = None;
        self.boot_at = None;
        self.kick_times.clear();
    }

    /// CPU1 holds the DSP in reset (PCLRR_C bit 4 low, `0x40082c6e`).
    pub fn hold_reset(&mut self) {
        self.dsp.phase = Phase::Off;
        self.boot_at = None;
        self.kick_times.clear();
    }

    /// Whether the DSP still takes boot-stream bytes (channel 29's request):
    /// the first major loop only, section 6. Section 7's stream, re-armed by
    /// CPU1's done interrupt, is not clocked: the stand-in needs no program.
    pub fn streaming(&self, majors: u64) -> bool {
        self.dsp.phase == Phase::Streaming && majors == self.dsp.stream_majors
    }

    /// A byte CPU1's DMA wrote to the boot port.
    pub fn boot_byte(&mut self, b: u8) {
        if self.boot_stream.len() < 1 << 20 {
            self.boot_stream.push(b);
        }
    }

    /// CPU1 wrote its kick line. -> whether a step is now pending (`irq_at`).
    pub fn set_kick_line(&mut self, high: bool, now: u64) {
        if high == self.kick_line {
            return;
        }
        self.kick_line = high;
        self.kicks += 1;
        if self.real {
            if self.dsp.phase != Phase::Off {
                self.kick_times.push(now);
            }
        } else {
            self.on_kick(now);
        }
    }

    /// Time passes: the stream's end and the DSP's own steps. -> the clock of
    /// the next thing due (for the I/O deadline).
    pub fn service(&mut self, now: u64, majors: u64) -> Option<u64> {
        if self.dsp.phase == Phase::Streaming && majors != self.dsp.stream_majors {
            self.dsp.phase = Phase::Hello;
            if self.real {
                // Section 6 is in; it fetches section 7 and jumps to it.
                // The core starts with section 7 loaded (`Machine::dsp_boot`).
                self.dsp.phase = Phase::Running;
                self.boot_at = Some(now + LOAD_TIME);
                return self.irq_at;
            }
            self.dsp.at = Some(now + BOOT_TIME);
        }
        if self.dsp.phase == Phase::Hello {
            if let Some(t) = self.dsp.at {
                if now >= t {
                    self.dsp.at = None;
                    self.hello(now);
                }
            }
        }
        match (self.dsp.at, self.irq_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The edge due now, if any. -> true when CPU1's IRQ4 flag should set.
    pub fn take_irq(&mut self, now: u64) -> bool {
        match self.irq_at {
            Some(t) if now >= t => {
                self.irq_at = None;
                self.irqs += 1;
                true
            }
            _ => false,
        }
    }

    /// The DSP's mailbox test, then `'HO' 'HA'` and its version (section 7,
    /// `0x40000e6a..0x40000ed2`), then the sync: an IRQ edge.
    fn hello(&mut self, now: u64) {
        self.mbox[0..8].copy_from_slice(&[0xAA, 0xAA, 0xAA, 0xAA, 0x55, 0x55, 0x55, 0x55]);
        self.set16(0, 0x484F);
        self.set16(2, 0x4841);
        let v = self.dsp.version;
        self.set16(4, v);
        self.set16(8, 4);
        self.dsp.phase = Phase::WaitB0;
        self.raise(now, LATENCY);
    }

    fn on_kick(&mut self, now: u64) {
        match self.dsp.phase {
            Phase::WaitB0 => {
                // 0x40000efe..0x40000f2a: woken, +0 must hold 'B0'.
                self.set16(8, 5);
                self.set16(6, 0);
                self.set16(8, 6);
                if self.w16(0) == 0x4230 {
                    // 0x40000f3a: the answer, then the sync.
                    self.set16(8, 8);
                    self.set16(4, 0xA5A5);
                    self.set16(8, 9);
                    self.dsp.phase = Phase::Blocks;
                    self.raise(now, LATENCY);
                } else {
                    // Not yet: test the mailbox again and say hello again.
                    self.dsp.phase = Phase::Hello;
                    self.dsp.at = Some(now + MEMTEST_TIME);
                }
            }
            Phase::Blocks => {
                // 0x40000f8e: +2 = 3 ends the data; else take block +0xC.
                self.set16(8, 0xA);
                if self.w16(2) == 3 {
                    self.set16(8, 0xB);
                    self.set16(8, 0xC);
                    self.dsp.phase = Phase::Running;
                } else {
                    self.dsp.blocks += 1;
                }
                self.raise(now, LATENCY);
            }
            Phase::Running => {
                // A block: silence at +0x1B0 (left as is: zero), and the edge.
                self.dsp.frames += 1;
                self.raise(now, LATENCY);
            }
            Phase::Off | Phase::Streaming | Phase::Hello => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The handshake as CPU1 drives it: the DSP's answers at each step.
    #[test]
    fn handshake() {
        let mut l = DspLink::new(7);
        l.stream_open(0);
        assert!(l.streaming(0));
        l.service(100, 1);
        assert!(!l.streaming(1));
        let t = l.service(100 + BOOT_TIME, 1).unwrap();
        assert_eq!(l.read(MBOX_BASE, 4), 0x484F_4841);
        assert_eq!(l.read(MBOX_BASE + 4, 2), 7);
        assert!(l.take_irq(t));
        // A kick before 'B0': back to hello.
        l.set_kick_line(true, t);
        assert_eq!(l.dsp.phase, Phase::Hello);
        let t = l.service(t + MEMTEST_TIME, 1).unwrap();
        assert!(l.take_irq(t));
        l.write(MBOX_BASE + 2, 2, 1);
        l.write(MBOX_BASE, 2, 0x4230);
        l.set_kick_line(false, t);
        assert_eq!(l.read(MBOX_BASE + 4, 2), 0xA5A5);
        assert_eq!(l.dsp.phase, Phase::Blocks);
        l.write(MBOX_BASE + 2, 2, 2);
        l.set_kick_line(true, t);
        l.write(MBOX_BASE + 2, 2, 3);
        l.set_kick_line(false, t);
        assert_eq!(l.dsp.phase, Phase::Running);
        assert_eq!(l.dsp.blocks, 1);
    }

    /// The DSP's end: CPU1's kicks set DTIM0's capture flag at their clocks,
    /// port G bit 2 makes IRQ edges, the mailbox answers at 0 and a complete
    /// audio block is captured as frames.
    #[test]
    fn dsp_side() {
        let mut b = crate::bus::Bus::new(16);
        b.io.dsp_side = Some(Box::new(DspSide { irq_line: true, mbox: vec![0; MBOX_SIZE as usize], capture: Some(Vec::new()), ..Default::default() }));
        b.write16(0xFC07_0000, 0xC1); // DTMR: capture on either edge
        b.io.dsp_side.as_mut().unwrap().kicks.push_back(100);
        b.io.service();
        assert_eq!(b.io.deadline, 100);
        assert_eq!(b.io.dtim[0].dter & 1, 0);
        b.io.now = 100;
        b.io.service();
        assert_eq!(b.io.dtim[0].dter & 1, 1);
        b.write8(0xEC09_402A, 0xFB); // PCLRR_G: the line goes low
        b.write8(0xEC09_402A, 0xFB); // no edge: already low
        b.write8(0xEC09_401E, 0x04); // PPDSDR_G: high again
        assert_eq!(b.io.dsp_side.as_ref().unwrap().irq_edges, vec![100, 100]);
        assert_eq!(b.read8(0xEC09_401C) & 1, 1); // INIT_B
        assert_eq!(b.read8(0xEC09_401D) & 0x80, 0x80); // DONE
        b.write32(0, 0x484F_4841);
        assert_eq!(b.io.dsp_side.as_ref().unwrap().mbox[..4], [0x48, 0x4F, 0x48, 0x41]);
        for t in 0..TRACKS {
            for f in 0..BLOCK_FRAMES {
                b.write32((AUDIO_OFF + 4 * (BLOCK_FRAMES * t + f)) as u32, (100 * t + f) as u32);
            }
        }
        let s = b.io.dsp_side.as_ref().unwrap();
        assert_eq!(s.blocks, 1);
        let c = s.capture.as_ref().unwrap();
        assert_eq!(c.len(), TRACKS * BLOCK_FRAMES);
        assert_eq!((c[0], c[1], c[8], c[8 * 31 + 7]), (0, 100, 1, 731));
    }
}
