//! Unit timing (`DTEMU_TIMING=unit`): the clock counts core cycles, not
//! instructions, so the CPU load the firmware sees (analoghealth's CPU %,
//! DSP %, the UI tick rate) follows the unit's instead of an ideal one
//! instruction per tick.
//!
//! The default mode is untouched: one tick per instruction, 200 M a second.
//! In unit mode:
//! - `Io::ips` is the core clock, 250 MHz (MCF5441x), so every peripheral
//!   still converts through it and keeps its real rate.
//! - Each instruction costs one cycle plus a per-class extra (multiply,
//!   divide, MOVEM per register, flow changes, EMAC accumulator reads,
//!   cache maintenance), plus `base` (a fractional extra on every
//!   instruction, for pipeline stalls the classes don't name).
//! - Memory: the cache model (cache.rs) is turned on, and each D-cache miss,
//!   dirty write-back and I-cache miss costs a DDR line transfer; an access
//!   that bypasses the caches costs `unc` (DDR through the uncached alias),
//!   `sram` or `io` (peripheral registers).
//! - An exception or interrupt entry costs `exc`.
//!
//! The numbers are parameters, not datasheet values: `DTEMU_TIMING_PARAMS`
//! (`key=value,...`) sets any of them, and `tools/timing-fit` fits them to
//! CPU readings recorded on a unit. Defaults are the fitted values
//! (docs/TIMING.md says from which runs).
//!
//! Not modelled: the V4 pipeline itself, the store buffer, the branch cache
//! (taken branches cost `branch` whether predicted or not), DDR banks and
//! refresh, bus contention with eDMA. The aim is the load within a few
//! points, not exact instruction timing.

use serde::{Deserialize, Serialize};

/// Cost parameters, in core cycles (`base` in cycles per instruction).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Params {
    /// Core clock, Hz.
    pub hz: f64,
    /// Extra cycles on every instruction (fractional).
    pub base: f64,
    pub mul_w: u32,
    pub mul_l: u32,
    pub div_w: u32,
    pub div_l: u32,
    /// MOVEM: per register moved, beyond the first.
    pub movem: u32,
    /// Bcc/BRA (taken or not: the branch cache is not modelled).
    pub branch: u32,
    /// BSR, JSR, JMP, RTS.
    pub call: u32,
    pub rte: u32,
    /// MOVE from an EMAC accumulator or MACSR (waits for the MAC pipeline).
    pub mac_rd: u32,
    /// MOVEC, CPUSHL, INTOUCH, STOP-free SR writes: pipeline syncs.
    pub sync: u32,
    /// Exception or interrupt entry.
    pub exc: u32,
    /// D-cache line fill from DDR.
    pub dmiss: u32,
    /// Dirty D-cache line written back.
    pub wb: u32,
    /// I-cache line fill from DDR.
    pub imiss: u32,
    /// A data access to DDR that bypasses the D-cache (uncached alias).
    pub unc: u32,
    /// An access to internal SRAM.
    pub sram: u32,
    /// An access to a peripheral register.
    pub io: u32,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            hz: 250_000_000.0,
            base: 0.0,
            mul_w: 1,
            mul_l: 2,
            div_w: 10,
            div_l: 18,
            movem: 1,
            branch: 1,
            call: 2,
            rte: 8,
            mac_rd: 3,
            sync: 6,
            exc: 10,
            dmiss: 20,
            wb: 8,
            imiss: 20,
            unc: 16,
            sram: 0,
            io: 12,
        }
    }
}

impl Params {
    /// Apply `key=value,...` over these values.
    pub fn apply(&mut self, s: &str) -> Result<(), String> {
        for kv in s.split(',').map(str::trim).filter(|kv| !kv.is_empty()) {
            let (k, v) = kv.split_once('=').ok_or_else(|| format!("timing param `{kv}`: want key=value"))?;
            let f: f64 = v.trim().parse().map_err(|_| format!("timing param `{kv}`: not a number"))?;
            let u = || -> Result<u32, String> {
                if f < 0.0 || f.fract() != 0.0 {
                    Err(format!("timing param `{kv}`: want a whole number of cycles"))
                } else {
                    Ok(f as u32)
                }
            };
            match k.trim() {
                "hz" => self.hz = f,
                "base" => self.base = f,
                "mul_w" => self.mul_w = u()?,
                "mul_l" => self.mul_l = u()?,
                "div_w" => self.div_w = u()?,
                "div_l" => self.div_l = u()?,
                "movem" => self.movem = u()?,
                "branch" => self.branch = u()?,
                "call" => self.call = u()?,
                "rte" => self.rte = u()?,
                "mac_rd" => self.mac_rd = u()?,
                "sync" => self.sync = u()?,
                "exc" => self.exc = u()?,
                "dmiss" => self.dmiss = u()?,
                "wb" => self.wb = u()?,
                "imiss" => self.imiss = u()?,
                "unc" => self.unc = u()?,
                "sram" => self.sram = u()?,
                "io" => self.io = u()?,
                other => return Err(format!("timing param `{other}`: unknown")),
            }
        }
        if self.hz <= 0.0 || self.base < 0.0 {
            return Err("timing params: hz must be > 0 and base >= 0".into());
        }
        Ok(())
    }
}

