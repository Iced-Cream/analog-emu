//! The MKI units' boot and configuration flash: parallel NOR on FlexBus CS0,
//! 8 MB at address 0, and the 2 MB persistent memory on CS1 at 0x20000000
//! (docs/MK1-PORT.md).
//!
//! The flash is two 8-bit AMD/JEDEC parts side by side on the 16-bit bus: the
//! OS writes every command byte to both lanes (`0xAAAA` to byte address
//! `0xAAAA`, `0x5555` to `0x5554`), so each part sees word address `0x5555` /
//! `0x2AAA` [measured, A4 MKI OS 1.56 `0x4007594c..0x40075a78`]. Modelled:
//!
//! ```text
//!   F0             reset to read array (any address)
//!   AA 55 A0, w    program one word (bits can only clear)
//!   AA 55 80 AA 55 30   erase the sector holding the address
//!   AA 55 80 AA 55 10   erase the chip
//!   AA 55 90       autoselect (IDs at words 0 and 1) until F0
//!   98 at 0x55     CFI query until F0
//! ```
//!
//! Operations complete at once, so a status poll reads the final data
//! (DQ7 true, DQ6 still). Sectors are 8 KB below 0x10000 and 64 KB above: the
//! OS's bootstrap upgrade erases 0..0xFFFF as eight 8 KB sectors (A4 MKI
//! `0x40100c36`) [measured]; 64 KB above is [inferred].
//!
//! The contents live in the bus's sparse pages, so snapshots carry them; a
//! page never written reads erased (0xFF). The CS1 memory is plain sparse
//! memory (zero at first) [inferred: battery-backed SRAM or MRAM].

use std::collections::HashMap;

pub const NOR_SIZE: u32 = 8 << 20;
pub const PRAM_BASE: u32 = 0x2000_0000;
pub const PRAM_SIZE: u32 = 2 << 20;
const PAGE: u32 = 0x1000;

type Sparse = HashMap<u32, Box<[u8; PAGE as usize]>>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Read,
    /// The next write is a word to program.
    Program,
    AutoSelect,
    Cfi,
}

#[derive(Debug, Default)]
pub struct Nor {
    /// Unlock cycles seen (0..2).
    cycle: u8,
    /// `80` came: the next unlocked command is an erase.
    erase_armed: bool,
    mode: Mode,
    pub programs: u64,
    pub erases: u64,
}

/// 8 KB boot sectors below 0x10000, 64 KB above: -> (start, length).
fn sector(off: u32) -> (u32, u32) {
    if off < 0x1_0000 {
        (off & !0x1FFF, 0x2000)
    } else {
        (off & !0xFFFF, 0x1_0000)
    }
}

/// A flash byte from the sparse pages: erased (0xFF) where nothing was written.
pub fn byte(sparse: &Sparse, off: u32) -> u8 {
    sparse.get(&(off & !(PAGE - 1))).map_or(0xFF, |p| p[(off & (PAGE - 1)) as usize])
}

fn page_mut(sparse: &mut Sparse, off: u32) -> &mut [u8; PAGE as usize] {
    sparse.entry(off & !(PAGE - 1)).or_insert_with(|| Box::new([0xFF; PAGE as usize]))
}

/// Store bytes into the flash as a programmer would (no command cycles).
pub fn load(sparse: &mut Sparse, off: u32, data: &[u8]) {
    for (i, &b) in data.iter().enumerate() {
        let o = off + i as u32;
        page_mut(sparse, o)[(o & (PAGE - 1)) as usize] = b;
    }
}

impl Nor {
    pub fn read(&self, sparse: &Sparse, a: u32, size: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..size {
            let o = a.wrapping_add(i);
            let b = match self.mode {
                Mode::Read | Mode::Program => byte(sparse, o),
                // Both lanes answer alike. Toshiba's maker ID (the teardown
                // names Toshiba flash) and an arbitrary device ID [inferred].
                Mode::AutoSelect => match (o >> 1) & 0xFF {
                    0 => 0x98,
                    1 => 0x4F,
                    _ => 0,
                },
                Mode::Cfi => match (o >> 1) & 0xFF {
                    0x10 => b'Q',
                    0x11 => b'R',
                    0x12 => b'Y',
                    0x13 => 0x02, // AMD command set
                    0x27 => 22,   // 2^22 = 4 MB per part
                    _ => 0,
                },
            };
            v = v << 8 | b as u32;
        }
        v
    }

