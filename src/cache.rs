//! An optional model of the ColdFire V4 caches (`DTEMU_CACHE=1`, or `=log`):
//! a copyback/writethrough D-cache and an I-cache, so that a firmware mod that
//! writes code and runs it without pushing the D-cache and invalidating the
//! I-cache fails here as it would on the device. docs/CACHE.md.
//!
//! Geometry: 4 ways, 16-byte lines [V4 core: 4-way, 16-byte lines]; 16 KB
//! each by default, so 256 sets (`DTEMU_CACHE_KB` to change it) -- the
//! MCF5441x size is not verified against its reference manual [inferred].
//! CPUSHL (An) names a line by set and way, not by address: set = An[11:4]
//! (for 256 sets), way = An[1:0] (CFPRM CPUSHL). Replacement: an invalid way
//! first, else a round-robin pointer per set [inferred; the V4's exact
//! policy is not modelled].
//!
//! What it sees: the CPU's data accesses (`Bus::read*`/`write*`) and its
//! instruction fetches (`Bus::ifetch*`). eDMA (`Bus::in_dma`) reads and writes
//! memory, past the caches, as on the device. `peek`/`poke` (the debugger,
//! boot loading and the machine's stand-ins) are coherent: a peek sees a
//! cached line, a poke updates memory and any line holding the address.
//! Only DDR (0x40000000-0x7FFFFFFF) is cached; SRAM and I/O never are.
//!
//! Cacheability (MCF5441x CACR / ACR, as Linux's m54xxacr.h names the bits):
//! data by ACR0/ACR1, else CACR[DDCM]; instructions by ACR2/ACR3, else
//! CACR[IDCM]. The stock OS writes CACR 0xa50ce100 (DEC, DESB, DDCM precise
//! inhibited, DCINVA, BEC, BCINVA, IEC, DNFB, ICINVA) and ACR0 0x4007e020
//! (0x40000000-0x47FFFFFF copyback), so 0x48000000+ is an uncached data alias
//! and instructions are cached everywhere in DDR. The ACR S field (user /
//! supervisor match), the store buffer, half-locking and the branch cache are
//! not modelled.

use std::collections::BTreeMap;

pub const LINE: u32 = 16;
pub const WAYS: usize = 4;

const CACR_DEC: u32 = 0x8000_0000;
const CACR_DPI: u32 = 0x1000_0000;
const CACR_DDCM: u32 = 0x0600_0000;
const CACR_DCINVA: u32 = 0x0100_0000;
const CACR_IEC: u32 = 0x0000_8000;
const CACR_IDPI: u32 = 0x0000_1000;
const CACR_IDCM: u32 = 0x0000_0400;
const CACR_ICINVA: u32 = 0x0000_0100;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    WriteThrough,
    CopyBack,
    Inhibited,
}

#[derive(Clone, Copy, Default)]
struct Line {
    tag: u32,
    valid: bool,
    dirty: bool,
    data: [u8; LINE as usize],
}

/// What a stale fetch was: the I-cache line no longer matches memory (no
/// I-cache invalidate after memory changed), or memory does not hold what
/// the CPU wrote, which sits dirty in the D-cache (no push).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Stale {
    ICache,
    NoPush,
}

#[derive(Default, Debug, Clone)]
pub struct Stats {
    pub d_hits: u64,
    pub d_misses: u64,
    pub d_writebacks: u64,
    /// Dirty lines dropped without a push (CACR[DCINVA], or CPUSHL with DPI clear never drops).
    pub d_dirty_lost: u64,
    pub i_hits: u64,
    pub i_misses: u64,
    pub cpushl_d: u64,
    pub cpushl_i: u64,
    pub d_inval_all: u64,
    pub i_inval_all: u64,
    /// Instruction fetches whose bytes differ from the CPU's view of memory.
    pub stale_icache: u64,
    pub stale_nopush: u64,
}

