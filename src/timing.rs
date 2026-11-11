//! Unit timing (`DTEMU_TIMING=unit`): the clock counts core cycles, not
//! instructions, so the CPU load the firmware sees (analoghealth's CPU %,
//! DSP %, the UI tick rate) follows the unit's instead of an ideal one
//! instruction per tick. docs/TIMING.md has the sources and the fit.
//!
//! The default mode is untouched: one tick per instruction, 200 M a second.
//! In unit mode:
//! - `Io::ips` is the core clock, so every peripheral still converts through
//!   it and keeps its real rate. The default is 2 x the 132 MHz bus clock the
//!   PIT rates establish (io.rs), as the MCF5441x runs its bus at fsys/2.
//! - Instruction costs come from the MCF54418 reference manual (Rev. 8,
//!   section 3.3.5, tables 3-13 to 3-20): one cycle for most instructions,
//!   including a memory operand and a read-modify-write; +1 for the
//!   (d8,An,Xi*SF) mode and for a memory-to-memory MOVE; MUL 4, DIV.W 20,
//!   DIV.L and REM 35, MOVEM n, MOVEC 20, NOP 6, RTE 15, JSR/JMP (An) 5, ...
//! - The load-use stall of section 3.3.5.1: an address generated from a
//!   register the previous instruction changed in the execute stage waits
//!   2 cycles (base, or index scaled by 1) or 3 (index scaled by 2 or 4).
//! - Branches: the 8-entry branch cache and the 128-entry prediction table
//!   with 2-bit counters (section 3.1.1.1): a taken branch the branch cache
//!   predicts costs 0, other correct predictions 1, a misprediction 8. RTS
//!   uses the 4-entry return stack: 2 predicted, 9 mispredicted, 8 empty.
//! - EMAC: an accumulator store right after a MAC, MSAC or accumulator load
//!   takes 4 cycles; MOVE to MACSR 8, to MASK 7.
//! - Memory: the cache model (cache.rs) is turned on, at the MCF5441x's
//!   8 KB per cache (section 6.2). The manual's times assume zero-wait
//!   memory, so each D-cache line fill, dirty push and I-cache line fill
//!   costs a DDR transfer (`dmiss`, `wb`, `imiss`); a data access past the
//!   caches costs `unc_rd`/`unc_wr` (DDR through the uncached alias, which
//!   the stock CACR makes inhibited-precise: a write holds the pipeline),
//!   `sram` or `io` (peripheral registers on the fsys/2 bus). These memory
//!   numbers are not in the manual; they are fitted to readings from a unit.
//! - An interrupt or exception entry costs `exc` (TRAP's 18 in table 3-17).
//!
//! Not modelled: MOVE folding (two instructions issued together, "fold"
//! is a fitted fraction instead), the IFP/OEP decoupling that makes BRA,
//! BSR and absolute JSR take 1-3 cycles (taken as 2), stalls from other
//! register dependencies the manual doesn't list, DDR banks and refresh,
//! and bus contention with eDMA. The aim is the load within a few points.

use serde::{Deserialize, Serialize};

/// Cost parameters, in core cycles.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Params {
    /// Core clock, Hz.
    pub hz: f64,
    /// Fraction of MOVEs issued for free with the instruction before them.
    pub fold: f64,
    /// Exception or interrupt entry.
    pub exc: u32,
    /// Branch misprediction (manual: 8).
    pub mispredict: u32,
    /// D-cache line fill from DDR.
    pub dmiss: u32,
    /// Dirty D-cache line pushed to DDR (mostly hidden by the push buffer).
    pub wb: u32,
    /// I-cache line fill from DDR.
    pub imiss: u32,
    /// Data read from DDR past the D-cache (uncached alias).
    pub unc_rd: u32,
    /// Data write to DDR past the D-cache (precise: holds the pipeline).
    pub unc_wr: u32,
    /// An access to internal SRAM (single-cycle: 0 extra).
    pub sram: u32,
    /// An access to a peripheral register.
    pub io: u32,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            hz: 2.0 * crate::io::F_BUS,
            fold: 0.0,
            exc: 17,
            mispredict: 8,
            dmiss: 24,
            wb: 4,
            imiss: 24,
            unc_rd: 20,
            unc_wr: 8,
            sram: 0,
            io: 10,
        }
    }
}

