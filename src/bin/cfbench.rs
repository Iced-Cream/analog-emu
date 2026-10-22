//! Run compiled ColdFire code on the CPU core alone and count its instructions:
//! for sizing code before it goes into firmware (a mod's DSP, for instance).
//!
//!     cfbench IMAGE.bin LOAD  STEP...
//!
//! IMAGE.bin is loaded at LOAD (hex). Each STEP is either a call,
//! `ADDR[:ARG,ARG...]` (hex; 32-bit arguments pushed as the C ABI does), which
//! runs until the function returns and prints its instruction count, or
//! `dump:ADDR:N`, which prints N big-endian 16-bit samples at ADDR and their
//! peak, `dump32:ADDR:N` (32-bit samples, top 16 bits shown) or
//! `poke:ADDR:HEXBYTES`. Supervisor mode, interrupts masked, nothing else running.

use dtemu::bus::Bus;
use dtemu::cpu::{Cpu, Stop};

const RET: u32 = 0x4000_0100;
const STACK: u32 = 0x407F_0000;

fn hex(s: &str) -> u32 {
    u32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or_else(|_| panic!("not hex: {s}"))
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 2 {
        eprintln!("usage: cfbench IMAGE.bin LOAD STEP...   (STEP: ADDR[:ARG,...] or dump:ADDR:N)");
        std::process::exit(2);
    }
    let image = std::fs::read(&a[0]).unwrap_or_else(|e| panic!("{}: {e}", a[0]));
    let load = hex(&a[1]);
    let mut cpu = Cpu::new(Bus::new(128));
    cpu.bus.poke_bytes(load, &image);
    cpu.vbr = 0x4000_0000;
    cpu.sr = 0x2700;
    cpu.set_hook(RET, true);
    for step in &a[2..] {
        if let Some(rest) = step.strip_prefix("poke:") {
            // poke:ADDR:HEXBYTES
            let (addr, hexs) = rest.split_once(':').unwrap();
            let bytes: Vec<u8> = (0..hexs.len()).step_by(2).map(|i| u8::from_str_radix(&hexs[i..i + 2], 16).unwrap()).collect();
            cpu.bus.poke_bytes(hex(addr), &bytes);
            continue;
        }
        if let Some(rest) = step.strip_prefix("dump32:") {
            // dump32:ADDR:N - N big-endian 32-bit samples, shown as their top 16 bits
            let (addr, n) = rest.split_once(':').unwrap();
            let (addr, n) = (hex(addr), n.parse::<u32>().unwrap());
            let s: Vec<i32> = (0..n).map(|i| cpu.bus.peek32(addr + 4 * i) as i32 >> 16).collect();
            let peak = s.iter().map(|v| v.unsigned_abs()).max().unwrap_or(0);
            let shown: Vec<String> = s.iter().map(|v| v.to_string()).collect();
            println!("dump32 {addr:08x} x{n}: peak {peak}; {} ...", shown.join(" "));
            continue;
        }
        if let Some(rest) = step.strip_prefix("dump:") {
            let (addr, n) = rest.split_once(':').unwrap();
            let (addr, n) = (hex(addr), n.parse::<u32>().unwrap());
            let s: Vec<i16> = (0..n).map(|i| cpu.bus.peek16(addr + 2 * i) as i16).collect();
            let peak = s.iter().map(|v| v.unsigned_abs()).max().unwrap_or(0);
            let shown: Vec<String> = s.iter().take(16).map(|v| v.to_string()).collect();
            println!("dump {addr:08x} x{n}: peak {peak}; {} ...", shown.join(" "));
            continue;
        }
        let (addr, args) = match step.split_once(':') {
            Some((f, args)) => (hex(f), args.split(',').map(hex).collect::<Vec<_>>()),
            None => (hex(step), Vec::new()),
        };
        let mut sp = STACK;
        for &v in args.iter().rev() {
            sp -= 4;
            cpu.bus.poke32(sp, v);
        }
        sp -= 4;
        cpu.bus.poke32(sp, RET);
        cpu.a[7] = sp;
        cpu.pc = addr;
        let start = cpu.icount();
        loop {
            match cpu.run(cpu.icount() + 4_000_000_000) {
                Stop::Hook(pc) if pc == RET => break,
                Stop::Hook(pc) => {
                    cpu.skip_hook = Some(pc);
                }
                Stop::Budget | Stop::Event | Stop::Reset => {
                    eprintln!("call {addr:08x}: did not return");
                    std::process::exit(1);
                }
                Stop::Stopped => {
                    eprintln!("call {addr:08x}: the core stopped");
                    std::process::exit(1);
                }
                Stop::Fault(v, pc) => {
                    eprintln!("call {addr:08x}: exception vector {v} at {pc:08x}");
                    std::process::exit(1);
                }
            }
        }
        println!("call {addr:08x}({}) -> d0 {:08x}: {} instructions", args.iter().map(|v| format!("{v:x}")).collect::<Vec<_>>().join(","), cpu.d[0], cpu.icount() - start);
    }
}