pub struct Caches {
    sets: usize,
    d: Vec<Line>,
    i: Vec<Line>,
    d_rr: Vec<u8>,
    i_rr: Vec<u8>,
    pub cacr: u32,
    /// ACR0-ACR3.
    pub acr: [u32; 4],
    pub stats: Stats,
    /// Print each stale fetch (the first 64) as it happens (`DTEMU_CACHE=log`).
    pub log: bool,
    /// Stale fetches by (kind, line address) -> (first pc, count).
    pub stale: BTreeMap<(Stale, u32), (u32, u64)>,
    logged: u32,
    /// Bumped whenever an I-cache line or the configuration changes, and
    /// whenever a D-cache line is filled or invalidated: the fetch memo
    /// below is good while both match.
    i_gen: u64,
    d_gen: u64,
    /// The last fetch's line: (line address, I-cache index, D-cache index
    /// holding the same line, i_gen, d_gen).
    memo: (u32, usize, Option<usize>, u64, u64),
}

/// `n` (2 or 4) bytes of `s` as a big-endian word.
#[inline(always)]
fn be(s: &[u8], n: u32) -> u32 {
    if n == 2 {
        u16::from_be_bytes([s[0], s[1]]) as u32
    } else {
        u32::from_be_bytes([s[0], s[1], s[2], s[3]])
    }
}

/// The ACR address match: base A[31:24] against the mask's don't-care bits.
fn acr_match(acr: u32, a: u32) -> bool {
    if acr & 0x8000 == 0 {
        return false;
    }
    let base = acr >> 24;
    let mask = (acr >> 16) & 0xFF;
    ((a >> 24) & !mask & 0xFF) == (base & !mask)
}

impl Caches {
    pub fn new(kb: usize, log: bool) -> Caches {
        let sets = (kb * 1024 / (LINE as usize * WAYS)).max(1).next_power_of_two();
        Caches {
            sets,
            d: vec![Line::default(); sets * WAYS],
            i: vec![Line::default(); sets * WAYS],
            d_rr: vec![0; sets],
            i_rr: vec![0; sets],
            cacr: 0,
            acr: [0; 4],
            stats: Stats::default(),
            log,
            stale: BTreeMap::new(),
            logged: 0,
            i_gen: 1,
            d_gen: 1,
            memo: (0, 0, None, 0, 0),
        }
    }

    /// From the environment: None unless `DTEMU_CACHE` is set (and not 0).
    pub fn from_env() -> Option<Caches> {
        let v = std::env::var("DTEMU_CACHE").ok()?;
        if v.is_empty() || v == "0" {
            return None;
        }
        let kb = std::env::var("DTEMU_CACHE_KB").ok().and_then(|s| s.parse().ok()).unwrap_or(16);
        Some(Caches::new(kb, v == "log"))
    }

    /// Empty both caches (a snapshot load replaces memory under them).
    /// Set CACR and ACR0-3 without the invalidate bits acting.
    pub fn configure(&mut self, cacr: u32, acr: [u32; 4]) {
        self.cacr = cacr;
        self.acr = acr;
        self.i_gen += 1;
    }

    pub fn set_acr(&mut self, n: usize, v: u32) {
        self.acr[n] = v;
        self.i_gen += 1;
    }

    pub fn reset(&mut self) {
        self.i_gen += 1;
        self.d_gen += 1;
        self.d.iter_mut().chain(self.i.iter_mut()).for_each(|l| *l = Line::default());
    }

    pub fn sets(&self) -> usize {
        self.sets
    }

    #[inline(always)]
    fn set_of(&self, a: u32) -> usize {
        ((a / LINE) as usize) & (self.sets - 1)
    }

    /// A MOVEC to CACR: the invalidate-all bits act now.
    pub fn set_cacr(&mut self, v: u32) {
        self.cacr = v;
        self.i_gen += 1;
        self.d_gen += 1;
        if v & CACR_DCINVA != 0 {
            self.stats.d_inval_all += 1;
            for l in &mut self.d {
                if l.valid && l.dirty {
                    self.stats.d_dirty_lost += 1;
                }
                *l = Line::default();
            }
        }
        if v & CACR_ICINVA != 0 {
            self.stats.i_inval_all += 1;
            self.i.iter_mut().for_each(|l| *l = Line::default());
        }
    }