impl Params {
    /// Apply `key=value,...` over these values. Keys this model doesn't know
    /// are refused, except those of other models sharing the variable
    /// (`emmc_*`), which they parse themselves.
    pub fn apply(&mut self, s: &str) -> Result<(), String> {
        for kv in s.split(',').map(str::trim).filter(|kv| !kv.is_empty()) {
            let (k, v) = kv.split_once('=').ok_or_else(|| format!("timing param `{kv}`: want key=value"))?;
            let k = k.trim();
            if k.starts_with("emmc_") {
                continue;
            }
            let f: f64 = v.trim().parse().map_err(|_| format!("timing param `{kv}`: not a number"))?;
            let u = || -> Result<u32, String> {
                if f < 0.0 || f.fract() != 0.0 {
                    Err(format!("timing param `{kv}`: want a whole number of cycles"))
                } else {
                    Ok(f as u32)
                }
            };
            match k {
                "hz" => self.hz = f,
                "fold" => self.fold = f,
                "exc" => self.exc = u()?,
                "mispredict" => self.mispredict = u()?,
                "dmiss" => self.dmiss = u()?,
                "wb" => self.wb = u()?,
                "imiss" => self.imiss = u()?,
                "unc_rd" => self.unc_rd = u()?,
                "unc_wr" => self.unc_wr = u()?,
                "sram" => self.sram = u()?,
                "io" => self.io = u()?,
                other => return Err(format!("timing param `{other}`: unknown")),
            }
        }
        if self.hz <= 0.0 || !(0.0..=1.0).contains(&self.fold) {
            return Err("timing params: hz must be > 0 and fold within 0..1".into());
        }
        Ok(())
    }
}

/// Where the cycles went, for the end-of-run summary and the fit.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stats {
    pub instructions: u64,
    /// Table cycles beyond one per instruction.
    pub table: u64,
    pub stall: u64,
    /// Branch and return-stack cycles beyond one (negative: folded branches).
    pub branch: i64,
    pub mispredicts: u64,
    pub folded: u64,
    pub exc: u64,
    pub dmiss: u64,
    pub wb: u64,
    pub imiss: u64,
    pub unc: u64,
    pub sram: u64,
    pub io: u64,
}

/// What `post` needs to know about the instruction `pre` just costed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    None,
    /// A conditional branch at this PC, falling through to `next`.
    Bcc { pc: u32, next: u32 },
    /// BSR or JSR: the return address is on the stack after it runs.
    Call,
    Rts,
}

const NONE: u8 = 0xFF;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Timing {
    pub p: Params,
    pub stats: Stats,
    /// The register (0-7 D, 8-15 A) the previous instruction changed in
    /// the execute stage, or NONE.
    last_dst: u8,
    /// The previous instruction was a MAC, MSAC or accumulator load.
    last_mac: bool,
    /// The previous instruction was folded (a pair issues once).
    last_folded: bool,
    fold_acc: f64,
    /// Branch cache: 8 direct-mapped entries (tag PC, 2-bit counter).
    bc: [(u32, u8); 8],
    /// Prediction table: 128 direct-mapped 2-bit counters.
    pt: Vec<u8>,
    /// Return stack, 4 entries (LIFO, the oldest dropped).
    rs: Vec<u32>,
}

/// Effective-address mode and register of a 6-bit field.
#[inline]
fn ea(field: u16) -> (u16, u16) {
    ((field >> 3) & 7, field & 7)
}

/// Extension words an <ea> takes (`size`: 1, 2 or 4 bytes for #imm).
fn ea_words(mode: u16, reg: u16, size: u32) -> usize {
    match mode {
        5 | 6 => 1,
        7 => match reg {
            0 | 2 | 3 => 1,
            1 => 2,
            4 => {
                if size == 4 {
                    2
                } else {
                    1
                }
            }
            _ => 0,
        },
        _ => 0,
    }
}

