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
//!   02 a a a data    page program: bits only clear, wraps in its 256-byte page
//!   20 / 52 / D8 a a a     erase 4 KB / 32 KB / 64 KB
//!   C7 / 60          erase the chip
//!   9F               JEDEC ID (`SpiFlash::id`)
//!   30               clear status (Spansion's CLSR; the OS sends it after
//!                    an error bit, 0x60, which never sets here)
//! ```
//!
//! Program and erase need WEL, act when the chip select goes high (a word
//! without CONT), and clear WEL; they complete at once, so a status poll
//! never sees WIP. Which part the board carries, and so its erase sizes and
//! ID, is [inferred]; any other command is counted in `unknown` and answers
//! 0xFF. Nothing here reaches real hardware: the image is a byte vector
//! (written back to a file only by the tools' `--flash` option).

use std::collections::{BTreeMap, VecDeque};

pub const DSPI0_MCR: u32 = 0xFC05_C000;
pub const DSPI0_PUSHR: u32 = 0xFC05_C034;
pub const DSPI0_POPR: u32 = 0xFC05_C038;
/// PUSHR's PCS field (bits 23:16) for the flash.
const PCS_FLASH: u32 = 0x0002_0000;
const PAGE: usize = 256;

/// A Winbond W25Q128 (EF 4018). The OS's part check (Rytm MKII OS 1.74
/// `0x40136b50`, on the first five 9F bytes) knows Spansion 01 2018 (byte 4
/// 0: 512-byte pages, 256 KB sectors; 1: 256-byte pages, 64 KB sectors),
/// ISSI 9D 6017/6018, Macronix C2 2017/2018, Winbond EF 4014/7015/4017/4018
/// and EON 1C 7015; all but the first take 256-byte pages and 64 KB sectors
/// [measured]. Any other ID fails every program and erase. Which of them a
/// unit carries is not known; this one has the plain JEDEC command set.
pub const ID_W25Q128: [u8; 5] = [0xEF, 0x40, 0x18, 0x00, 0x00];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Erase { at: u32, len: u32 },
    Program { at: u32, len: u32 },
}

pub struct SpiFlash {
    pub mem: Vec<u8>,
    /// What 9F answers (the OS reads five bytes).
    pub id: [u8; 5],
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
    trace: bool,
    trace_all: bool,
}

impl SpiFlash {
    pub fn new(mem: Vec<u8>) -> SpiFlash {
        SpiFlash {
            mem,
            id: ID_W25Q128,
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

    pub fn push(&mut self, v: u32) {
        let out = if v & 0x00FF_0000 == PCS_FLASH { self.byte(v as u8) } else { 0xFF };
        if self.rx.len() < 64 {
            self.rx.push_back(out as u32);
        }
        if v & 0x8000_0000 == 0 {
            self.deselect();
        }
    }

    /// One byte clocked while selected -> the byte the flash drives back.
    fn byte(&mut self, b: u8) -> u8 {
        let i = self.idx;
        self.idx += 1;
        if i == 0 {
            self.cmd = b;
            self.addr = 0;
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
            0x05 => (self.wel as u8) << 1,
            0x9F => self.id.get(i as usize - 1).copied().unwrap_or(0xFF),
            0x03 => self.read_next(),
            0x0B if i == 4 => 0xFF,
            0x0B => self.read_next(),
            0x02 => {
                if self.prog.len() < PAGE {
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

    fn deselect(&mut self) {
        let (cmd, addr, n) = (self.cmd, self.addr as usize, self.idx);
        self.idx = 0;
        let prog = std::mem::take(&mut self.prog);
        if n == 0 {
            return;
        }
        if self.trace_all {
            eprintln!("[spi flash] cmd {cmd:02x} addr {addr:06x} {n} bytes wel {}", self.wel);
        }
        let erase = match cmd {
            0x20 => Some(0x1000),
            0x52 => Some(0x8000),
            0xD8 => Some(0x1_0000),
            0xC7 | 0x60 => Some(self.mem.len()),
            _ => None,
        };
        let complete = if matches!(cmd, 0xC7 | 0x60) { n >= 1 } else { n >= 4 };
        if let (Some(len), true) = (erase, complete) {
            if self.wel {
                let at = if len == self.mem.len() { 0 } else { (addr % self.mem.len()) & !(len - 1) };
                self.mem[at..at + len].fill(0xFF);
                self.done(Op::Erase { at: at as u32, len: len as u32 });
                self.erased_bytes += len as u64;
            }
            self.wel = false;
        } else if cmd == 0x02 && n >= 4 {
            if self.wel && !prog.is_empty() {
                let a = addr % self.mem.len();
                let page = a & !(PAGE - 1);
                for (k, d) in prog.iter().enumerate() {
                    self.mem[page + (a + k) % PAGE] &= d;
                }
                self.done(Op::Program { at: a as u32, len: prog.len() as u32 });
                self.programmed_bytes += prog.len() as u64;
            }
            self.wel = false;
        } else if cmd == 0x01 {
            self.wel = false;
        }
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
            f.push(cont | PCS_FLASH | b as u32);
        }
        (0..bytes.len()).map(|_| f.pop() as u8).collect()
    }

    #[test]
    fn erase_program_read() {
        let mut f = SpiFlash::new(vec![0u8; 1 << 20]);
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
    fn program_only_clears_bits() {
        let mut f = SpiFlash::new(vec![0xF0u8; 0x1000]);
        tx(&mut f, &[0x06]);
        tx(&mut f, &[0x02, 0, 0, 0, 0x3C]);
        assert_eq!(f.mem[0], 0x30);
    }

    #[test]
    fn mcr_clears_receive_queue() {
        let mut f = SpiFlash::new(vec![0u8; 0x1000]);
        f.push(0x8000_0000 | PCS_FLASH | 0x05);
        f.mcr(0xC00);
        assert_eq!(f.pop(), 0xFF);
    }
}
