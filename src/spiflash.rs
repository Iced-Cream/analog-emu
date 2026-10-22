//! The MKII units' and the Digitakt's serial flash on DSPI0 (`0xFC05C000`),
//! chip select PCS1: 16 MB of SPI NOR holding the OS container at 0x80000
//! (`machine::FLASH_SLOT`), the analog calibration and older state.
//!
//! The OS drives it by polling: `flash_read` (Rytm MKII OS 1.74 `0x40136482`)
//! clears the FIFOs (MCR `|= 0xC00`), pushes `0x84020003` (CONT, CTCNT, PCS1,
//! READ), three address bytes, then a dummy word per byte, each with CONT
//! until the last; it pops one POPR word per pushed word, four behind
//! [measured]. The machine still serves `flash_read` itself (one call instead
//! of a loop per byte) from the same image, so reads see what was written.
//!
//! Modelled, the JEDEC command set common to 24-bit-address SPI NOR parts:
//!
//! ```text
//!   06 / 04          write enable / disable (WEL)
//!   05               read status (WIP always 0, WEL), repeated while selected
//!   03 a a a / 0B a a a d   read / fast read
//!   02 a a a data    page program: bits only clear, wraps in its page
//!   D8 a a a         erase a sector (the part's: 256 KB on the default)
//!   20 / 52 a a a    erase 4 KB / 32 KB, on parts that have them
//!   C7 / 60          erase the chip
//!   9F               JEDEC ID (`SpiFlash::id`)
//!   30               clear status (Spansion's CLSR; the OS sends it after
//!                    an error bit, 0x60, which never sets here)
//! ```
//!
//! Program and erase need WEL and start when the chip select goes high (a
//! word without CONT). They then take the part's time on the emulated clock
//! (`Part::t_*`, datasheet typicals [inferred]): status reads WIP (and WEL)
//! meanwhile, other commands are ignored, and the change lands in the image
//! when the time is up (`settle`). A power cut (`cut`) leaves what `CutMode`
//! says of an operation under way (`DTEMU_SPI_CUT`: share, none or done;
//! how far a part gets is not specified anywhere, so this is a setting). `DTEMU_SPI_INSTANT=1` completes them at once. The part is `Part`: by default a contributor's Rytm MKII's,
//! read with the factory-test console's #STATUS (FLASH `01 2018 00`,
//! 2026-10-22) [measured]. Any other command is counted in `unknown` and
//! answers 0xFF. Nothing here reaches real hardware: the image is a byte vector
//! (written back to a file only by the tools' `--flash` option).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

pub const DSPI0_MCR: u32 = 0xFC05_C000;
pub const DSPI0_PUSHR: u32 = 0xFC05_C034;
pub const DSPI0_POPR: u32 = 0xFC05_C038;
/// PUSHR's PCS field (bits 23:16) for the flash.
const PCS_FLASH: u32 = 0x0002_0000;

/// A flash part: its 9F answer and geometry. The OS's part check (Rytm MKII
/// OS 1.74 `0x40136b50`, on the first five 9F bytes) knows Spansion 01 2018
/// (byte 4 = 0: 512-byte pages, 256 KB sectors; 1: 256-byte pages, 64 KB
/// sectors), ISSI 9D 6017/6018, Macronix C2 2017/2018, Winbond EF
/// 4014/7015/4017/4018 and EON 1C 7015 (256-byte pages, 64 KB sectors)
/// [measured]. Any other ID fails every program and erase.
#[derive(Clone, Copy, Debug)]
pub struct Part {
    pub id: [u8; 6],
    pub page: usize,
    /// What D8 erases.
    pub sector: usize,
    /// 20 (4 KB) and 52 (32 KB) erases exist.
    pub small_erases: bool,
    /// Typical times in microseconds: a page program, a D8 sector erase, a
    /// 4 KB and a 32 KB erase (when the part has them), the chip erase.
    pub t_page: f64,
    pub t_sector: f64,
    pub t_4k: f64,
    pub t_32k: f64,
    pub t_chip: f64,
}

