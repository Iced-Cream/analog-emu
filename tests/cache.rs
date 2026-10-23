//! The cache model (DTEMU_CACHE, src/cache.rs): a guest program that rewrites
//! a routine and calls it again runs the old code unless it pushes the
//! D-cache and invalidates the I-cache first, as on the device.

use dtemu::bus::Bus;
use dtemu::cache::Caches;
use dtemu::cpu::{Cpu, Stop};

const CODE: u32 = 0x4010_0000;
const TARGET: u32 = 0x4020_0000;

#[derive(Clone, Copy, PartialEq)]
enum Flush {
    None,
    /// cpushl bc over every set and way.
    PushInvalidate,
    /// cpushl dc over every set and way: memory updated, I-cache not.
    PushOnly,
    /// cpushl dc over every set and way, then CACR with ICINVA.
    PushThenIcinva,
}

fn l(v: u32) -> [u16; 2] {
    [(v >> 16) as u16, v as u16]
}

/// Loop over 4 ways x 512 sets (enough for a 32 KB cache; a 16 KB one
/// ignores An[12]) issuing `cpushl` with the given opcode on (a0).
fn flush_loop(op: u16) -> Vec<u16> {
    let mut w = vec![0x7400]; // moveq #0,d2
    w.extend([0x2042]); // wl: movea.l d2,a0
    w.extend([0x223C]); // move.l #512,d1
    w.extend(l(512));
    w.extend([op, 0x41E8, 0x0010, 0x5381, 0x66F6]); // sl: cpushl (a0); lea 16(a0),a0; subq.l #1,d1; bne.s sl
    w.extend([0x5282, 0x0C82]); // addq.l #1,d2; cmpi.l #4,d2
    w.extend(l(4));
    w.push(0x66E4); // bne.s wl
    w
}

/// Run the program: set CACR/ACR0 as the stock OS does, optionally call
/// TARGET once (`prime`, so the I-cache holds it), rewrite TARGET through the
/// data cache (`moveq #1` becomes `moveq #2`), flush as asked, call TARGET
/// again. -> (d0 from the second call, the CPU).
fn run(cache: bool, prime: bool, flush: Flush) -> (u32, Cpu) {
    let mut c = Cpu::new(Bus::new(128));
    if cache {
        c.bus.cache = Some(Box::new(Caches::new(16, false)));
    }
    let mut w: Vec<u16> = vec![0x203C];
    w.extend(l(0xa50c_e100));
    w.extend([0x4E7B, 0x0002]); // movec d0,cacr
    w.push(0x203C);
    w.extend(l(0x4007_e020));
    w.extend([0x4E7B, 0x0004]); // movec d0,acr0
    if prime {
        w.push(0x4EB9); // jsr TARGET
        w.extend(l(TARGET));
    }
    w.push(0x23FC); // move.l #(moveq #2,d0; rts),TARGET
    w.extend(l(0x7002_4E75));
    w.extend(l(TARGET));
    match flush {
        Flush::None => {}
        Flush::PushInvalidate => w.extend(flush_loop(0xF4E8)),
        Flush::PushOnly => w.extend(flush_loop(0xF468)),
        Flush::PushThenIcinva => {
            w.extend(flush_loop(0xF468));
            w.push(0x203C);
            w.extend(l(0xa50c_e100 & !0x0100_0000)); // ICINVA, not DCINVA
            w.extend([0x4E7B, 0x0002]);
        }
    }
    w.push(0x7000); // moveq #0,d0
    w.push(0x4EB9); // jsr TARGET
    w.extend(l(TARGET));
    w.push(0x4AC8); // halt
    let bytes: Vec<u8> = w.iter().flat_map(|x| x.to_be_bytes()).collect();
    c.bus.poke_bytes(CODE, &bytes);
    c.bus.poke_bytes(TARGET, &[0x70, 0x01, 0x4E, 0x75]); // moveq #1,d0; rts
    c.vbr = 0x4000_0000;
    c.sr = 0x2700;
    c.a[7] = 0x4080_0000;
    c.pc = CODE;
    let stop = c.run(c.icount() + 100_000);
    assert_eq!(stop, Stop::Stopped, "the program reaches its halt");
    (c.d[0], c)
}

fn stats(c: &Cpu) -> dtemu::cache::Stats {
    c.bus.cache.as_ref().unwrap().stats.clone()
}

#[test]
fn without_the_model_new_code_runs() {
    assert_eq!(run(false, true, Flush::None).0, 2);
}

#[test]
fn primed_no_flush_runs_old_code() {
    let (d0, c) = run(true, true, Flush::None);
    assert_eq!(d0, 1, "the I-cache still holds the old routine");
    // Memory never got the new bytes either: they sit dirty in the D-cache.
    assert!(stats(&c).stale_nopush > 0);
    assert_eq!(c.bus.peek32(TARGET), 0x7002_4E75, "a peek sees the D-cache");
}

#[test]
fn cold_no_flush_fetches_stale_memory() {
    // Never run before, so the I-cache misses -- and fills from memory,
    // which still has the old routine (the new one is unpushed).
    let (d0, c) = run(true, false, Flush::None);
    assert_eq!(d0, 1);
    let s = stats(&c);
    assert!(s.stale_nopush > 0 && s.stale_icache == 0);
}

#[test]
fn push_without_invalidate_runs_old_code() {
    let (d0, c) = run(true, true, Flush::PushOnly);
    assert_eq!(d0, 1, "memory has the new routine, the I-cache the old one");
    let s = stats(&c);
    assert!(s.stale_icache > 0 && s.stale_nopush == 0);
}

#[test]
fn push_and_invalidate_runs_new_code() {
    let (d0, c) = run(true, true, Flush::PushInvalidate);
    assert_eq!(d0, 2);
    let s = stats(&c);
    assert_eq!((s.stale_icache, s.stale_nopush), (0, 0));
    assert!(s.cpushl_d >= 1024 && s.cpushl_i >= 1024);
}

#[test]
fn push_then_icinva_runs_new_code() {
    let (d0, c) = run(true, true, Flush::PushThenIcinva);
    assert_eq!(d0, 2);
    let s = stats(&c);
    assert_eq!((s.stale_icache, s.stale_nopush), (0, 0));
}

#[test]
fn dma_sees_memory_not_the_d_cache() {
    let mut b = Bus::new(128);
    let mut k = Caches::new(16, false);
    k.set_cacr(0xa50c_e100);
    k.set_acr(0, 0x4007_e020);
    b.cache = Some(Box::new(k));
    b.write32(0x4030_0000, 0x1234_5678); // copyback: stays in the D-cache
    assert_eq!(b.read32(0x4830_0000), 0, "the uncached alias reads memory");
    b.in_dma = true;
    assert_eq!(b.read32(0x4030_0000), 0, "eDMA reads memory");
    b.in_dma = false;
    assert_eq!(b.read32(0x4030_0000), 0x1234_5678, "the CPU reads its cache");
}
