# dspbench: digital engines for the Rytm, sized on the emulator's CPU core

Can the Rytm's ColdFire (MCF54418: 250 MHz, EMAC, **no FPU**) run extra digital
voices, Machinedrum/Monomachine-style, next to its own engine? This builds
Mutable Instruments' **Braids** (MIT, Emilie Gillet; fixed-point, written for a
chip without an FPU, unlike its float-based successor Plaits) for that CPU and
counts instructions per sample with `cfbench`.

    tools/dspbench/build.sh            # fetches eurorack+stmlib at pinned commits into vendor/
    cfbench tools/dspbench/build/bench.bin 40400000 40400000:19 404001de:64 dump:<out>:1536

`bench_init(shape)` sets Braids' macro oscillator up (middle C, both parameters at
half), `bench_render(blocks)` renders 24-sample blocks. Toolchain: Homebrew
`m68k-elf-gcc` 16.2, `-mcpu=54418 -O2`, freestanding; `shim/` supplies the few
C++ and C header pieces Braids needs (no libstdc++ or newlib in the toolchain),
`libc.c` memset/memcpy.

## Results (2026-10-06, Braids unmodified, -O2, no EMAC)

Instructions per output sample, 2400 samples each, all with real output:

| engine | instr/sample | | engine | instr/sample |
|---|---|---|---|---|
| FM | 58 | | KICK | 128 |
| FEEDBACK_FM | 68 | | SAW_SWARM (supersaw) | 142 |
| CSAW | 69 | | SNARE | 147 |
| BOWED | 85 | | STRUCK_DRUM | 162 |
| VOSIM | 103 | | CYMBAL | 214 |
| WAVETABLES | 116 | | HARMONICS | 217 |

Headroom, measured on stock OS 1.73 in the emulator: the firmware executes about
51% of the instruction budget idle and 56% with all twelve tracks hit 16 times a
second and sends up, so about 44% is spare: ~88 M instructions a second, ~1,800
per sample at the emulator's assumed 200 M instructions a second. Real hardware
may cost more per instruction (DDR, cache), so take half as a margin: roughly 8
to 15 FM voices, or 4 to 6 of the heavier engines, before any EMAC tuning.

Size: all of Braids is 99.6 KB of code and tables, 0.9 KB data, 20 KB working
memory. A mod would take only the engines it uses; where that memory lives on the
Rytm (free DDR shown untouched at runtime, as rytm-mods requires for its pools) is
the open design question, along with the hook that renders into a voice's sample
layer (DAC word v), so the engine passes through the voice's analog filter,
overdrive and amp.