impl Part {
    /// How long an operation takes, in microseconds.
    fn time(&self, op: &Op) -> f64 {
        match *op {
            Op::Program { .. } => self.t_page,
            Op::Erase { len: 0x1000, .. } if self.small_erases => self.t_4k,
            Op::Erase { len: 0x8000, .. } if self.small_erases => self.t_32k,
            Op::Erase { len, .. } if len as usize == self.sector => self.t_sector,
            Op::Erase { .. } => self.t_chip,
        }
    }
}

/// Spansion S25FL128S with uniform 256 KB sectors: 01 2018, byte 4 = 00, as
/// a contributor's Rytm MKII reports [measured]; bytes 3 and 5 (4D, 80) are the
/// datasheet's [inferred: the console prints neither]. Times: the datasheet's
/// typicals, 340 us a 512-byte page, 520 ms a 256 KB sector, 33 s the chip
/// [inferred].
pub const S25FL128S_256K: Part = Part {
    id: [0x01, 0x20, 0x18, 0x4D, 0x00, 0x80],
    page: 512,
    sector: 0x4_0000,
    small_erases: false,
    t_page: 340.0,
    t_sector: 520_000.0,
    t_4k: 0.0,
    t_32k: 0.0,
    t_chip: 33_000_000.0,
};

/// A Winbond W25Q128 (EF 4018), another part the OS accepts (typicals: 0.7
/// ms a page; 45 / 120 / 150 ms a 4 / 32 / 64 KB erase; 40 s the chip).
pub const W25Q128: Part = Part {
    id: [0xEF, 0x40, 0x18, 0x00, 0x00, 0x00],
    page: 256,
    sector: 0x1_0000,
    small_erases: true,
    t_page: 700.0,
    t_sector: 150_000.0,
    t_4k: 45_000.0,
    t_32k: 120_000.0,
    t_chip: 40_000_000.0,
};

/// What a power cut leaves of an operation under way (`DTEMU_SPI_CUT`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutMode {
    /// The share of its bytes its elapsed time covers (the default).
    Share,
    /// Nothing: the bytes stay as they were.
    None,
    /// All of it, as if it had finished.
    Done,
}

impl CutMode {
    pub fn from_env() -> CutMode {
        match std::env::var("DTEMU_SPI_CUT").as_deref() {
            Ok("none") => CutMode::None,
            Ok("done") => CutMode::Done,
            _ => CutMode::Share,
        }
    }
}

/// An operation under way: the bytes it leaves, at their addresses, in the
/// order the part works through them, and its clock span.
#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    op: Op,
    bytes: Vec<(usize, u8)>,
    start: u64,
    end: u64,
}

/// What a snapshot keeps of the flash: its contents (LZ4) and the state a
/// running program can see (WEL, an operation under way and its clock span,
/// the counters). The part and the host settings stay the running machine's.
#[derive(Serialize, Deserialize)]
pub struct FlashState {
    mem_lz4: Vec<u8>,
    wel: bool,
    pending: Option<Pending>,
    erased_bytes: u64,
    programmed_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Erase { at: u32, len: u32 },
    Program { at: u32, len: u32 },
}

pub struct SpiFlash {
    pub mem: Vec<u8>,
    /// The part: what 9F answers (the OS reads five bytes) and its geometry.
    pub part: Part,
    wel: bool,
    /// Bytes into the current selection, and its command and address.
    idx: u32,
    cmd: u8,
    addr: u32,
    /// Page program's data, applied at deselect.
    prog: Vec<u8>,
    rx: VecDeque<u32>,
    /// Written since load (or since the tools last saved it).
    pub dirty: bool,
    pub erased_bytes: u64,
    pub programmed_bytes: u64,
    /// Commands not modelled, by opcode -> count.
    pub unknown: BTreeMap<u8, u64>,
    /// Program and erase operations, in order (`DTEMU_SPI_TRACE=1` prints them
    /// as they happen, `=all` every selection).
    pub ops: Vec<Op>,
    pending: Option<Pending>,
    /// `DTEMU_SPI_INSTANT=1`: operations complete at once.
    pub instant: bool,
    pub cut_mode: CutMode,
    trace: bool,
    trace_all: bool,
}