    pub fn write(&mut self, sparse: &mut Sparse, a: u32, size: u32, v: u32) {
        if size == 4 {
            self.write(sparse, a, 2, v >> 16);
            self.write(sparse, a.wrapping_add(2), 2, v & 0xFFFF);
            return;
        }
        let a = a & (NOR_SIZE - 1);
        if self.mode == Mode::Program {
            // Programming can only clear bits.
            for i in 0..size {
                let o = a + i;
                let b = (v >> (8 * (size - 1 - i))) as u8;
                let p = page_mut(sparse, o);
                p[(o & (PAGE - 1)) as usize] &= b;
            }
            self.programs += 1;
            self.mode = Mode::Read;
            self.cycle = 0;
            return;
        }
        let cmd = v as u8;
        let wa = (a >> 1) & 0x7FF;
        if cmd == 0xF0 {
            self.mode = Mode::Read;
            self.cycle = 0;
            self.erase_armed = false;
            return;
        }
        match self.cycle {
            0 if wa == 0x555 && cmd == 0xAA => self.cycle = 1,
            0 if wa == 0x55 && cmd == 0x98 => self.mode = Mode::Cfi,
            1 if wa == 0x2AA && cmd == 0x55 => self.cycle = 2,
            2 if self.erase_armed && (cmd == 0x30 || (cmd == 0x10 && wa == 0x555)) => {
                let (start, len) = if cmd == 0x30 { sector(a) } else { (0, NOR_SIZE) };
                for p in (start..start + len).step_by(PAGE as usize) {
                    sparse.remove(&p);
                }
                self.erases += 1;
                self.erase_armed = false;
                self.cycle = 0;
            }
            2 if wa == 0x555 => {
                match cmd {
                    0xA0 => self.mode = Mode::Program,
                    0x80 => self.erase_armed = true,
                    0x90 => self.mode = Mode::AutoSelect,
                    _ => {}
                }
                self.cycle = 0;
            }
            _ => self.cycle = 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_erase_and_ids() {
        let mut s = Sparse::new();
        let mut n = Nor::default();
        assert_eq!(n.read(&s, 0x7F_0000, 4), 0xFFFF_FFFF);
        let unlock = |n: &mut Nor, s: &mut Sparse| {
            n.write(s, 0xAAAA, 2, 0xAAAA);
            n.write(s, 0x5554, 2, 0x5555);
        };
        // Program 0x1234 at 0x7f0000 (the OS's 0x400759d6 sequence).
        n.write(&mut s, 0xAAAA, 2, 0xF0F0);
        unlock(&mut n, &mut s);
        n.write(&mut s, 0xAAAA, 2, 0xA0A0);
        n.write(&mut s, 0x7F_0000, 2, 0x1234);
        assert_eq!(n.read(&s, 0x7F_0000, 2), 0x1234);
        // Writes outside a command are ignored.
        n.write(&mut s, 0x7F_0000, 2, 0x0000);
        assert_eq!(n.read(&s, 0x7F_0000, 2), 0x1234);
        // Sector erase (0x4007594c).
        unlock(&mut n, &mut s);
        n.write(&mut s, 0xAAAA, 2, 0x8080);
        unlock(&mut n, &mut s);
        n.write(&mut s, 0x7F_0010, 2, 0x3030);
        assert_eq!(n.read(&s, 0x7F_0000, 2), 0xFFFF);
        assert!(s.is_empty());
        // Autoselect until reset.
        unlock(&mut n, &mut s);
        n.write(&mut s, 0xAAAA, 2, 0x9090);
        assert_eq!(n.read(&s, 0, 4), 0x9898_4F4F);
        n.write(&mut s, 0xAAAA, 2, 0xF0F0);
        assert_eq!(n.read(&s, 0, 2), 0xFFFF);
        // A boot sector is 8 KB.
        load(&mut s, 0x2000, &[1; 0x4000]);
        unlock(&mut n, &mut s);
        n.write(&mut s, 0xAAAA, 2, 0x8080);
        unlock(&mut n, &mut s);
        n.write(&mut s, 0x2000, 2, 0x3030);
        assert_eq!((n.read(&s, 0x3FFF, 1), n.read(&s, 0x4000, 1)), (0xFF, 1));
    }
}