/// Where the cycles went, for the end-of-run summary and the fit.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stats {
    pub instructions: u64,
    pub op_extra: u64,
    pub base_extra: u64,
    pub exc: u64,
    pub dmiss: u64,
    pub wb: u64,
    pub imiss: u64,
    pub unc: u64,
    pub sram: u64,
    pub io: u64,
}

impl Stats {
    pub fn total_extra(&self) -> u64 {
        self.op_extra + self.base_extra + self.exc + self.dmiss + self.wb + self.imiss + self.unc + self.sram + self.io
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Timing {
    pub p: Params,
    /// `base` carried between instructions, in 1/65536 cycle.
    frac: u64,
    base_fx: u64,
    pub stats: Stats,
}

impl Timing {
    pub fn new(p: Params) -> Timing {
        let base_fx = (p.base * 65536.0).round() as u64;
        Timing { p, frac: 0, base_fx, stats: Stats::default() }
    }

    /// From the environment: None unless `DTEMU_TIMING=unit`. A bad
    /// `DTEMU_TIMING` or `DTEMU_TIMING_PARAMS` is an error, not a silent
    /// default, so a run never claims unit timing it doesn't have.
    pub fn from_env() -> Result<Option<Timing>, String> {
        let v = std::env::var("DTEMU_TIMING").unwrap_or_default();
        match v.as_str() {
            "" | "0" | "off" => return Ok(None),
            "unit" | "1" => {}
            other => return Err(format!("DTEMU_TIMING={other}: want `unit` (or unset)")),
        }
        let mut p = Params::default();
        if let Ok(s) = std::env::var("DTEMU_TIMING_PARAMS") {
            p.apply(&s)?;
        }
        Ok(Some(Timing::new(p)))
    }

    /// Extra cycles for instruction `op` (first word), beyond its one cycle
    /// and its memory accesses. `ext` is the word after it (MOVEM's mask).
    #[inline]
    pub fn op_extra(&mut self, op: u16, ext: u16) -> u64 {
        let p = &self.p;
        let x = match op >> 12 {
            0x4 => match op {
                0x4E75 => p.call,                     // RTS
                0x4E73 => p.rte,                      // RTE
                0x4E7B => p.sync,                     // MOVEC
                _ if op & 0xFFC0 == 0x4E80 => p.call, // JSR
                _ if op & 0xFFC0 == 0x4EC0 => p.call, // JMP
                _ if op & 0xFFC0 == 0x4C00 => p.mul_l,
                _ if op & 0xFFC0 == 0x4C40 => p.div_l, // DIVx.L / REMx.L
                _ if op & 0xFFC0 == 0x48C0 || op & 0xFFC0 == 0x4CC0 => {
                    p.movem * ext.count_ones().saturating_sub(1)
                }
                _ if op & 0xFFC0 == 0x46C0 => p.sync, // MOVE to SR
                _ => 0,
            },
            0x6 => {
                if op & 0xFF00 == 0x6100 {
                    p.call // BSR
                } else {
                    p.branch
                }
            }
            0x8 if op & 0x01C0 == 0x00C0 || op & 0x01C0 == 0x01C0 => p.div_w,
            0xC if op & 0x01C0 == 0x00C0 || op & 0x01C0 == 0x01C0 => p.mul_w,
            // EMAC: MOVE from ACC/ACCext/MACSR/MASK (a1x0/a9x0/a980/ad80 forms).
            0xA if op & 0xF9B0 == 0xA180 || op & 0xFFF0 == 0xA980 || op & 0xFFF0 == 0xAD80 => p.mac_rd,
            0xF if op & 0xFF00 == 0xF400 => p.sync, // CPUSHL / INTOUCH
            _ => 0,
        } as u64;
        self.stats.instructions += 1;
        self.stats.op_extra += x;
        self.frac += self.base_fx;
        let b = self.frac >> 16;
        self.frac &= 0xFFFF;
        self.stats.base_extra += b;
        x + b
    }

    #[inline]
    pub fn exception(&mut self) -> u64 {
        self.stats.exc += self.p.exc as u64;
        self.p.exc as u64
    }

    /// Cache-model counter deltas over one access -> cycles.
    #[inline]
    pub fn cache_cost(&mut self, dmiss: u64, wb: u64, imiss: u64) -> u64 {
        let (a, b, c) = (dmiss * self.p.dmiss as u64, wb * self.p.wb as u64, imiss * self.p.imiss as u64);
        self.stats.dmiss += a;
        self.stats.wb += b;
        self.stats.imiss += c;
        a + b + c
    }

    /// A CPU access that bypassed the caches.
    #[inline]
    pub fn uncached(&mut self, kind: Region) -> u64 {
        let c = match kind {
            Region::Ddr => {
                self.stats.unc += self.p.unc as u64;
                self.p.unc
            }
            Region::Sram => {
                self.stats.sram += self.p.sram as u64;
                self.p.sram
            }
            Region::Io => {
                self.stats.io += self.p.io as u64;
                self.p.io
            }
        };
        c as u64
    }

    /// One line for the end of a run.
    pub fn summary(&self, now: u64) -> String {
        let s = &self.stats;
        let pct = |v: u64| if now == 0 { 0.0 } else { v as f64 * 100.0 / now as f64 };
        format!(
            "timing: unit, {:.0} MHz; {} instructions; extra cycles: ops {} ({:.1}%), base {} ({:.1}%), \
             exc {} ({:.1}%), dmiss {} ({:.1}%), wb {} ({:.1}%), imiss {} ({:.1}%), unc {} ({:.1}%), \
             sram {} ({:.1}%), io {} ({:.1}%) of {} cycles",
            self.p.hz / 1e6,
            s.instructions,
            s.op_extra,
            pct(s.op_extra),
            s.base_extra,
            pct(s.base_extra),
            s.exc,
            pct(s.exc),
            s.dmiss,
            pct(s.dmiss),
            s.wb,
            pct(s.wb),
            s.imiss,
            pct(s.imiss),
            s.unc,
            pct(s.unc),
            s.sram,
            pct(s.sram),
            s.io,
            pct(s.io),
            now
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Ddr,
    Sram,
    Io,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_parse() {
        let mut p = Params::default();
        p.apply("dmiss=30, base=0.25,io=5").unwrap();
        assert_eq!((p.dmiss, p.base, p.io), (30, 0.25, 5));
        assert!(p.apply("nope=1").is_err());
        assert!(p.apply("dmiss=1.5").is_err());
        assert!(p.apply("dmiss").is_err());
    }

    #[test]
    fn op_classes() {
        let mut t = Timing::new(Params::default());
        let p = Params::default();
        assert_eq!(t.op_extra(0x4E71, 0), 0); // NOP
        assert_eq!(t.op_extra(0x4E75, 0), p.call as u64); // RTS
        assert_eq!(t.op_extra(0x4E90, 0), p.call as u64); // JSR (a0)
        assert_eq!(t.op_extra(0x6608, 0), p.branch as u64); // BNE.S
        assert_eq!(t.op_extra(0x6100, 0), p.call as u64); // BSR.W
        assert_eq!(t.op_extra(0x4C00, 0x0800), p.mul_l as u64); // MULS.L
        assert_eq!(t.op_extra(0x4C40, 0x0800), p.div_l as u64); // DIVS.L
        assert_eq!(t.op_extra(0xC1C0, 0), p.mul_w as u64); // MULS.W d0,d0
        assert_eq!(t.op_extra(0x80C1, 0), p.div_w as u64); // DIVU.W d1,d0
        assert_eq!(t.op_extra(0x48D7, 0x00FF), p.movem as u64 * 7); // MOVEM.L d0-d7,(sp)
        assert_eq!(t.op_extra(0x2001, 0), 0); // MOVE.L d1,d0
        assert_eq!(t.op_extra(0xF4F8, 0), p.sync as u64); // CPUSHL bc
    }

    #[test]
    fn base_accumulates() {
        let mut t = Timing::new(Params { base: 0.25, ..Params::default() });
        let total: u64 = (0..8).map(|_| t.op_extra(0x4E71, 0)).sum();
        assert_eq!(total, 2);
    }
}