impl SpiFlash {
    pub fn new(mem: Vec<u8>) -> SpiFlash {
        SpiFlash {
            mem,
            part: S25FL128S_256K,
            wel: false,
            idx: 0,
            cmd: 0,
            addr: 0,
            prog: Vec::new(),
            rx: VecDeque::new(),
            dirty: false,
            erased_bytes: 0,
            programmed_bytes: 0,
            unknown: BTreeMap::new(),
            ops: Vec::new(),
            pending: None,
            instant: std::env::var_os("DTEMU_SPI_INSTANT").is_some(),
            cut_mode: CutMode::from_env(),
            trace: std::env::var_os("DTEMU_SPI_TRACE").is_some(),
            trace_all: std::env::var("DTEMU_SPI_TRACE").is_ok_and(|v| v == "all"),
        }
    }

    /// MCR written: CLR_TXF / CLR_RXF (bits 11, 10) empty the receive queue.
    pub fn mcr(&mut self, v: u32) {
        if v & 0x400 != 0 {
            self.rx.clear();
        }
    }

    pub fn pop(&mut self) -> u32 {
        self.rx.pop_front().unwrap_or(0xFF)
    }

    /// A PUSHR word at clock `now` (instructions; `ips` of them a second).
    pub fn push(&mut self, v: u32, now: u64, ips: f64) {
        self.settle(now);
        let out = if v & 0x00FF_0000 == PCS_FLASH { self.byte(v as u8) } else { 0xFF };
        if self.rx.len() < 64 {
            self.rx.push_back(out as u32);
        }
        if v & 0x8000_0000 == 0 {
            self.deselect(now, ips);
        }
    }

    /// An operation is under way.
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Finish an operation whose time is up by clock `now` (u64::MAX: any).
    pub fn settle(&mut self, now: u64) {
        if self.pending.as_ref().is_some_and(|p| now >= p.end) {
            let p = self.pending.take().unwrap();
            for &(a, b) in &p.bytes {
                self.mem[a] = b;
            }
            self.wel = false;
        }
    }

    /// The power goes at clock `now`: an operation under way stops, leaving
    /// what `cut_mode` says. -> what was cut, if any.
    pub fn cut(&mut self, now: u64) -> Option<String> {
        self.settle(now);
        let p = self.pending.take()?;
        let f = match self.cut_mode {
            CutMode::Share => (now.saturating_sub(p.start) as f64 / (p.end - p.start).max(1) as f64).clamp(0.0, 1.0),
            CutMode::None => 0.0,
            CutMode::Done => 1.0,
        };
        let n = (p.bytes.len() as f64 * f) as usize;
        for &(a, b) in &p.bytes[..n] {
            self.mem[a] = b;
        }
        self.wel = false;
        Some(format!("{:x?} cut {:.0}% through ({n} of {} bytes changed)", p.op, f * 100.0, p.bytes.len()))
    }

    /// One byte clocked while selected -> the byte the flash drives back.
    fn byte(&mut self, b: u8) -> u8 {
        let i = self.idx;
        self.idx += 1;
        if i == 0 {
            self.cmd = b;
            self.addr = 0;
            if self.pending.is_some() {
                if b != 0x05 {
                    self.cmd = 0xFF; // ignored while busy
                }
                return 0xFF;
            }
            match b {
                0x06 => self.wel = true,
                0x04 => self.wel = false,
                0x03 | 0x0B | 0x02 | 0x20 | 0x52 | 0xD8 | 0xC7 | 0x60 | 0x05 | 0x9F | 0x01 | 0x30 | 0xAB | 0xB9 => {}
                _ => *self.unknown.entry(b).or_insert(0) += 1,
            }
            return 0xFF;
        }
        let addressed = matches!(self.cmd, 0x03 | 0x0B | 0x02 | 0x20 | 0x52 | 0xD8);
        if addressed && i <= 3 {
            self.addr = (self.addr << 8) | b as u32;
            return 0xFF;
        }
        match self.cmd {
            0x05 => (self.wel as u8) << 1 | self.pending.is_some() as u8,
            0x9F => self.part.id.get(i as usize - 1).copied().unwrap_or(0xFF),
            0x03 => self.read_next(),
            0x0B if i == 4 => 0xFF,
            0x0B => self.read_next(),
            0x02 => {
                if self.prog.len() < self.part.page {
                    self.prog.push(b);
                }
                0xFF
            }
            _ => 0xFF,
        }
    }