    pub fn data_mode(&self, a: u32) -> Mode {
        if self.cacr & CACR_DEC == 0 {
            return Mode::Inhibited;
        }
        let cm = if acr_match(self.acr[0], a) {
            (self.acr[0] >> 5) & 3
        } else if acr_match(self.acr[1], a) {
            (self.acr[1] >> 5) & 3
        } else {
            (self.cacr & CACR_DDCM) >> 25
        };
        match cm {
            0 => Mode::WriteThrough,
            1 => Mode::CopyBack,
            _ => Mode::Inhibited,
        }
    }

    pub fn insn_cached(&self, a: u32) -> bool {
        if self.cacr & CACR_IEC == 0 {
            return false;
        }
        let cm = if acr_match(self.acr[2], a) {
            (self.acr[2] >> 5) & 3
        } else if acr_match(self.acr[3], a) {
            (self.acr[3] >> 5) & 3
        } else if self.cacr & CACR_IDCM != 0 {
            2
        } else {
            0
        };
        cm < 2
    }

    fn find(lines: &[Line], set: usize, la: u32) -> Option<usize> {
        (0..WAYS).map(|w| set * WAYS + w).find(|&k| lines[k].valid && lines[k].tag == la)
    }

    /// Write line `k` of the D-cache back to memory if it is dirty.
    fn writeback(&mut self, ddr: &mut [u8], mask: u32, k: usize) {
        let l = &mut self.d[k];
        if l.valid && l.dirty {
            let o = (l.tag & mask) as usize;
            ddr[o..o + LINE as usize].copy_from_slice(&l.data);
            l.dirty = false;
            self.stats.d_writebacks += 1;
        }
    }

    /// The D-cache line for `la`, filled from memory on a miss (evicting a
    /// victim, written back if dirty).
    fn d_line(&mut self, ddr: &mut [u8], mask: u32, la: u32) -> usize {
        let set = self.set_of(la);
        if let Some(k) = Self::find(&self.d, set, la) {
            self.stats.d_hits += 1;
            return k;
        }
        self.stats.d_misses += 1;
        let way = match (0..WAYS).find(|&w| !self.d[set * WAYS + w].valid) {
            Some(w) => w,
            None => {
                let w = self.d_rr[set] as usize;
                self.d_rr[set] = ((w + 1) % WAYS) as u8;
                w
            }
        };
        let k = set * WAYS + way;
        self.writeback(ddr, mask, k);
        self.d_gen += 1;
        let o = (la & mask) as usize;
        let l = &mut self.d[k];
        l.tag = la;
        l.valid = true;
        l.dirty = false;
        l.data.copy_from_slice(&ddr[o..o + LINE as usize]);
        k
    }

    /// A CPU data read of `n` bytes at DDR address `a`. None when the
    /// address is not data-cacheable (the caller reads memory).
    pub fn read(&mut self, ddr: &mut [u8], mask: u32, a: u32, n: u32) -> Option<u32> {
        if self.data_mode(a) == Mode::Inhibited {
            return None;
        }
        let mut v = 0u32;
        if (a % LINE) + n <= LINE {
            let k = self.d_line(ddr, mask, a & !(LINE - 1));
            let off = (a % LINE) as usize;
            for b in &self.d[k].data[off..off + n as usize] {
                v = v << 8 | *b as u32;
            }
        } else {
            for i in 0..n {
                let x = a.wrapping_add(i);
                let k = self.d_line(ddr, mask, x & !(LINE - 1));
                v = v << 8 | self.d[k].data[(x % LINE) as usize] as u32;
            }
        }
        Some(v)
    }

