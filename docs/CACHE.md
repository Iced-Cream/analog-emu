# Cache model (`DTEMU_CACHE`)

By default rytm-emu has no CPU caches: a store is visible to the next
instruction fetch, and `cpushl` does nothing. Code that writes code and runs
it without cache maintenance therefore works here and fails on the device. With
`DTEMU_CACHE=1` the emulator models the ColdFire V4 I- and D-caches, so such a
mistake shows up here too. Source: `src/cache.rs`. Tests: `tests/cache.rs`.

```
DTEMU_CACHE=1   ./target/release/dtboot FW.syx ...   # model on; summary line `cache: ...` at the end
DTEMU_CACHE=log ./target/release/dtboot FW.syx ...   # also prints each stale fetch as it happens (the first 64)
DTEMU_CACHE_KB=32 ...                                 # cache size per cache (default 16)
```

It runs on the interpreter only. Setting it turns the JIT and the predecoded
block cache off. Every instruction is fetched through the I-cache. The default
mode does not change: the JIT, `jitcheck` and the block cache are not touched.

## What is modelled

- **D-cache:** 4 ways, 16-byte lines. Each data address is cacheable as set by
  ACR0/ACR1, or by CACR[DDCM] when neither matches.
  - Copyback: reads and writes allocate a line, and a write marks it dirty. A
    dirty line reaches memory only when it is evicted (round-robin per set,
    invalid ways first) or pushed by `cpushl dc`/`bc`.
  - Writethrough: memory is written, and a line that hits is updated.
  - Inhibited: the access goes to memory.
  - The stock OS sets CACR `0xa50ce100` and ACR0 `0x4007e020`. So
    0x40000000-0x47FFFFFF is copyback, and 0x48000000+ (the same DDR) is an
    uncached data alias.
- **I-cache:** 4 ways, 16-byte lines. Lines fill from **memory**, never from
  the D-cache. Instruction addresses are cacheable as set by ACR2/ACR3, or
  by CACR[IDCM] when neither matches. The stock OS leaves IDCM at 0, so all
  of DDR is cacheable for instructions. A line stays until something
  invalidates it: CACR[ICINVA], `cpushl ic`/`bc`, or eviction.
- **CPUSHL (An)** selects the line by **set and way, not by address**:
  set = An[11:4] (256 sets at 16 KB), way = An[1:0].
  - `dc` writes the line back if it is dirty, then invalidates it (unless
    CACR[DPI] is set).
  - `ic` invalidates the line (unless CACR[IDPI] is set).
  - `bc` does both.
  - `intouch (An)` loads the line holding An into the I-cache.
- **CACR writes:** DCINVA invalidates the whole D-cache *without* pushing.
  Dirty lines dropped this way are counted as `dirty-lost`. ICINVA
  invalidates the whole I-cache.
- **Who sees what:**
  - The CPU's data accesses (`Bus::read*`/`write*`) go through the D-cache,
    and its instruction fetches (`Bus::ifetch*`) go through the I-cache.
  - eDMA (`Bus::in_dma`) reads and writes memory directly, past both caches,
    as on the device.
  - `peek`/`poke` stay coherent: a peek sees a cached line, and a poke
    updates memory and any line holding that address. These are used by the
    debugger, boot loading and the machine's high-level stand-ins.
  - Peripheral models that read guest memory through `Bus::read*` (not
    eDMA) see the CPU's cached view.
  - Only DDR is cached. SRAM and I/O never are.
- **Stale-fetch detection:** each fetched byte is compared with two things:
  - Memory. If they differ, it is counted as `stale I-cache`: memory changed
    after the line was filled, and nothing invalidated the line.
  - A dirty D-cache line for the same address. If memory differs from that
    line, it is counted as `stale no-push`: the CPU wrote the code, but it was
    never pushed to memory.

  Each kind is listed per line at the end of the run (`cache: stale ...` with
  the first PC). `DTEMU_CACHE=log` also prints each one as it happens.
- **Snapshots** hold memory only. Loading one empties the caches. Saving in
  cache mode does not include dirty lines.

**Not modelled:**
- the ACR S field (user/supervisor match)
- the store buffer
- half-locking
- the branch cache (a stale prediction on the V4 is corrected by the pipeline,
  so it is not a correctness issue)
- cache timing
- the exact V4 replacement policy [inferred]

The size is not verified. The 16 KB default (with 4 ways and 16-byte lines)
matches the brief and the drive-code-loader spec. It has not been checked
against the MCF5441x reference manual [inferred]. The V4 core is built with
caches of 0-32 KB each.

## Results (stock OS 1.73, 2026-10-16)

- **Cold boot** of `Analog-Rytm_MKII_OS1.73.syx` on a copy of `rytm.img`, 4.5G
  instructions, PLAY at 3G, with `DTEMU_CACHE=log`:
  - It executes exactly the same instruction count as the interpreter
    (2,516,316,142).
  - The end screen PNG and the main-mix WAV are byte-identical.
- **Play** from a 4G snapshot: NO x3, PLAY, BD and SD pads, 2G instructions.
  - The screen PNG and the 16-channel codec WAV are byte-identical to
    `DTEMU_NO_JIT=1`.
  - The sequencer is running (▶) and the audio is not silent.
- **Cache activity:**
  - D-cache: about 99.2% hits, 4.6M writebacks over the boot.
  - I-cache: about 99.6% hits.
  - The OS's 27 `cpushl dc` and its CACR invalidates at boot: no dirty line
    is lost.
  - **0 stale fetches.**

  The OS does its DMA through the uncached 0x48000000 alias. Its own
  `cpushl dc,(a0)` calls pass a data address as An, which on this core
  selects a set and way, not that address's line. They are harmless.
- **Speed:** about 55-62 MIPS executed (50% of real time at 200 MIPS
  emulated). For comparison, the interpreter with blocks
  (`DTEMU_NO_JIT=1`) runs at about 250-290 MIPS, and the interpreter
  without blocks at 113 MIPS. The default (JIT) mode is unchanged. Its only
  addition is one predictable branch on `Bus::read*`/`write*`, which the
  JIT's direct page accesses skip.

## For a code loader (e.g. the +Drive code loader mod)

After copying code into cached DDR, and before jumping to it, push the
D-cache and invalidate the I-cache over every set and way, with interrupts
masked:

```
        moveq   #0,d2            ; way
wl:     movea.l d2,a0
        move.l  #512,d1          ; 512 sets: also covers a 32 KB cache (a 16 KB one ignores An[12])
sl:     cpushl  bc,(a0)
        lea     16(a0),a0
        subq.l  #1,d1
        bne.s   sl
        addq.l  #1,d2
        cmpi.l  #4,d2
        bne.s   wl
```

Pushing all of `dc` and then writing CACR with ICINVA (keeping DCINVA clear)
also works. Then run the build under `DTEMU_CACHE=log` and check:

- no `cache: pc ... stale` lines
- `stale fetches: I-cache 0 no-push 0` in the summary
- the loaded code behaves as it does without the model

A missing push shows as `no-push`: the old memory bytes run. A missing
invalidate shows as `I-cache`: the old line runs. A `cpushl` that passes an
address instead of a set/way index shows as one of the two, whenever the
loaded code's lines are not the ones it happened to name. Run long enough
for the loader to execute. A loader that runs at boot needs a cold boot under
the model. For one triggered later, a snapshot made without the model is a
quicker start: load it with `DTEMU_CACHE=1`, and the caches start empty.