    fn read_next(&mut self) -> u8 {
        let a = self.addr as usize % self.mem.len();
        self.addr = self.addr.wrapping_add(1);
        self.mem[a]
    }

    fn deselect(&mut self, now: u64, ips: f64) {
        let (cmd, addr, n) = (self.cmd, self.addr as usize, self.idx);
        self.idx = 0;
        let prog = std::mem::take(&mut self.prog);
        if n == 0 {
            return;
        }
        if self.trace_all {
            eprintln!("[spi flash] cmd {cmd:02x} addr {addr:06x} {n} bytes wel {}", self.wel);
        }
        let small = self.part.small_erases;
        let erase = match cmd {
            0x20 if small => Some(0x1000),
            0x52 if small => Some(0x8000),
            0xD8 => Some(self.part.sector),
            0xC7 | 0x60 => Some(self.mem.len()),
            _ => None,
        };
        let complete = if matches!(cmd, 0xC7 | 0x60) { n >= 1 } else { n >= 4 };
        if let (Some(len), true) = (erase, complete) {
            if self.wel {
                let at = if len == self.mem.len() { 0 } else { (addr % self.mem.len()) & !(len - 1) };
                let bytes = (at..at + len).map(|a| (a, 0xFF)).collect();
                self.start(Op::Erase { at: at as u32, len: len as u32 }, bytes, now, ips);
                self.erased_bytes += len as u64;
            } else {
                self.wel = false;
            }
        } else if cmd == 0x02 && n >= 4 {
            if self.wel && !prog.is_empty() {
                let a = addr % self.mem.len();
                let size = self.part.page;
                let page = a & !(size - 1);
                let mut bytes: Vec<(usize, u8)> = Vec::with_capacity(prog.len());
                for (k, d) in prog.iter().enumerate() {
                    let at = page + (a - page + k) % size;
                    let old = bytes.iter().rev().find(|x| x.0 == at).map_or(self.mem[at], |x| x.1);
                    bytes.push((at, old & d));
                }
                self.start(Op::Program { at: a as u32, len: prog.len() as u32 }, bytes, now, ips);
                self.programmed_bytes += prog.len() as u64;
            } else {
                self.wel = false;
            }
        } else if cmd == 0x01 {
            self.wel = false;
        }
    }

    /// Begin `op` at clock `now`: it lands when the part's time is up.
    fn start(&mut self, op: Op, bytes: Vec<(usize, u8)>, now: u64, ips: f64) {
        let t = if self.instant { 0.0 } else { self.part.time(&op) };
        let end = now + (t * 1e-6 * ips).ceil() as u64;
        self.pending = Some(Pending { op, bytes, start: now, end });
        self.settle(now);
        self.done(op);
    }

    fn done(&mut self, op: Op) {
        self.dirty = true;
        if self.trace {
            eprintln!("[spi flash] {op:x?}");
        }
        if self.ops.len() < 1 << 20 {
            self.ops.push(op);
        }
    }

    pub fn state(&self) -> FlashState {
        FlashState {
            mem_lz4: lz4_flex::compress_prepend_size(&self.mem),
            wel: self.wel,
            pending: self.pending.clone(),
            erased_bytes: self.erased_bytes,
            programmed_bytes: self.programmed_bytes,
        }
    }