    /// A CPU data write. false when not data-cacheable (the caller writes
    /// memory). Copyback allocates and dirties the line; writethrough
    /// writes memory and updates a line that hits.
    pub fn write(&mut self, ddr: &mut [u8], mask: u32, a: u32, n: u32, v: u32) -> bool {
        let mode = self.data_mode(a);
        if mode == Mode::Inhibited {
            return false;
        }
        for i in 0..n {
            let x = a.wrapping_add(i);
            let b = (v >> (8 * (n - 1 - i))) as u8;
            let la = x & !(LINE - 1);
            if mode == Mode::CopyBack {
                let k = self.d_line(ddr, mask, la);
                let l = &mut self.d[k];
                l.data[(x % LINE) as usize] = b;
                l.dirty = true;
            } else {
                ddr[(x & mask) as usize] = b;
                let set = self.set_of(la);
                if let Some(k) = Self::find(&self.d, set, la) {
                    self.d[k].data[(x % LINE) as usize] = b;
                }
            }
        }
        true
    }

    /// The I-cache line for `la`, filled from MEMORY (never the D-cache).
    fn i_line(&mut self, ddr: &[u8], mask: u32, la: u32) -> usize {
        let set = self.set_of(la);
        if let Some(k) = Self::find(&self.i, set, la) {
            self.stats.i_hits += 1;
            return k;
        }
        self.stats.i_misses += 1;
        let way = match (0..WAYS).find(|&w| !self.i[set * WAYS + w].valid) {
            Some(w) => w,
            None => {
                let w = self.i_rr[set] as usize;
                self.i_rr[set] = ((w + 1) % WAYS) as u8;
                w
            }
        };
        let k = set * WAYS + way;
        self.i_gen += 1;
        let o = (la & mask) as usize;
        let l = &mut self.i[k];
        l.tag = la;
        l.valid = true;
        l.data.copy_from_slice(&ddr[o..o + LINE as usize]);
        k
    }

    /// An instruction fetch of `n` bytes at DDR address `a`, `pc` the
    /// instruction's. None when not instruction-cacheable (the caller
    /// reads memory). Checks each byte against memory and the D-cache.
    pub fn ifetch(&mut self, ddr: &[u8], mask: u32, a: u32, n: u32, pc: u32) -> Option<u32> {
        let la = a & !(LINE - 1);
        let off = (a % LINE) as usize;
        if off + n as usize <= LINE as usize {
            // Within one line: the memo saves the lookups while nothing changed.
            let (ml, mut k, mut dk, ig, dg) = self.memo;
            if ml != la || ig != self.i_gen {
                if !self.insn_cached(a) {
                    return None;
                }
                k = self.i_line(ddr, mask, la);
                dk = Self::find(&self.d, self.set_of(la), la);
            } else {
                self.stats.i_hits += 1;
                if dg != self.d_gen {
                    dk = Self::find(&self.d, self.set_of(la), la);
                }
            }
            self.memo = (la, k, dk, self.i_gen, self.d_gen);
            let o = (a & mask) as usize;
            let v = be(&self.i[k].data[off..], n);
            let m = be(&ddr[o..], n);
            let dirty = dk.is_some_and(|d| self.d[d].dirty && be(&self.d[d].data[off..], n) != m);
            if v != m || dirty {
                for i in 0..n as usize {
                    let (b, m) = (self.i[k].data[off + i], ddr[o + i]);
                    let x = a.wrapping_add(i as u32);
                    if b != m {
                        self.note_stale(Stale::ICache, la, pc, x, b, m);
                    } else if let Some(d) = dk.filter(|&d| self.d[d].dirty) {
                        let db = self.d[d].data[off + i];
                        if db != m {
                            self.note_stale(Stale::NoPush, la, pc, x, db, m);
                        }
                    }
                }
            }
            return Some(v);
        }
        if !self.insn_cached(a) {
            return None;
        }
        let mut v = 0u32;
        for i in 0..n {
            let x = a.wrapping_add(i);
            let la = x & !(LINE - 1);
            let k = self.i_line(ddr, mask, la);
            let b = self.i[k].data[(x % LINE) as usize];
            v = v << 8 | b as u32;
            let m = ddr[(x & mask) as usize];
            if b != m {
                self.note_stale(Stale::ICache, la, pc, x, b, m);
            } else if let Some(dk) = Self::find(&self.d, self.set_of(la), la) {
                let d = self.d[dk].data[(x % LINE) as usize];
                if self.d[dk].dirty && d != m {
                    self.note_stale(Stale::NoPush, la, pc, x, d, m);
                }
            }
        }
        Some(v)
    }