fn is_mem(mode: u16, reg: u16) -> bool {
    mode >= 2 && !(mode == 7 && reg == 4)
}

fn is_indexed(mode: u16, reg: u16) -> bool {
    mode == 6 || (mode == 7 && reg == 3)
}

impl Timing {
    pub fn new(p: Params) -> Timing {
        Timing {
            p,
            stats: Stats::default(),
            last_dst: NONE,
            last_mac: false,
            last_folded: false,
            fold_acc: 0.0,
            bc: [(u32::MAX, 0); 8],
            pt: vec![1; 128],
            rs: Vec::with_capacity(4),
        }
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

    /// Load-use stall for an address generated by <ea> (mode, reg), whose
    /// index extension word (if any) is `ext`.
    fn ea_stall(&self, mode: u16, reg: u16, ext: u16) -> u64 {
        if self.last_dst == NONE {
            return 0;
        }
        let mut s = 0;
        if (2..=6).contains(&mode) && self.last_dst == 8 + reg as u8 {
            s = 2;
        }
        if is_indexed(mode, reg) {
            let xi = (ext >> 12) as u8 & 15;
            let scale = (ext >> 9) & 3;
            if xi == self.last_dst {
                s = s.max(if scale == 0 { 2 } else { 3 });
            }
        }
        s
    }

    /// Cycles beyond one for the instruction at `pc` whose words start
    /// with `w` (`w[0]` the opword; 6 words, zero past the end are fine).
    /// -> (extra cycles, what `post` must check, whether a cycle is
    /// refunded for folding).
    pub fn pre(&mut self, pc: u32, w: &[u16; 6]) -> (u64, Flow, bool) {
        let op = w[0];
        let mut x: u64 = 0;
        let mut stall: u64 = 0;
        let mut flow = Flow::None;
        let mut dst = NONE;
        let mut mac = false;
        let mut is_move = false;
        let (sm, sr) = ea(op & 0x3F);
        match op >> 12 {
            // MOVE.B/.L/.W
            0x1 | 0x2 | 0x3 => {
                is_move = true;
                let size = match op >> 12 {
                    1 => 1,
                    2 => 4,
                    _ => 2,
                };
                let (dm, dr) = ((op >> 6) & 7, (op >> 9) & 7);
                let sw = ea_words(sm, sr, size);
                if is_indexed(sm, sr) {
                    x += 1;
                }
                if dm == 6 {
                    x += 1;
                }
                if is_mem(sm, sr) && dm >= 2 {
                    x += 1;
                }
                stall = self.ea_stall(sm, sr, w[1]).max(if dm >= 2 { self.ea_stall(dm, dr, w[1 + sw]) } else { 0 });
                match dm {
                    0 => dst = dr as u8,
                    1 => dst = 8 + dr as u8,
                    _ => {}
                }
            }
            0x0 => {
                // Bit ops: BTST/BCHG/BCLR/BSET, dynamic (bit 8) or static (0x08xx).
                let bitop = op & 0x0100 != 0 || op & 0x0F00 == 0x0800;
                if bitop {
                    let kind = (op >> 6) & 3; // 0 BTST, 1 BCHG, 2 BCLR, 3 BSET
                    let ext = if op & 0x0100 != 0 { 0 } else { 1 };
                    if kind != 0 || (sm == 0 && op & 0x0100 != 0) {
                        x += 1;
                    }
                    if is_indexed(sm, sr) {
                        x += 1;
                    }
                    stall = self.ea_stall(sm, sr, w[1 + ext]);
                    if sm == 0 && kind != 0 {
                        dst = sr as u8;
                    }
                } else {
                    // ORI/ANDI/SUBI/ADDI/EORI/CMPI #imm,Dx
                    if sm == 0 && (op >> 9) & 7 != 6 {
                        dst = sr as u8;
                    }
                }
            }
            0x4 => {
                match op {
                    0x4E71 => x += 5,                               // NOP: 6, syncs
                    0x4E73 => x += 14,                              // RTE: 15
                    0x4E72 => x += 5,                               // STOP: 6
                    0x4E75 => flow = Flow::Rts,                     // RTS: post
                    0x4E7B => x += 19,                              // MOVEC: 20
                    0x4E60..=0x4E6F => x += 2,                      // MOVE USP: 3
                    _ if op & 0xFFF8 == 0x4E50 => x += 1,           // LINK.W: 2
                    _ if op & 0xFFF0 == 0x4E40 => {}                // TRAP: exc
                    _ if op & 0xFF80 == 0x4E80 => {
                        // JSR (0x4E80) / JMP (0x4EC0)
                        x += match sm {
                            2 | 5 => 4,
                            6 => 5,
                            7 if sr == 3 => 5,
                            _ => 1, // abs, (d16,PC): 1-3 cycles
                        };
                        stall = self.ea_stall(sm, sr, w[1]);
                        if op & 0x0040 == 0 {
                            flow = Flow::Call;
                        }
                    }
                    _ if op & 0xFFC0 == 0x4C00 => {
                        // MULx.L: 4
                        x += 3;
                        stall = self.ea_stall(sm, sr, w[2]);
                        dst = ((w[1] >> 12) & 7) as u8;
                    }
                    _ if op & 0xFFC0 == 0x4C40 => {
                        // DIVx.L / REMx.L: 35
                        x += 34;
                        stall = self.ea_stall(sm, sr, w[2]);
                        dst = ((w[1] >> 12) & 7) as u8;
                    }
                    _ if (op & 0xFFC0 == 0x48C0 || op & 0xFFC0 == 0x4CC0) && (sm == 2 || sm == 5) => {
                        // MOVEM.L: n
                        x += (w[1].count_ones() as u64).saturating_sub(1);
                        stall = self.ea_stall(sm, sr, 0);
                    }
                    _ if op & 0xFFC0 == 0x46C0 => {
                        // MOVE to SR: 4 (1 with #imm and bit 13 set)
                        if !(sm == 7 && sr == 4 && w[1] & 0x2000 != 0) {
                            x += 3;
                        }
                    }
                    _ if op & 0xF1C0 == 0x41C0 => {
                        // LEA: the address ALU, forwarded without stalls.
                        if is_indexed(sm, sr) {
                            x += 1;
                        }
                        stall = self.ea_stall(sm, sr, w[1]);
                    }
                    _ if op & 0xFFC0 == 0x4840 => {
                        // PEA (0x4840) / SWAP (mode 0)
                        if sm == 0 {
                            dst = sr as u8;
                        } else {
                            if is_indexed(sm, sr) {
                                x += 1;
                            }
                            stall = self.ea_stall(sm, sr, w[1]);
                        }
                    }
                    _ => {
                        // CLR, NEG, NEGX, NOT, EXT, TST, TAS, MOVE from SR...
                        if is_indexed(sm, sr) {
                            x += 1;
                        }
                        stall = self.ea_stall(sm, sr, w[1]);
                        if sm == 0 && op & 0x0F00 != 0x0A00 {
                            dst = sr as u8;
                        }
                    }
                }
            }
            0x5 => {
                // ADDQ/SUBQ/Scc/TPF
                if is_indexed(sm, sr) {
                    x += 1;
                }
                stall = self.ea_stall(sm, sr, w[1]);
                match sm {
                    0 => dst = sr as u8,
                    1 => dst = 8 + sr as u8,
                    _ => {}
                }
            }
            0x6 => {
                let cond = (op >> 8) & 15;
                let d8 = op & 0xFF;
                let len = match d8 {
                    0 => 4,
                    0xFF => 6,
                    _ => 2,
                };
                match cond {
                    0 => x += 1, // BRA: 1-3
                    1 => {
                        x += 1; // BSR: 1-3
                        flow = Flow::Call;
                    }
                    _ => flow = Flow::Bcc { pc, next: pc.wrapping_add(len) },
                }
            }
            0x7 => dst = ((op >> 9) & 7) as u8, // MOVEQ / MVS / MVZ
            0x8 | 0xC => {
                let opmode = (op >> 6) & 7;
                let dx = ((op >> 9) & 7) as u8;
                if opmode == 3 || opmode == 7 {
                    // DIVx.W 20 / MULx.W 4
                    x += if op >> 12 == 8 { 19 } else { 3 };
                    dst = dx;
                } else if opmode < 3 {
                    dst = dx;
                }
                if is_indexed(sm, sr) {
                    x += 1;
                }
                stall = self.ea_stall(sm, sr, w[1]);
            }
            0x9 | 0xB | 0xD => {
                let opmode = (op >> 6) & 7;
                let rx = ((op >> 9) & 7) as u8;
                if is_indexed(sm, sr) {
                    x += 1;
                }
                stall = self.ea_stall(sm, sr, w[1]);
                let cmp = op >> 12 == 0xB && (opmode < 3 || opmode == 7 || opmode == 3);
                if !cmp {
                    match opmode {
                        0..=2 => dst = rx,
                        3 | 7 => dst = 8 + rx,
                        _ if sm == 0 && op >> 12 == 0xB => dst = sr as u8, // EOR Dy,Dx
                        _ if sm == 0 => dst = rx,                          // ADDX / SUBX Dy,Dx
                        _ => {}
                    }
                }
            }
            0xA => {
                // EMAC.
                let movacc_to = op & 0xF9C0 == 0xA100; // MOVE.L <ea>,ACCx
                if op & 0xFFC0 == 0xA900 {
                    x += 7; // MOVE.L <ea>,MACSR: 8
                } else if op & 0xFFC0 == 0xAD00 {
                    x += 6; // MOVE.L <ea>,MASK: 7
                } else if op & 0xF9F0 == 0xA180 || op & 0xFFF0 == 0xA980 || op & 0xFFF0 == 0xAD80 || op & 0xFFF0 == 0xAB80 || op & 0xFFF0 == 0xAF80 {
                    // MOVE.L ACCx/MACSR/MASK/ACCEXT,Rx: 4 right after a MAC.
                    if self.last_mac {
                        x += 3;
                    }
                    dst = (op & 15) as u8;
                } else if movacc_to {
                    mac = true;
                } else {
                    // MAC/MSAC, maybe with a load (bit 7 of the opword set
                    // means register form for some encodings; the load forms
                    // write Rw).
                    mac = true;
                    stall = self.ea_stall(sm, sr, 0);
                }
            }
            0xE => {
                if op & 0x00C0 != 0x00C0 {
                    dst = (op & 7) as u8;
                }
            }
            0xF => {
                if op & 0xFF00 == 0xF400 {
                    x += match (op >> 6) & 3 {
                        1 => 11, // dc: 12
                        _ => 17, // ic, bc: 18
                    };
                    if op & 0xFFF8 == 0xF428 {
                        x = 18; // INTOUCH: 19
                    }
                }
            }
            _ => {}
        }
        // MOVE folding: a MOVE after an unfolded instruction issues with it.
        let mut folded = false;
        if is_move && !self.last_folded && self.p.fold > 0.0 && stall == 0 && x == 0 {
            self.fold_acc += self.p.fold;
            if self.fold_acc >= 1.0 {
                self.fold_acc -= 1.0;
                folded = true;
                self.stats.folded += 1;
            }
        }
        self.last_folded = folded;
        self.last_dst = dst;
        self.last_mac = mac;
        self.stats.instructions += 1;
        self.stats.table += x;
        self.stats.stall += stall;
        (x + stall, flow, folded)
    }

    /// After the instruction ran: branch prediction and the return stack.
    /// `pc_after` is the PC it left; `sp_top` the long at (A7).
    /// -> cycles to add, beyond the one the instruction already took; -1
    /// for a taken branch the branch cache folds away (0 cycles).
    pub fn post(&mut self, flow: Flow, pc_after: u32, sp_top: u32) -> i64 {
        let c = match flow {
            Flow::None => 0,
            Flow::Call => {
                if self.rs.len() == 4 {
                    self.rs.remove(0);
                }
                self.rs.push(sp_top);
                0
            }
            Flow::Rts => match self.rs.pop() {
                Some(a) if a == pc_after => 1,
                Some(_) => 8,
                None => 7,
            },
            Flow::Bcc { pc, next } => {
                let taken = pc_after != next;
                let bi = ((pc >> 1) & 7) as usize;
                let pi = ((pc >> 1) & 127) as usize;
                let (hit, ctr) = if self.bc[bi].0 == pc { (true, self.bc[bi].1) } else { (false, self.pt[pi]) };
                let pred = ctr >= 2;
                let cost: i64 = if pred != taken {
                    self.stats.mispredicts += 1;
                    self.p.mispredict as i64 - 1
                } else if taken && hit {
                    -1
                } else {
                    0
                };
                let n = if taken { (ctr + 1).min(3) } else { ctr.saturating_sub(1) };
                if hit {
                    self.bc[bi].1 = n;
                } else if taken {
                    // Taken branches go into the branch cache.
                    self.bc[bi] = (pc, n);
                }
                self.pt[pi] = n;
                cost
            }
        };
        self.stats.branch += c;
        c
    }

    #[inline]
    pub fn exception(&mut self) -> u64 {
        self.last_dst = NONE;
        self.last_mac = false;
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
    pub fn uncached(&mut self, kind: Region, write: bool) -> u64 {
        let c = match kind {
            Region::Ddr => {
                let c = if write { self.p.unc_wr } else { self.p.unc_rd };
                self.stats.unc += c as u64;
                c
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
        let parts = [
            ("table", s.table),
            ("stall", s.stall),
            ("exc", s.exc),
            ("dmiss", s.dmiss),
            ("wb", s.wb),
            ("imiss", s.imiss),
            ("unc", s.unc),
            ("sram", s.sram),
            ("io", s.io),
        ];
        let mut out = format!(
            "timing: unit, {:.0} MHz, {} cycles, {} instructions ({} folded, {} mispredicted branches); extra:",
            self.p.hz / 1e6,
            now,
            s.instructions,
            s.folded,
            s.mispredicts
        );
        for (k, v) in parts {
            out += &format!(" {k} {v} ({:.1}%)", pct(v));
        }
        out += &format!(" branch {}", s.branch);
        out
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

    fn cost(t: &mut Timing, w: &[u16]) -> u64 {
        let mut a = [0u16; 6];
        a[..w.len()].copy_from_slice(w);
        t.pre(0x4000_0000, &a).0
    }

    #[test]
    fn params_parse() {
        let mut p = Params::default();
        p.apply("dmiss=30, fold=0.25,io=5, emmc_write_us=200").unwrap();
        assert_eq!((p.dmiss, p.fold, p.io), (30, 0.25, 5));
        assert!(p.apply("nope=1").is_err());
        assert!(p.apply("dmiss=1.5").is_err());
        assert!(p.apply("dmiss").is_err());
        assert!(p.apply("fold=2").is_err());
    }

    /// Table values from MCF54418RM section 3.3.5 (extra = table - 1).
    #[test]
    fn manual_tables() {
        let mut t = Timing::new(Params::default());
        assert_eq!(cost(&mut t, &[0x2001]), 0); // MOVE.L d1,d0: 1
        assert_eq!(cost(&mut t, &[0x2290]), 1); // MOVE.L (a0),(a1): 2
        assert_eq!(cost(&mut t, &[0x2030, 0x1800]), 1); // MOVE.L (0,a0,d1.l),d0: 2
        assert_eq!(cost(&mut t, &[0x4E71]), 5); // NOP: 6
        assert_eq!(cost(&mut t, &[0x4E73]), 14); // RTE: 15
        assert_eq!(cost(&mut t, &[0x4E7B, 0x0002]), 19); // MOVEC: 20
        assert_eq!(cost(&mut t, &[0x4C00, 0x0800]), 3); // MULS.L d0,d0: 4
        assert_eq!(cost(&mut t, &[0x4C41, 0x0800]), 34); // DIVS.L d1,d0: 35
        assert_eq!(cost(&mut t, &[0xC1C1]), 3); // MULS.W d1,d0: 4
        assert_eq!(cost(&mut t, &[0x80C1]), 19); // DIVU.W d1,d0: 20
        assert_eq!(cost(&mut t, &[0x48D7, 0x00FF]), 7); // MOVEM.L d0-d7,(sp): 8
        assert_eq!(cost(&mut t, &[0x4E90]), 4); // JSR (a0): 5
        assert_eq!(cost(&mut t, &[0x4EB0, 0x0800]), 5); // JSR (0,a0,d0): 6
        assert_eq!(cost(&mut t, &[0x08D0, 0x0003]), 1); // BSET #3,(a0): 2
        assert_eq!(cost(&mut t, &[0xF4E8]), 17); // CPUSHL bc,(a0): 18
        assert_eq!(cost(&mut t, &[0x46FC, 0x2700]), 0); // MOVE #$2700,SR: 1
        assert_eq!(cost(&mut t, &[0x46C0]), 3); // MOVE d0,SR: 4
    }

    #[test]
    fn load_use_stall() {
        let mut t = Timing::new(Params::default());
        // MULS.L #..,d0 then MOVE.L (0,a0,d0.l*4),d1: 3-cycle stall.
        cost(&mut t, &[0x4C3C, 0x0800, 0, 4]);
        assert_eq!(cost(&mut t, &[0x2230, 0x0C00]), 1 + 3);
        // ADDQ.L #4,a0 then MOVE.L (a0),d1: base register, 2 cycles.
        cost(&mut t, &[0x5888]);
        assert_eq!(cost(&mut t, &[0x2210]), 2);
        // ...but not when an unrelated instruction sits between.
        cost(&mut t, &[0x5888]);
        cost(&mut t, &[0x7001]);
        assert_eq!(cost(&mut t, &[0x2210]), 0);
        // LEA forwards: no stall.
        cost(&mut t, &[0x41E8, 0x0010]);
        assert_eq!(cost(&mut t, &[0x2210]), 0);
    }

    #[test]
    fn branch_prediction() {
        let mut t = Timing::new(Params::default());
        let pc = 0x4000_1000;
        let mut costs = vec![];
        for _ in 0..4 {
            let (_, f, _) = t.pre(pc, &[0x66FE, 0, 0, 0, 0, 0]); // BNE.S *
            costs.push(t.post(f, pc, 0)); // taken (back to itself)
        }
        // Weakly not-taken at first: a mispredict (8 cycles), then in the
        // branch cache and predicted taken: 0 cycles (one refunded).
        assert_eq!(costs, vec![7, -1, -1, -1]);
        // Falls through: mispredicted.
        let (_, f, _) = t.pre(pc, &[0x66FE, 0, 0, 0, 0, 0]);
        assert_eq!(t.post(f, pc + 2, 0), 7);
    }

    #[test]
    fn return_stack() {
        let mut t = Timing::new(Params::default());
        let (_, f, _) = t.pre(0x4000_0000, &[0x4E90, 0, 0, 0, 0, 0]);
        t.post(f, 0x4000_2000, 0x4000_0002);
        let (_, f, _) = t.pre(0x4000_2000, &[0x4E75, 0, 0, 0, 0, 0]);
        assert_eq!(t.post(f, 0x4000_0002, 0), 1); // RTS predicted: 2
        let (_, f, _) = t.pre(0x4000_2000, &[0x4E75, 0, 0, 0, 0, 0]);
        assert_eq!(t.post(f, 0x4000_0010, 0), 7); // stack empty: 8
    }

    #[test]
    fn folding() {
        let mut t = Timing::new(Params { fold: 0.5, ..Params::default() });
        let n = (0..8).filter(|_| t.pre(0, &[0x2001, 0, 0, 0, 0, 0]).2).count();
        // A pair issues once, so a folded MOVE never folds the next one.
        assert_eq!(n, 3);
    }
}