    /// Back to a snapshot's flash. A selection in progress is dropped (a
    /// snapshot is taken between instructions; the OS deselects within its
    /// own routine, so none is open in practice).
    pub fn restore(&mut self, s: FlashState) -> Result<(), String> {
        let mem = lz4_flex::decompress_size_prepended(&s.mem_lz4).map_err(|e| e.to_string())?;
        if mem.len() != self.mem.len() {
            return Err(format!("snapshot flash is {} bytes, this machine's {}", mem.len(), self.mem.len()));
        }
        self.mem = mem;
        self.wel = s.wel;
        self.pending = s.pending;
        self.erased_bytes = s.erased_bytes;
        self.programmed_bytes = s.programmed_bytes;
        self.idx = 0;
        self.prog.clear();
        self.rx.clear();
        self.dirty = true;
        Ok(())
    }

    /// One line for the end of a run.
    pub fn summary(&self) -> String {
        format!(
            "spi flash: {} B erased, {} B programmed in {} operations{}",
            self.erased_bytes,
            self.programmed_bytes,
            self.ops.len(),
            if self.unknown.is_empty() { String::new() } else { format!(", unmodelled commands {:02x?}", self.unknown) }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(f: &mut SpiFlash, bytes: &[u8]) -> Vec<u8> {
        for (k, &b) in bytes.iter().enumerate() {
            let cont = if k + 1 < bytes.len() { 0x8000_0000 } else { 0x0800_0000 };
            f.push(cont | PCS_FLASH | b as u32, 0, 1.0);
        }
        (0..bytes.len()).map(|_| f.pop() as u8).collect()
    }

    #[test]
    fn erase_program_read() {
        let mut f = SpiFlash::new(vec![0u8; 1 << 20]);
        f.part = W25Q128;
        f.instant = true;
        // Without WREN nothing changes.
        tx(&mut f, &[0x20, 0x00, 0x10, 0x00]);
        assert_eq!(f.mem[0x1000], 0);
        tx(&mut f, &[0x06]);
        assert_eq!(tx(&mut f, &[0x05, 0])[1], 2);
        tx(&mut f, &[0x20, 0x00, 0x10, 0x23]);
        assert!(f.mem[0x1000..0x2000].iter().all(|&b| b == 0xFF));
        assert_eq!(f.mem[0x2000], 0);
        assert_eq!(tx(&mut f, &[0x05, 0])[1], 0);
        tx(&mut f, &[0x06]);
        tx(&mut f, &[0x02, 0x00, 0x10, 0xFE, 0x12, 0x34, 0x56]);
        // Wraps in the page: 0x10FE, 0x10FF, then 0x1000.
        assert_eq!(&f.mem[0x10FE..0x1100], &[0x12, 0x34]);
        assert_eq!(f.mem[0x1000], 0x56);
        let r = tx(&mut f, &[0x03, 0x00, 0x10, 0xFE, 0, 0]);
        assert_eq!(&r[4..], &[0x12, 0x34]);
        assert_eq!(f.ops.len(), 2);
    }

    #[test]
    fn default_part_256k_sectors_512_byte_pages() {
        let mut f = SpiFlash::new(vec![0u8; 1 << 20]);
        f.instant = true;
        assert_eq!(&tx(&mut f, &[0x9F, 0, 0, 0, 0, 0])[1..], &[0x01, 0x20, 0x18, 0x4D, 0x00]);
        tx(&mut f, &[0x06]);
        tx(&mut f, &[0x20, 0x04, 0x00, 0x00]); // no 4 KB erase on this part
        assert_eq!(f.mem[0x4_0000], 0);
        tx(&mut f, &[0x06]);
        tx(&mut f, &[0xD8, 0x05, 0x00, 0x00]);
        assert!(f.mem[0x4_0000..0x8_0000].iter().all(|&b| b == 0xFF));
        assert_eq!(f.mem[0x8_0000], 0);
        tx(&mut f, &[0x06]);
        let mut cmd = vec![0x02, 0x04, 0x01, 0xFF];
        cmd.extend([0x11, 0x22]);
        tx(&mut f, &cmd);
        // 0x401FF is the last byte of the 512-byte page at 0x40000.
        assert_eq!(f.mem[0x4_01FF], 0x11);
        assert_eq!(f.mem[0x4_0000], 0x22);
    }

    #[test]
    fn timed_erase_reads_busy_then_lands() {
        let mut f = SpiFlash::new(vec![0u8; 1 << 20]);
        f.instant = false;
        let ips = 1e6; // one instruction a microsecond
        let at = |f: &mut SpiFlash, t: u64, bytes: &[u8]| -> Vec<u8> {
            for (k, &b) in bytes.iter().enumerate() {
                let cont = if k + 1 < bytes.len() { 0x8000_0000 } else { 0x0800_0000 };
                f.push(cont | PCS_FLASH | b as u32, t, ips);
            }
            (0..bytes.len()).map(|_| f.pop() as u8).collect()
        };
        at(&mut f, 0, &[0x06]);
        at(&mut f, 0, &[0xD8, 0x04, 0x00, 0x00]);
        assert_eq!(at(&mut f, 10, &[0x05, 0])[1] & 1, 1);
        assert_eq!(f.mem[0x4_0000], 0, "lands at the end");
        at(&mut f, 20, &[0x06]); // ignored while busy
        assert_eq!(at(&mut f, 520_000, &[0x05, 0])[1], 0);
        assert_eq!(f.mem[0x4_0000], 0xFF);
        // A cut halfway leaves half the sector erased.
        at(&mut f, 600_000, &[0x06]);
        f.mem[0x8_0000..0xC_0000].fill(0);
        at(&mut f, 600_000, &[0xD8, 0x08, 0x00, 0x00]);
        f.cut_mode = CutMode::Share;
        assert!(f.cut(860_000).is_some());
        assert_eq!(f.mem[0x8_0000], 0xFF);
        assert_eq!(f.mem[0xB_FFFF], 0);
        // The other settings: nothing, or all of it.
        for (mode, want) in [(CutMode::None, 0u8), (CutMode::Done, 0xFF)] {
            at(&mut f, 900_000, &[0x06]);
            f.mem[0x8_0000..0xC_0000].fill(0);
            at(&mut f, 900_000, &[0xD8, 0x08, 0x00, 0x00]);
            f.cut_mode = mode;
            assert!(f.cut(900_010).is_some());
            assert_eq!((f.mem[0x8_0000], f.mem[0xB_FFFF]), (want, want));
        }
    }

    #[test]
    fn state_round_trip_keeps_an_operation_under_way() {
        let mut f = SpiFlash::new(vec![0u8; 1 << 20]);
        f.instant = false;
        tx(&mut f, &[0x06]);
        tx(&mut f, &[0xD8, 0x04, 0x00, 0x00]);
        assert!(f.busy());
        let s = f.state();
        let mut g = SpiFlash::new(vec![0u8; 1 << 20]);
        g.restore(s).unwrap();
        assert!(g.busy());
        g.settle(u64::MAX);
        assert_eq!(g.mem[0x4_0000], 0xFF);
        assert_eq!(g.erased_bytes, 0x4_0000);
    }

    #[test]
    fn program_only_clears_bits() {
        let mut f = SpiFlash::new(vec![0xF0u8; 0x1000]);
        f.instant = true;
        tx(&mut f, &[0x06]);
        tx(&mut f, &[0x02, 0, 0, 0, 0x3C]);
        assert_eq!(f.mem[0], 0x30);
    }

    #[test]
    fn mcr_clears_receive_queue() {
        let mut f = SpiFlash::new(vec![0u8; 0x1000]);
        f.push(0x8000_0000 | PCS_FLASH | 0x05, 0, 1.0);
        f.mcr(0xC00);
        assert_eq!(f.pop(), 0xFF);
    }
}