    #[cold]
    fn note_stale(&mut self, kind: Stale, la: u32, pc: u32, x: u32, got: u8, want: u8) {
        match kind {
            Stale::ICache => self.stats.stale_icache += 1,
            Stale::NoPush => self.stats.stale_nopush += 1,
        }
        let e = self.stale.entry((kind, la)).or_insert((pc, 0));
        e.1 += 1;
        if self.log && self.logged < 64 {
            self.logged += 1;
            match kind {
                Stale::ICache => eprintln!(
                    "cache: pc {pc:08x} fetches {x:08x} = {got:02x} from a stale I-cache line; memory holds {want:02x} (no I-cache invalidate)"
                ),
                Stale::NoPush => eprintln!(
                    "cache: pc {pc:08x} fetches {x:08x} = {want:02x} from memory; the D-cache holds {got:02x} dirty (no D-cache push)"
                ),
            }
        }
    }

    /// CPUSHL dc/ic/bc (An): `which` bit 0 the D-cache, bit 1 the I-cache;
    /// the line at set An[..:4], way An[1:0].
    pub fn cpushl(&mut self, ddr: &mut [u8], mask: u32, which: u8, an: u32) {
        let k = self.set_of(an) * WAYS + (an & 3) as usize;
        self.i_gen += 1;
        self.d_gen += 1;
        if which & 1 != 0 {
            self.stats.cpushl_d += 1;
            self.writeback(ddr, mask, k);
            if self.cacr & CACR_DPI == 0 {
                self.d[k].valid = false;
            }
        }
        if which & 2 != 0 {
            self.stats.cpushl_i += 1;
            if self.cacr & CACR_IDPI == 0 {
                self.i[k].valid = false;
            }
        }
    }

    /// INTOUCH (An): load the line holding An into the I-cache.
    pub fn intouch(&mut self, ddr: &[u8], mask: u32, a: u32) {
        if self.insn_cached(a) {
            self.i_line(ddr, mask, a & !(LINE - 1));
        }
    }

    /// The D-cache's copy of byte `a`, if a line holds it (a coherent peek).
    pub fn peek(&self, a: u32) -> Option<u8> {
        let la = a & !(LINE - 1);
        Self::find(&self.d, self.set_of(la), la).map(|k| self.d[k].data[(a % LINE) as usize])
    }

    /// A coherent poke: any D- or I-cache line holding `a` takes the byte
    /// too (memory is written by the caller).
    pub fn poke(&mut self, a: u32, v: u8) {
        let la = a & !(LINE - 1);
        let set = self.set_of(la);
        if let Some(k) = Self::find(&self.d, set, la) {
            self.d[k].data[(a % LINE) as usize] = v;
        }
        if let Some(k) = Self::find(&self.i, set, la) {
            self.i[k].data[(a % LINE) as usize] = v;
        }
    }

    /// Dirty lines now in the D-cache.
    pub fn dirty_lines(&self) -> usize {
        self.d.iter().filter(|l| l.valid && l.dirty).count()
    }

    /// A summary for the end of a run.
    pub fn report(&self) -> String {
        let s = &self.stats;
        let mut r = format!(
            "cache: {} KB x2, {} sets; D hits {} misses {} writebacks {} dirty-lost {} (now dirty {}); I hits {} misses {}; cpushl d {} i {}; invalidate-all d {} i {}; stale fetches: I-cache {} no-push {}\n",
            self.sets * WAYS * LINE as usize / 1024,
            self.sets,
            s.d_hits,
            s.d_misses,
            s.d_writebacks,
            s.d_dirty_lost,
            self.dirty_lines(),
            s.i_hits,
            s.i_misses,
            s.cpushl_d,
            s.cpushl_i,
            s.d_inval_all,
            s.i_inval_all,
            s.stale_icache,
            s.stale_nopush
        );
        for ((kind, la), (pc, n)) in self.stale.iter().take(64) {
            r += &format!("cache: stale {kind:?} line {la:08x} first pc {pc:08x} x{n}\n");
        }
        r
    }
}
