//! Small models of the MKI units' board parts that the MKII units lack.
//!
//! The codec's control port on DSPI1. The A4 MKI OS sets its codec up over
//! DSPI1 (`0xFC03C000`) before it starts SSI1, in the Cirrus Logic SPI control
//! format: a chip address byte (`0x9E` write, `0x9F` read), a register pointer,
//! then data (`0x40001150` writes, `0x400010e8` reads) [measured]. It writes
//! register 2 = 1 and reads it back; anything else leaves SSI1 off and sets a
//! codec-failure flag (`0x4000121c`, `*0x4043f3f0`) [measured]. 0x9E is the
//! CS4271's chip address [inferred: the part is not named in the image].
//!
//! Modelled: PUSHR (`+0x34`) words end a transaction when CONT (bit 31) is
//! clear; each pushes a POPR (`+0x38`) word, the register's value in the data
//! phase of a read, 0 otherwise. SR keeps reading as memory (the OS sets TCF
//! itself before it polls).

use std::collections::VecDeque;

pub const DSPI1_PUSHR: u32 = 0xFC03_C034;
pub const DSPI1_POPR: u32 = 0xFC03_C038;

#[derive(Debug)]
pub struct CodecSpi {
    pub regs: Vec<u8>,
    /// Bytes into the current transaction.
    idx: u32,
    chip: u8,
    map: u8,
    rx: VecDeque<u32>,
}

impl Default for CodecSpi {
    fn default() -> Self {
        CodecSpi { regs: vec![0; 128], idx: 0, chip: 0, map: 0, rx: VecDeque::new() }
    }
}

impl CodecSpi {
    pub fn push(&mut self, v: u32) {
        let b = v as u8;
        let mut out = 0u32;
        match (self.idx, self.chip) {
            (0, _) => self.chip = b,
            (1, 0x9E) => self.map = b & 0x7F,
            (_, 0x9F) => {
                out = self.regs[self.map as usize] as u32;
                self.map = (self.map + 1) & 0x7F;
            }
            (_, 0x9E) => {
                self.regs[self.map as usize] = b;
                self.map = (self.map + 1) & 0x7F;
            }
            _ => {}
        }
        self.idx += 1;
        if v & 0x8000_0000 == 0 {
            self.idx = 0;
        }
        if self.rx.len() < 16 {
            self.rx.push_back(out);
        }
    }

    pub fn pop(&mut self) -> u32 {
        self.rx.pop_front().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_back() {
        let mut c = CodecSpi::default();
        // 0x40001150(2, 1): write register 2 = 1.
        for w in [0x8002_009E, 0x8002_0002, 0x0802_0001] {
            c.push(w);
        }
        for _ in 0..3 {
            c.pop();
        }
        // 0x400010e8(2): pointer, then a read; the 4th POPR word is the value.
        for w in [0x8002_009E, 0x0002_0002, 0x8002_009F, 0x0802_0000] {
            c.push(w);
        }
        let r: Vec<u32> = (0..4).map(|_| c.pop()).collect();
        assert_eq!(r, [0, 0, 0, 1]);
    }
}
