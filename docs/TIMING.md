# Unit timing

`DTEMU_TIMING=unit` makes the emulator's clock count ColdFire core cycles instead of instructions, so the
CPU load the firmware measures (analoghealth's CPU % and DSP %, the UI tick rate) follows the unit's. Without
it nothing changes: one tick per instruction at 200 M a second.

| variable | values |
|---|---|
| `DTEMU_TIMING` | `unit` (sampled, about real time) or `exact` (every instruction measured, about 3x slower) |
| `DTEMU_TIMING_PARAMS` | `key=value,...` overrides of the parameters below; `emmc_*` keys belong to the eMMC model |
| `DTEMU_TIMING_FN` | a function address (hex): its calls and average cycles, callees included, in the summary |

dtboot prints `busy=X%` on each report line (1 minus the idle time skipped in that window) and a summary at
the end: cycles by cause, cache hit rates and the busiest peripheral pages. The JIT is off under timing.

## Model and sources

All instruction costs are from the MCF54418 Reference Manual (NXP MCF54418RM, Rev. 8), section 3.3.5,
tables 3-13 to 3-20. src/timing.rs lists them with the table each comes from.

- Clock: 264 MHz, twice the 132 MHz bus clock the PIT rates establish (the MCF5441x runs its bus at fsys/2).
- Instructions: table costs; the load-use stall of 3.3.5.1 (2 or 3 cycles); EMAC accumulator stalls.
- Branches: 8-entry branch cache plus 128-entry prediction table with 2-bit counters (3.1.1.1), mispredict 8;
  RTS through the 4-entry return stack.
- Caches: tag-only 8 KB 4-way I and D (section 6.2), following CACR and ACR0-3, CPUSHL and INTOUCH.
- Memory: the manual assumes zero-wait memory, so DDR fills, write-backs, uncached accesses and peripheral
  accesses cost the fitted numbers below.
- Exceptions and interrupts: 17 extra (TRAP's 18).

Not modelled: MOVE folding (`fold` is a fitted fraction, 0 by default), IFP/OEP decoupling, DDR banks and
refresh, eDMA bus contention. Cycle exactness is not the aim; the load within a few points is.

## Parameters

| key | default | meaning |
|---|---|---|
| `hz` | 264e6 | core clock |
| `exc` | 17 | exception or interrupt entry |
| `mispredict` | 8 | branch misprediction (manual) |
| `dmiss` | 42 | D-cache line fill from DDR (fitted) |
| `wb` | 4 | dirty line push, mostly hidden by the push buffer |
| `imiss`, `imiss_seq` | 42, 42 | I-cache line fill after a change of flow / of the next line (fitted) |
| `unc_rd`, `unc_wr` | 38, 8 | DDR through the uncached alias (fitted / estimated) |
| `sram`, `io` | 0, 10 | internal SRAM, peripheral register |
| `fold` | 0 | share of MOVEs issued free with the instruction before |
| `window_us`, `period_us` | 2000, 20000 | sampled mode: measure this long of each period (gaps jittered 0.5-1.5x) |
| `warm` | 0.5 | share of each window spent re-warming the cache tags before learning |

## Sampled mode

Between windows the cache tags and branch tables are not updated, and each predecoded block is charged its
learned cost (a running average of its measured runs; blocks not seen in a window yet get the average of
first runs, which run cold). Windows re-warm the tags for `warm` of their length before learning.

On AH10 (stock 1.74 + analoghealth, boot, three NO taps, 8-12 G cycles) sampled mode reads about 1.5 points
below exact mode (busy 73.5 vs 75.1 %, analoghealth CPU 72.5 vs 73.9 %). It runs a 12 G cycle boot in about
50 s with four runs sharing the machine (45 s of emulated time).

## Calibration so far (2026-11-11)

Unit figures are analoghealth stats from the hardware test log (T138 AH10, T175 builds). Emulator figures are
analoghealth's own stats read over USB MIDI in dtboot (`--usb-midi` with the stats request,
`DTBOOT_USB_OUT`), exact mode unless noted, bare boot with no project and no Sidecar traffic.

| build | unit CPU % | emulator, exact |
|---|---|---|
| AH10 (stock + analoghealth) | 75.3-75.6 (DSP 70.8) | analoghealth 73.9 (DSP 68.8), busy 75.1 |
| CTL1 (stock + core 0.10 + analoghealth) | 81 | busy 75.1 |
| C11F, C11N, CH01, CH03 (core 0.11 variants) | 85-90 | busy 75.3-75.4 |
| L10L (core 0.10 + lock-check) | 90 | busy 88.4 |
| V11A, V11F, V11P (0.11 + lock-check) | 95-100 | busy 88.4-88.8 |

On V11P, analoghealth's own CPU figure (sampled, about 77 %) sits well under busy (82 % sampled): with
lock-check, time outside the idle task's loop counts as busy to one and not the other. Which matches the
unit's reading better is still open.

Lock-check's cost shows in the emulator (+13 points against about +9 on the unit). Core 0.10 and 0.11's costs
(about +5 and +9 on the unit) do not show on a bare boot: they come from hooks driven by the unit's
conditions at the time (Sidecar USB traffic, a loaded project), not from the CPU model. The next calibration
step is running the same elkbench plans on the emulator with timing on, so both sides do the same work.
