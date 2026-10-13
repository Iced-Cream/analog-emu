# Porting to the Analog Rytm MKI and Analog Four MKI (survey)

2026-10-12. Analog Rytm MKI OS 1.74 and Analog Four MKI / Analog Keys OS 1.56,
compared against Analog Rytm MKII OS 1.74, Analog Four MKII OS 1.56 and Digitakt
OS 1.53. Everything here is static analysis (disassembly with `tools/cfdis.py`,
strings, `tools/xmatch.py`, a throwaway ELE2 unpacker); sections 9 and 10 record the first boots in the
emulator.
Each claim is tagged **[measured]** (read from the image, the code or a tool's
output), **[inferred]**, or **[community]** (public, not checked against a unit).

## Summary

Both MKI units run on the **same CPU family and the same OS framework** as the MKII
units: ColdFire V4e with EMAC (MCF5441x), the same RTOS, the same middleware and
nearly the same voice-control frames. `xmatch.py` ports the RTOS, MIDI, SD-driver
and framework addresses with no hand work. What differs is the board around the CPU:

| | MKII (both) | Rytm MKI | A4 MKI |
|---|---|---|---|
| OS file | ELE3 container, 5 sections | **ELE2**, one section (MAIN OS) | **ELE2**, one section |
| DDR (ACR0) | 128 MB | 128 MB (`0x4007e020`) | **64 MB** (`0x4003e020`) |
| Boot / config flash | SPI NOR on DSPI0 | **parallel NOR, FlexBus CS0, 8 MB at 0** | the same |
| Persistent state | MRAM | **2 MB device on FlexBus CS1 at `0x20000000`** | the same [inferred] |
| Voice SPI | DSPI1 (`0xFC03C000`), eDMA ch 15 | **DSPI2 (`0xEC038000`), eDMA ch 29** | the same as the Rytm MKI |
| Voice frame | Rytm 492 w / A4 100 w | 492 words, same format | 100 words, same format |
| Codec SSI | SSI1, Rytm 16 / A4 8 words | **SSI0 + SSI1**, 8 words each | SSI1, 8 words |
| Display | 128x64 OLED, 8 pages | **122x32 LCD, 4 pages** | **122x32 LCD, 4 pages** |
| LEDs | RGB, palette | **1 bit each, 12 groups of 8** | **1 bit each, 14 groups of 8** |
| USB | high speed capable | **full speed only** (PORTSC PTS=11) | full speed only |
| UI card check | yes ("WRONG UI CARD") | none | none |

The display stays behind the same panel MCU on UART8 and uses the same tile command
(`1p cc d0..d7`), with 4 pages instead of 8. So the emulator's panel model needs a
second geometry, not a new protocol.

## 1. Container [measured]

`fwinfo` refused both files before ELE2 support ("container: no ELE3 magic"; it
reads them now, section 9). The outer layers are
unchanged: SysEx messages with 126-byte bodies (8-in-7 encoded), each checksum ok,
and the same decoded preamble `[u32 length][u32 checksum]`. The container is new.

- SysEx device byte: **0x07** (Rytm MKI) and **0x06** (A4 MKI / Analog Keys). The
  MKII files use 0x0C and 0x0B.
- Decoded layout, from offset 0 of the decoded stream:

| offset | Rytm MKI | A4 MKI | meaning |
|---|---|---|---|
| +0x00 | `0x0013a258` | `0x000def18` | container length |
| +0x04 | `0x426ad21b` | `0xab90ea4f` | checksum (algorithm not checked) |
| +0x08 | `ELE2` | `ELE2` | magic |
| +0x0C | `0176` | `0204` | build |
| +0x18 | `1.74` | `1.56` | version |
| +0x1C | `0x40000400` | `0x40000400` | MAIN OS load address |
| +0x20 | 1286709 | 913144 | packed length |
| +0x24 | `0x0879d23a` | `0x05c99453` | byte sum of the packed stream (both ok) |
| +0x28 | | | the packed stream |

- There is **no section table**. The file holds only MAIN OS: no bootstrap section,
  no FPGA bitstream, no panel-MCU section.
- The packed stream is the same aPLib-style LZ as the MKII (`firmware::depack`). It
  runs to the end of the declared length **without the usual end marker**: the
  depacker meets the end of input inside a gamma code. The output is 2824228 B
  (Rytm) and 2372584 B (A4). The last non-zero byte is at offset `0x2acf04` and
  `0x23e74c`, so the true image length may be shorter. Treat running out of input
  as the end for ELE2.
- Depacked sha256 (as above, full length): Rytm MKI `6d021e22e8912cfc...a5ab811d`,
  A4 MKI `cc19347b11d220ab...682c714c26`.
- MAIN OS word 0 is the entry point, as on the MKII: Rytm MKI **`0x40000592`**,
  A4 MKI **`0x40000e52`**.
- **The bootstrap is embedded in MAIN OS.** Each image carries a bootstrap blob
  linked for flash (it calls absolute addresses such as `0x3262`). The Rytm MKI's is
  at `0x4028c770..0x402a1b84` (87060 B). Its version halfword, `0x0602`, is at
  `0x4028c768`. The A4 MKI's version halfword is `0x0601` (`0x40226ee8`).
  - The blob holds the DDR controller init (`0xFC0B8000`), the FlexBus chip
    selects, UART bring-up, the startup menu ("1 ... TEST MODE", "4 ... OS
    UPGRADE"), "DRAM:(%c)", "FPGA:(%c)" and "UI FIRMWARE: 1.%d".
  - The entry code compares the halfword at flash `0x1fff8` (Rytm) or `0xfff8` (A4)
    with that version. When the flash copy is older, it calls "BOOTSTRAP UPGRADE"
    (`0x4011a7a4` on the Rytm).

## 2. CPU and peripherals

**The CPU family is the same MCF5441x ColdFire V4e [measured].**

- **Entry** (Rytm `0x40000592`, A4 `0x40000e52`):
  1. saves its caller's first argument (a boot-flags word) to `0x40236d64` /
     `0x401d8ea4`;
  2. sets SP to `0x48000000` (Rytm) or **`0x44000000`** (A4);
  3. checks the bootstrap version;
  4. writes PPMCR (`0xFC04002D`) to enable clocks, as the MKII does.
- **CACR and ACR0:** CACR is `0xa50ce100`, as on the MKII. ACR0 is `0x4007e020`
  (Rytm, 128 MB) and `0x4003e020` (A4, 64 MB). ACR1 `0x2001e420` briefly maps
  `0x20000000` while the OS clears the CS1 device (`0x40000706`).
- **Context switch** at `0x40000410` in both, the same code as the MKII.
- **EMAC:** used throughout; the RTOS and audio code match the MKII under `xmatch`.

**Peripheral references** (32-bit operands per 16 KB block) **[measured]**:

| block | Rytm MKI | Rytm MKII | A4 MKI | A4 MKII | DT |
|---|---|---|---|---|---|
| DSPI0 `0xFC05C000` (MKII SPI flash) | **0** | 312 | **0** | 312 | 315 |
| DSPI1 `0xFC03C000` | 44 | 62 | 44 | 31 | 48 |
| DSPI2 `0xEC038000` | **33** | 1 | **30** | 0 | 0 |
| eDMA | 325 | 263 | 270 | 251 | 240 |
| INTC0/1/2 | 15/51/27 | 17/45/31 | 15/48/26 | 17/39/30 | 18/38/32 |
| DTIM0..3, PIT0..3 | similar | | similar | | |
| USB OTG `0xFC0B0000` | 204 | 149 | 199 | 147 | 147 |
| DDR ctrl `0xFC0B8000` | 46 (blob) | 0 | 46 (blob) | 0 | 0 |
| SSI0 `0xFC0BC000` | **55** | 1 | 0 | 0 | 0 |
| SSI1 `0xFC0C8000` | 54 | 29 | 37 | 12 | 23 |
| eSDHC `0xFC0CC000` | 99 | 131 | 90 | 128 | 128 |
| UART8 / UART9 | 23 / 55 | 21 / 22 | 27 / 53 | 26 / 21 | 26 / 22 |
| CCM `0xEC090000` | 53 | 15 | 51 | 15 | 16 |
| GPIO `0xEC094000` | 243 | 236 | 210 | 209 | 219 |

- **Flash is parallel NOR on FlexBus [measured].**
  - The blob sets CS0 to CSAR=0, CSMR=`0x007f0001` (8 MB), CSCR=`0x102da0`; and CS1
    to CSAR=`0x20000000`, CSMR=`0x001f0001` (2 MB), CSCR=`0x1011a0`
    (`0x4028cc8e`).
  - The OS programs flash with the AMD/JEDEC command set: `0xF0F0` reset, then
    `0xAAAA` / `0x5555` written to `0xAAAA` / `0x5554` (16-bit words), then
    `0x8080` erase (A4 `0x4007594c`). It reads flash as plain memory (A4
    `0x400ebd26`).
  - The A4 MKII and Rytm MKII read SPI flash through DSPI0 instead.
- **The CS1 device at `0x20000000`** holds boot flags and a counter that are read at
  power-up (+0x08, +0x14; `0x40000666`). The factory-reset path clears it from
  `0x20000110` to `0x20200000`. It is probably battery-backed SRAM or MRAM, the MKI
  equivalent of the MKII's MRAM **[inferred]**.
- **DIN MIDI on UART9** **[inferred]**: the blob sets UBG2 = `0x78`, which gives
  31250 baud with a 120 MHz bus. `sysex_send` and the MIDI senders match the MKII by
  `xmatch`.
- **Panel MCU on UART8:** UBG2 = `0x18`, about 156 kbaud **[inferred]**.
- **DTIM2:** MAIN OS sets DTMR = `0x1d` itself (internal clock / 16;
  `0x400f343a`). **No DTIN dependence**, unlike the MKII's bootstrap-set
  DTMR = 7 **[measured]**.

**Public facts:**

- An Analog Four teardown (forum, 2012, so an MKI) reads the CPU as an
  **MCF54415**, 250 MHz, with a Spartan-3A FPGA probably on the analog side, Micron
  SDRAM and Toshiba flash **[community]**. That agrees with the 5441x peripheral
  map above. The emulator models the MCF54418, a superset.
- Retail specs give the Rytm MKI a **backlit 122x32 LCD** **[community]**. That
  matches the firmware (section 4).

## 3. Voices and codec [measured]

**Voice control frame:** the same format as the MKII. It is a double buffer in SRAM:

- the marker `0x?000AAAA`;
- N words `0x?001wwww` (DSPI PUSHR);
- the end marker `0x?8005555` with EOQ set;

sent by eDMA through an SPI. The MKI uses **DSPI2 at `0xEC038000`** with **eDMA
channel 29** (TCD `0xFC0453A0`), not DSPI1 / channel 15.

| | builder | sender | words | source | buffers |
|---|---|---|---|---|---|
| Rytm MKI | `0x40078cac` | `0x40078d3a` | **492** (`0x1ec`) | `0x80004ce0` | `0x80003400` + `0x810*k`, CTAR0 `0x8001` |
| A4 MKI | `0x40061b08` | `0x40061b92` | **100** (`0x64`) | `0x800032b0` | `0x80001800` + 496 k, CTAR1 `0x9001` |

Compared with the MKII:

- Rytm MKII: 492 words from `0x800063c0`.
- A4 MKII: 100 words from `0x80003d6c`.

The frame sizes and word formats are equal, so `analog.rs` and `a4voice.rs` can read
the MKI frames once the source address and SPI block are parameterised **[inferred:
same word layout; not checked word by word]**.

The DSPI2 status read at `0xEC03802C` (TCF, bit 28) is already forced in
`Io::new()`. That value came from upstream's harness.

**Codec SSI:** the Rytm MKI has two SSIs, the A4 MKI has one.

| | SSI | STCCR (`|=`) | frame | other |
|---|---|---|---|---|
| Rytm MKI | SSI0 | `0x40000 | 0x16000 | 0x700` (`0x40006732`) | **8 words of 24 bits** | STCR `0xe9`; TX via eDMA ch 48 from `0x80002000` |
| Rytm MKI | SSI1 | the same | **8 words of 24 bits** | SRCR `0x489` |
| A4 MKI | SSI1 | the same | **8 words** | as the A4 MKII |

The Rytm MKII's SSI1 has 16 words (DC field `0xf00`). The Rytm MKI therefore splits
its 16 channels across two 8-slot SSIs; section 10 confirms it from the TCDs (SSI0 carries
DAC words 0..7, SSI1 8..15), and the emulator models both on the Rytm MKI.

**Calibration [measured]:** the `RBOW` format is unchanged (magic, size, the XOR-sum
checksum); only the address and the flash type change.

| | record | where | code |
|---|---|---|---|
| Rytm MKI | 110706 B (`0x1b072`, the MKII's size) | flash `0x780000` and `0x7c0000` | `0x400f3e40` |
| A4 MKI | 3670 B (`0xe56`) | flash `0x7f0000` | `0x400ebcde` |

The MKII records are at `0x300000` and `0x340000`.

## 4. Display, panel, keys, LEDs

**Display [measured]:** the ColdFire does not drive the LCD itself. It sends tiles
to the panel MCU over UART8 with the MKII command:

- `0x10|page`, a column, then 8 data bytes;
- through the UART8 ring (Rytm `0x40001a0e` per byte; lock `0x40001a52`).

| | flush | full redraw | clear | front / back fb |
|---|---|---|---|---|
| A4 MKI | `0x4007333a` | `0x400732c8` | `0x400733d2` | `0x401ed954` / `0x401ed958` |
| Rytm MKI | `0x40093bb6` | `0x40093b44` | `0x40093c4e` | `0x4024bbb4` / `0x4024bbb8` (xmatch 7/5 votes) |

- The framebuffer is **4 pages x 128 columns, byte = page + 4 x column**. The MKII
  uses 8 pages, page + 8 x column.
- The clear routine clears **488 B = 122 x 4**.
- Centred text uses `(0x7a - width) / 2`.

So the panel is **122x32 pixels, 1 bit, 4 pages of 8 rows**. The tile loop still
walks 16 tiles of 8 columns, and columns 122..127 are off-glass **[inferred]**.

**The LCD controller chip is not visible from the ColdFire side.** 122x32 is the
usual geometry of SED1520-class dual-controller modules (2 x 61 columns)
**[inferred]**. The "CONTRAST" setting presumably maps to that controller.

- **Panel MCU firmware:**
  - The A4 MKI OS bundles a **5444 B panel-MCU image** at `0x401b8a92`. It is
    flashed only when the boot flags ask for it ("UI UPGRADE", `0x40100f7c`: send
    `0xff80`, `0x42`, magic `0xb00fb00f`, then the words). Its first opcodes,
    `2f0e 2f07 4238 8060` (`clr.b $8060.w`, a V1 peripheral address), suggest a
    ColdFire V1 MCU **[inferred]**. Reading that image would identify the LCD
    controller.
  - The Rytm MKI OS bundles **no** panel image **[measured: no `[len][ver]` + code
    header; no "UI UPGRADE" string]**.
  - Neither MKI OS has the MKII's "WRONG UI CARD" check.
- **LEDs [measured]:** every LED is a single bit.
  - `led_group(g, v)` sends `0x20|g, v` for g < 16. For g >= 16 it sends `0x30|g`
    (Rytm) or `0xA0|g` (A4).
  - The flush (Rytm `0x40093d60`, A4 `0x400734e8`) sends `state XOR blink` per
    changed group.
  - **12 groups on the Rytm MKI (96 LEDs), 14 on the A4 MKI (112 LEDs).**
  - There is no palette or RGB slot command (the MKII's `B4`/`B5`).
  - How the Rytm MKI's pad backlights and level bars are driven (more groups, the
    `0x30` path, or the panel MCU itself) is not traced.
- **Keys and encoders:** the A4 MKII key parser (slot table `0x40212158`) has no
  byte-pattern match in either MKI image. The input side (button-group and encoder
  messages) is presumably the MKII's `0x2g mask` / `0x3e delta` **[inferred]**. The
  raw code to key mapping must be measured after first boot, by pressing codes as
  was done for the A4 MKII (docs/A4-PORT.md section 8).
- **Rytm MKI pads:** "PAD CALIBRATION" and "FAULTY PADS!" strings exist. Velocity
  and pressure presumably come from the panel MCU, as on the MKII **[inferred]**.

**Key and LED names, seeded from front-panel photos [community: photos of real
units; LED-to-group mapping still to be measured]**

Analog Rytm MKI:

| area | keys (main / FUNC function) | LEDs |
|---|---|---|
| top left | FX/Kit, Mute/Sound, Chromatic/Track, Scene/Pattern, Perf/Song | one above each (5) |
| left column | Track/Global, Function, Retrig/Click | none |
| pads | BD1 SD2 RS3 CP4 / BT5 LT6 MT7 HT8 / CH9 OH10 CY11 CB12 | pad backlight, level bar per pad |
| centre | Tempo/Tap; Yes/Save, No/Reload; Up = Retrig+, Down = Retrig-, Left = uTime-, Right = uTime+ | Tempo LED |
| bank | Bank Group/Direct Change | A-D, E-H LEDs, bank-group LED |
| below bank | Trig Mute, Accent, Swing, Slide | one above each |
| transport | Record/Copy, Play/Clear, Stop/Paste | record LED |
| mode | Song/Edit, Chain/New | Mode LEDs (2) |
| pages | Trig/Setup, Synth/Delay, Sample/Reverb, Filter/Dist, Amp/Comp, LFO/LFO | one above each (6) |
| encoders | A-H (8), Level, Master Volume (pot) | |
| bottom | trig 1-16, Page/Scale | 16 trig LEDs, 1:4 2:4 3:4 4:4 |

Analog Four MKI:

| area | keys | LEDs |
|---|---|---|
| left | Song/Edit, Chain/New, Tempo/Tap, Function, Bank Group/Direct Change | Mode LEDs (2), Tempo LED, bank-group LED, A-D / E-H LEDs |
| below bank | Trig Mute, Accent, Note Slide, Parameter Slide | one above each |
| centre | Yes/Save, No/Reload, Up, Down, Left, Right | |
| transport | Record/Copy, Play/Clear, Stop/Paste | record LED |
| pages | Perf/Setup, Arp/Setup, Note/Setup, Osc 1 (Ext In / CV A), Osc 2 (CV B), Filters (Chorus / CV C), Amp (Delay / CV D), Env (Reverb / Env), LFO (LFO / LFO) | one above each (9) |
| keyboard | white: Kit, Sound, Track, Pattern, Song, Click, Swing, Global; black: 5 unlabelled round keys | one above each; Oct LED column of 5 |
| tracks | Trk 1-4 / Mute, FX / Mute, CV / Mute, Transpose/Lock | one beside each (7) |
| encoders | A-J (10), Level, Master Volume (pot) | |
| bottom | trig 1-16, Page/Scale | 16 trig LEDs, 1:4 2:4 3:4 4:4 |

## 5. Storage and USB

- **+Drive on eSDHC [measured]:** both MKI OSes have the +Drive strings ("FORMAT
  +DRIVE", "+DRIVE SOUNDS") and the eSDHC driver. The SD-driver data block keeps the
  MKII's relative layout (`xmatch` below).
  - The working-state raw slots that the Rytm MKII saves on power-off (card
    `0x7a000` / `0x7c000`) are not checked. On the MKI the CS1 device probably holds
    that role **[inferred]**.
  - The Rytm MKI also has a factory-sample check ("VER. FACTORY SMPLS", "SAMPLE DATA
    CORRUPT").
- **USB [measured]:**
  - PORTSC1 is written as `0xc0000000` (PTS = 11, the serial/full-speed transceiver)
    at Rytm `0x40003e02` / A4 `0x40004a84`. The MKII toggles PFSC on a
    high-speed-capable PHY. So **MKI USB is full speed**.
  - Device descriptor: VID `0x1935`, PID **`0x0007`** (Rytm MKI) and
    **`0x0005` / `0x0003`** (A4 MKI / Analog Keys). There are also alternate PIDs
    `0x0b4d` and `0x0b4c` / `0x0b4b`.
  - The Overbridge interfaces are vendor-class, with isochronous IN endpoint `0x83`
    in alt settings of 6, 202, 300, 398, 594... bytes. There is no USB-audio-class
    interface like the MKII's.
  - `usbdev.rs` assumes MKII descriptors and high-speed iso sizes. It needs a
    full-speed path.

## 6. Profile (`src/symbols.rs`)

Matched by `tools/xmatch.py` from Rytm MKII 1.74 to Rytm MKI 1.74, and from A4 MKII
1.56 to A4 MKI 1.56 **[measured: unique masked code match / data votes]**.

| field | Rytm MKI 1.74 | A4 MKI 1.56 |
|---|---|---|
| `entry` | `0x40000592` (word 0) | `0x40000e52` (word 0) |
| `ctx_switch` | `0x40000410` | `0x40000410` |
| `task_create` | `0x40000c68` | `0x400018e8` |
| `sem_pend` / `pend_b` | `0x40000dba` / `0x40000d46` | `0x40001a3a` / `0x400019c6` |
| `queue_send` / `queue_recv` | `0x40001236` / `0x400012e6` | `0x40001eb6` / `0x40001f66` |
| `sem_post` | `0x40000efc` | `0x40001b7c` |
| `current_tcb` / `prio_heads` | `0x42755268` (16 votes) / `0x42755228` | `0x425d5be4` (16) / `0x425d5ba4` |
| `pend_call` / `depack_copy` | `0x40096d28` / `0x400974d0` | `0x40075c08` / `0x400763b0` |
| `abort_loop` | `0x4009d3aa` | `0x4007b4ca` |
| `panel_diff` | `0x40093bb6` (via the A4 MKI) | `0x4007333a` (by hand) |
| `fb_front` / `fb_back` | `0x4024bbb4` / `0x4024bbb8` | `0x401ed954` / `0x401ed958` |
| `display_sem` / `completion_sem` | `0x40ae8910` / `0x40b08bc4` | `0x4090dd54` / `0x4092e120` |
| `sd_flag`, `sd_capacity`, `sd_status` | `0x40ae82b4`, `0x40ae82d8`, `0x40ae82e4` | `0x4090d6f8`, `0x4090d71c`, `0x4090d728` |
| `sd_dma/data/cmd_sem` | `0x40ae82f0` / `0x40ae82f8` / `0x40ae8300` | `0x4090d734` / `0x4090d73c` / `0x4090d744` |
| `progress_total` / `_done` | `0x40ae8b7c` / `0x40ae8b80` | `0x4090dfc0` / `0x4090dfc4` |
| `mounted` | `0x40a13358` (28 of 40 votes) | not found (as on the A4 MKII) |
| `uart8_ring_ptr` / `_consume_idx` | `0x405fe9d8` / `0x405fe9f8` (weak) | `0x404414d4` / `0x404414f4` (weak) |
| `usb_midi_queue` / `midi_event` | `0x40acaac4` / `0x40acaa9c` | `0x408ef09c` / `0x408ef074` |
| `midi_pool`, `_map`, `_sem` | `0x42757894`, `0x42755408`, `0x42755400` | `0x425d8228`, `0x425d5d9c`, `0x425d5d94` |
| `midi_short_ring` / `_idx` | `0x42755488` (1 vote) / `0x40a15568` | `0x425d5e1c` (1) / `0x40839ca0` |
| `usb_midi_send` / `_short` | `0x4000283c` / `0x40002790` | `0x400034bc` / `0x40003410` |
| `sysex_send` | `0x40083d44` | `0x40066c68` |
| `flash_read` | **none**: NOR is memory-mapped | **none** |
| `usb_audio_copy`, `voice_output_mix`, `ob_in_*` | not found (different USB) | n/a |

The data matches are consistent: the SD block keeps its relative offsets, and the
TCB/prio pair sits `0x40` apart as on the MKII.

## 7. Likely first boot obstacles

1. **ELE2 container**: `Firmware::from_syx_bytes` needs an ELE2 path: one section
   at +0x20, dest from +0x1C, and depack until input runs out. `fwinfo`, the
   profile hash and the `sections/` workflow follow from that.
2. **No bootstrap to imitate from a section.** The entry expects its caller's
   boot-flags argument at 4(SP) and a sane SP. It reads the bootstrap version at
   flash `0x1fff8` / `0xfff8`.
   - With flash reading `0x0000` it would start "BOOTSTRAP UPGRADE" (writes flash).
     The flash model must read erased (`0xFFFF`) or carry the version.
   - Boot flags = 0 is the first guess **[inferred]**; bit 20 plus a model byte
     triggers the A4's UI upgrade.
   - The bootstrap's DDR, FlexBus, clock and UART set-up must be recreated in a
     `mk1_bootstrap_state()`.
3. **Flash model:**
   - a CFI/AMD 16-bit NOR at `0x00000000`, 8 MB: read array, `AA/55` unlock, erase,
     program, status polling;
   - the CS1 device at `0x20000000`, 2 MB, plain persistent memory;
   - the SPI flash model on DSPI0 stays for the MKII.
4. **DDR size**: 64 MB for the A4 MKI (the mirror at `0x44000000` matters once the
   stack sits at its top). 128 MB for the Rytm MKI.
5. **Voice SPI on DSPI2 / eDMA ch 29**, frame source per profile. The Rytm's analog
   model must read `0x80004ce0`, the A4's `0x800032b0`.
6. **SSI0 for the Rytm MKI**: the audio ISR and both codecs need frames. The
   capture, `--wav` and window mix assume one SSI.
7. **Panel model**:
   - 122x32 with 4-page tiles;
   - 1-bit LED groups;
   - no UI card query expected (verify);
   - new key maps and window layouts (the tables above).
8. **USB device model**: full speed, MKI descriptors. Low priority: DIN MIDI over
   UART9 is unchanged.
9. **Calibration**: synthetic `RBOW` records must go to the new flash addresses
   (`0x780000` / `0x7c0000` Rytm, `0x7f0000` A4) in parallel-flash layout, or boot
   uncalibrated first.
10. **`Machine::is_rytm()` / `is_a4()`** gate many MKII assumptions (DTIN at
    12.288 MHz, card types, MRAM/supercap GPIO at `0xEC094018/1A`, analog model
    addresses). They need a model enum (`RytmMk2`, `A4Mk2`, `RytmMk1`, `A4Mk1`, DT)
    rather than more booleans.

## 8. Work estimate and order

Reusable as is: the CPU, JIT, EMAC, INTC, PIT, DTIM, eDMA, UART, eSDHC and `ekfs`;
the RTOS profile mechanism and `xmatch`; snapshots; DIN MIDI; the voice models'
frame decoding; the panel's tile protocol and OLED drawing code (with parameters);
most of the window.

New work, shared by both MKIs (do it once):

- ELE2 parsing: small.
- NOR flash and CS1 models: medium.
- The MKI bootstrap state: medium, with the risk in the boot-flag semantics.
- DSPI2 as the voice SPI: small.
- 122x32 display and 1-bit LED groups: small to medium.
- The model enum refactor: medium.

Per device:

- **A4 MKI**: profile (done above, about one hour to enter and check), the panel key
  map (measure), window layout, and the A4 voice model pointed at `0x800032b0`.
  Single SSI as on the A4 MKII. **Estimate: about 2-3 sessions to the main screen,
  1-2 more for keys, LEDs and sound**, comparable to the A4 MKII port.
- **Rytm MKI**: profile (done above), plus SSI0, the pads (velocity and pressure,
  the backlight and level-bar LEDs), a larger key map, and the analog model at
  `0x80004ce0`. The bigger card-content checks (factory samples) may also prompt.
  **Estimate: about 1-2 sessions after the A4 MKI to boot, 2-3 more for pads, LEDs,
  the second SSI and sound.**

**Recommended order:**

1. the ELE2 container, the NOR/CS1 models and the model enum;
2. the A4 MKI: it has the fewest new parts (one SSI, no pads) and its OS carries
   the panel image that names the LCD controller;
3. the Rytm MKI, reusing everything from step 2 and adding SSI0 and the pads.

**What a real unit would settle** (not blocking a first boot):

- the LED-to-group/bit mapping and the raw key codes: measurable in the emulator,
  but a photo or video of the startup LED test helps;
- the Rytm MKI's pad backlight and level-bar control;
- whether the CS1 device survives power-off (battery or MRAM part number from a
  board photo);
- the LCD module or controller part number (a board photo), to name the contrast
  semantics.

## 9. First boot, A4 MKI OS 1.56 (2026-10-12) [measured unless tagged]

`dtboot fw/Analog-Four_Analog-Keys_OS1.56.syx --card COPY_OF_A4_CARD --instr 3G`
reaches the main screen: "120.0 PERF 1:1:1", "KIT 1:POLYTRO", "[A01] A02", the
OSC/filter knobs of track 1. With no calibration record it shows "MISSING CAL: AUD
CV" over it. 13 tasks; SSI1 runs (about 45 k frames per emulated second) with the
A4 voice model on the MKI's frame; keys, LEDs and encoders answer. The window
(`rytm` with that firmware) draws an A4 MKI panel with the 122x32 LCD.

What it took (commits on branch `mk1`):

- **ELE2** (`firmware.rs`): magic, build at +4 (12 B), version at +0x10, MAIN OS
  load address at +0x14, then the packed `[len][sum]` stream at +0x18; depacked
  until the input runs out. The MAIN OS hashes match section 1 above. The OS's own
  packed blobs (0x401bb5a0 etc.) do end with an end marker; only the file's stream
  lacks one.
- **`Device` enum** (`symbols.rs`): `is_rytm()` / `is_a4()` now mean the MKII
  units by device, not by name prefix; MKII and Digitakt behaviour is unchanged
  (the gate screens are byte-identical).
- **Profiles** `a4mk1_156` and `rytm_mk1_174`: section 6's addresses, re-run
  through `xmatch.py` (same results). `panel_diff` 0x4007333a read by hand.
- **Memory**: 64 MB DDR on the A4 MKI. NOR flash (`nor.rs`): two x8 AMD parts on
  the 16-bit bus (every command byte on both lanes: `0xAAAA` to `0xAAAA`), 8 KB
  sectors below 0x10000 (the bootstrap upgrade erases eight of them,
  0x40100c36), 64 KB above [inferred]. Its content lives in the bus's sparse
  pages (erased = 0xFF), so snapshots carry it. The flash gets the bootstrap
  MAIN OS carries (0x40226ef0..0x40233fc0) at 0 and the version record
  (0x40226ee8, `0x0601`) at 0xfff8, as a unit's would; the entry compares those
  and skips the bootstrap upgrade. CS1 at 0x20000000 is plain memory, zero at
  first [inferred: battery-backed].
- **Boot flags 0.** Bit 20 selects the **Analog Keys**: with it the intro logo
  reads "analog KEYS" and 0x400f1110 names the unit "Analog Keys" (clear: "Analog
  Four"); with bit 20 and a top byte of 0, 1 or 3 the OS also starts a panel
  firmware upgrade (0x40100f7c). Bits 0-2 and 31 concern the CS1 settings
  (0x40000f2a..0x4000102a). With a blank CS1 the entry's check (0x400f1308)
  fails and the OS depacks its defaults (0x401bb5a0, 2096892 B) into
  0x20000104 itself.
- **Timers**: DTRR resets to 0xFFFFFFFF (the OS runs DTIM0/3 free with FRR set,
  never writing DTRR; with 0 the timers fired every bus clock). DTIM0 is started
  as the bootstrap does (DTMR 0x0b, Rytm MKI blob 0x4028cf58).
- **Codec control** (`mk1.rs`): the OS writes codec register 2 = 1 over DSPI1 in
  the Cirrus SPI format (chip address 0x9E/0x9F) and reads it back
  (0x40001150 / 0x400010e8); on a mismatch it leaves SSI1 off (`*0x4043f3f0` =
  1). A small register file answers. 0x9E is the CS4271's address [inferred].
- **Voice frame**: DSPI2 SR reads TCF set (the sender 0x40061b7e waits for it);
  the A4 voice model reads the frame at 0x800032b0 (0x400f64be hands that to the
  builder 0x40061b08).
- **Calibration**: the A4 MKII's synthetic record, its neutral tables taken
  0x26750 lower in the MKI image (xmatch: six of nine words at that distance),
  in the NOR flash at 0x7f0000. "MISSING CAL" goes away.
- **Display**: the framebuffer and the panel's tiles are 4 pages of 128 columns,
  page 0 the bottom band (as the MKII's OLED); 122 columns are on glass.
  `Machine::panel_pixels` and `DTBOOT_END_PNG` draw 122x32.

**Panel.** Keys arrive as on the MKII, `0x2g mask` (handler 0x40062b1c, slot =
8 group + bit into the OS's table `0x401ebaf8`; the Analog Keys' is 0x401ebc78).
Other panel messages: `0x3e delta` an encoder (wire channel -> knob table
0x401ebdf8: B C D E G H I J A F LEVEL), type 4 two absolute inputs (pots), types
5/6/8 three-byte messages (the Keys' keyboard [inferred]). Encoder counts are
taken one to one: five turns of +1, +2, +4, +8 moved OSC1 TUN (from -12) by 5,
10, 20 and 40. Names, `panel::A4MK1_KEYS`:

| group | bits 0..7 |
|---|---|
| 0, 1 | T1..T8, T9..T16 |
| 2 | PAGE, NOTE, OSC1, OSC2, FILTERS, AMP, ENV, LFO |
| 3 | TRACK1..4, FX, CV, TEMPO, TRNS |
| 4 | YES, NO, LEFT, DOWN, RIGHT, UP, PERF, ARP |
| 5 | RECORD, PLAY, STOP, the black keys C# D# F# G# A# (ids 0x90..0x94: notes 61, 63, 66, 68, 70 on MIDI out) |
| 6 | the white keys C1..C2 (all id 0x86; notes 60, 62 .. 72) |
| 7 | BANK GROUP (0x60; it shows the track mute row) [inferred name], SONG MODE, CHAIN, FUNC, BANK A..D (0x61..0x64: "BANK A : CHOOSE PTN") |
| 8 | pushes B C D E G H I J |
| 9 | bits 4..6: pushes A, F, LEVEL; bits 0..3, 7 none on the A4 |
| 10 | ids 0xa0..0xa5: Analog Keys keys, not on the A4's panel [inferred] |

The page keys, BANK A..D, keyboard and track keys were checked on screen or MIDI
out; FUNC, SONG MODE, CHAIN, TEMPO, TRNS, PAGE and the pushes keep the A4 MKII's
ids [inferred].

**LEDs**: `0x2g bits`, 14 groups, one bit per LED (LED = 8 g + bit), read back
after key presses (`DTBOOT_LEDS=1`): trigs 0..15; REC 32; the four bank LEDs
40..43 (the pattern's bank); CHAIN 45, SONG MODE 46; PERF 48, ARP 49, NOTE 50;
FILTERS 56, AMP 58, OSC2 60, OSC1 62; FX 64, CV 66, LFO 68, ENV 70; TRACK2 72,
TRACK1 74, TRACK4 76, TRACK3 78 (each track's neighbouring bit is set while it
is not selected). The pattern page LEDs are group 2's high nibble [inferred].
The host also sends `0x36 xx` and `0x3f xx` (not LEDs; meaning open) and `43 00`
once at boot (unanswered; nothing seems to wait for it).

**Open (A4 MKI):**

- **A blank card** (fixed, 2026-10-12) [measured]. On a +Drive without its header
  (0xBEEFBABA at byte 0; 0x400708ec returns 1 when the card is present and the
  header is missing) the startup task (0x40096908) runs the factory-reset handler
  (0x400965c6) with the flags (1, 1, 1): step 1 (0x40072284) copies CS1's
  working project (2061014 B from 0x20000104) into project slot 1 (id 1); step 2
  (0x400727cc) depacks the default image 0x401bb5a0 into a new slot for id 0. Slot
  1 is still in use, so that is slot 2 (0x40642490), whose data area holds
  2061016 B up to the slots' end at 0x40839774 (0x40020050 walks 2061028-byte
  slots to there). The MKI's default image is the whole CS1 image, 2096892 B
  (0x20000104..0x20200000, written there by the entry at 0x40000fc2), so it
  overruns 35876 B into live statics and the OS dies in its abort loop. The A4
  MKII's default (0x401dd90c) is 2056068 B and fits. RAM size and CS1 contents
  play no part (the slots are fixed BSS); it is the image's own code, so a unit
  meeting such a card would do the same. The bootstrap menu's FACTORY RESET
  passes boot flags 2, or **0x12** with keys 0x81 and 0x82 held (A4 MKI blob,
  offset 0x3b08..0x3b42; EMPTY RESET passes 1, its USB maintenance commands
  `#BOOT ER/FR/PR` 1/2/8). Bit 4 runs the same handler with flags (1, 0, 1):
  no CS1 copy, so the default lands in slot 1 and overruns only into slot 2.
  The emulator now boots the A4 MKI on a blank card (no header at byte 0) with
  boot flags 0x12 (`attach_card`; `DTEMU_BOOT_FLAGS` overrides): the OS
  initialises the card, and later cold boots on it (flags 0) reach the main
  screen. `dtcard X.img format` or a missing image both work. The Rytm MKI has
  the same handler (0x400a0fc0, 0x40091128 by xmatch) and the same bootstrap
  menu code (0x12 at its blob's 0x3982); its header is 0xBEEFBACE (section 10).
- The LED bits not listed, the second bit beside each page/track LED, TEMPO,
  the Mode and Oct LEDs; the pots (type 4); `43 00`, `0x36`, `0x3f` (not
  looked at in this round; the Rytm MKI sends `43 00` too, and its `0x3g`
  are 16 level values beside the 12 bit groups, so the A4 MKI's `0x36` /
  `0x3f` are probably LED levels as well [inferred]).
- The codec's other registers; USB (full speed) is not modelled.

**Noise in the window (fixed, 2026-10-12).** Reported: with the A4 MKI in the
window, white noise on the left output at idle, on both once PLAY runs. Two causes
[measured, headless: `dtboot --wav` (the 8 DAC words) and `--analog-wav`, and the
window's own path with `rytm --headless --audio-null --wav`, from snapshots before
and after PLAY on the default project "KIT 1:POLYTRO"]:

- **The window played DAC words 0 and 1 as L/R.** Its model-mix branch was taken
  for the A4 MKII only (`is_a4()` is the MKII device), so the A4 MKI fell through
  to the Digitakt path. On the MKI the DAC's words 0..3 carry white noise
  (spectral flatness 0.56, centroid 12 kHz, no lag correlation): word 0 at
  -28.0 dBFS and word 2 at -48 dBFS at idle; with the pattern playing words 0, 1, 2
  at -28.0 and word 3 at -41 dBFS. That is the noise generator, as the MKII's
  word 3 (-33.3 dBFS at rest), here one word per voice [inferred: four words,
  levels changing with the playing tracks]. Words 4/5 carry low-frequency content
  while playing (-36 dBFS), the effects' return as on the MKII [inferred: same
  slots, not probed with a send]. The window now takes the model's mix (voices
  plus TX 4/5) for the A4 MKI too. Window output before: idle L -28.0 dBFS noise,
  R silent; playing L and R -28.0 dBFS noise (uncorrelated). After: idle digital
  silence; playing -12.5 dBFS, tonal (flatness 0.000, centroid 430 Hz).
- **The model's triangle carried a DC.** It integrated the pulse itself, whose
  mean is 2 PW - 1, through a 0.999 leak: a DC of 4 dt (2 PW - 1) / 0.001. The
  default kit's tracks use triangles (switch words 0x0033) with PW words such as
  0x45b0, so the ladder's tanh sat at a rail: the model mix while playing was
  -6.3 dBFS, almost all DC (mean -0.48, centroid 1 Hz). Now the integrator takes
  the pulse less its mean: playing, the mix is tonal around zero (mean -0.01).
  KB_C1 on TRACK1..4 from a stopped idle state: -18.3, -18.6, -29.3, -32.1 dBFS,
  flatness 0.000..0.002, idle before the note exact zero. This is shared with the
  A4 MKII: at PW 0x8000 nothing changes, but its default kit's OSC2 is a
  triangle at PW 0x7735 (level nearly off): its render of a MIDI note (ch 1, 3G,
  copy of a4.img) differs in 23 of 144000 frames by one 16-bit LSB, and its end
  screen is byte-identical [measured]. With OSC2 LEV up the MKII had the same DC
  [inferred from the formula].

## 10. Rytm MKI OS 1.74 (2026-10-12) [measured]

First attempt: on a copy of a Rytm MKII card it creates 9 tasks, passes the codec check (it
writes and reads register 2 of two codecs, PCS0 and PCS1, `0x4000656c`), and
draws the start of its intro logo on the 122x32 LCD; then the SSI0 interrupt
(vector 174, 0x40115884) spins at 0x4011589a on the DONE bit of the eDMA TCD
whose address it keeps at `*0x80004420`. The entry's SRAM copy (0x40000506:
0x402a3000.. to 0x80000000) puts 0xfc045400 there, but stores through the SRAM
mirror (DTEMU_STRICT=log: 0x800c2218.., 0x80026078..) clear it later: something
upstream writes through stray pointers. Not traced further; SSI0 and the second
codec are not modelled either. The Rytm MKI's pads, its key table and LEDs are
untouched.

**The overwritten pointer (fixed) [measured].** The stores that cleared
`*0x80004420` were eDMA, not the CPU: the Rytm MKI sets the eDMA's minor-loop
mapping (CR[EMLM]) and its SSI receive channels use it. SSI0 receive (channel
48, SRX0 to 0x80002000) has NBYTES `0x40004018`: DMLOE, an offset of +16 and 24
bytes per minor loop, so 128 frames of 40 bytes and DLAST -0x13f0; SSI1 receive
(channel 52) `0x40006010`. The emulator read NBYTES as 0x6010 / 0x4018 bytes, so
the destination ran through the SRAM mirror and over everything. `edma.rs` now
splits NBYTES under EMLM and adds the offset after every minor loop but the last
(DLAST then returns to the buffer's start, which fixes that rule). With that the
OS runs on: 13 tasks, the audio interrupt runs, and the screen comes up.

**To the main screen (2026-10-12) [measured unless tagged].** On a blank card
(`dtcard X.img format`, or a missing image) the Rytm MKI OS boots to its main
screen: "127.0 BDPL 1:1:1", "KIT 1:RYTHLIN", "[A01]", the BD page's TUN SWT SWD
DEC / TYP MOD TIC LEV. What it took after the eDMA fix:

- **Blank card**: the same factory-reset path as the A4 MKI (section 9). The
  Rytm MKI's +Drive header is **0xBEEFBACE** (0x40091128; a Rytm MKII card
  carries it too). Booted with flags 0x12 the OS lays out its own sample
  filesystem: **ekFS version 1 at sector 0x180000**, 8 KB blocks (`ekFS 0001
  0000 2000`), where the MKII's is version 2 at 0x1C0000. So a Rytm MKII card
  (or a `dtcard`-formatted sample area) boots to "ALERT: FAULTY FILESYSTEM.
  PLEASE FORMAT +DRIVE SAMPLES". The first boot shows "FILE SYSTEM OK, PRESS
  Y/N TO CLOSE" (NO closes it).
- **Calibration**: the MKII's record format (RBOW, format 5, block 1 12490 B,
  110706 B; 0x400f3e40), in the NOR flash at 0x780000 with a copy at 0x7c0000
  (the reader 0x400f3f4c also tries 0x7f0000). The version word at +16 means
  the opposite of the MKII's: **1 is current** (0x400f4254), 0xFFFF missing
  (0x400f42a6); with 2 the OS says "MISSING CAL: SYNTH" and offers its 2.5-hour
  "improved tuning" calibration. The record carries 1 on the Rytm MKI.
  Whether the offer also depends on ADC channel 0 (`0xFC094012`, filtered;
  0x400fd47c asks for <= 3700 after scaling; the ADC reads 0 here) was not
  needed [inferred].
- **UI firmware version**: the bootstrap puts byte 1 of the panel's `70 00`
  reply (the UI card query, blob 0x3ef0) in the boot flags' top byte (blob
  0x8a4) and sets bit 5 when the panel's pad calibration (query `ad fc`, reply
  magic 0x422a, blob 0x411c) is missing ("MISSING CAL: PADS"). With a top byte
  of 0 the OS adds "OLD UI FW" (0x400a19be). The emulator now passes the panel
  model's version (2) there, for both MKIs (the A4 MKI's screen is unchanged).
- **Left open**: "ERROR: NO FACTORY SAMPLES" at every boot (NO closes it). The
  OS reads 0x2000 bytes at card offset 0x100000 (0x400f9fa6) and wants a "MaGi"
  record, version 1, whose words sum to 0, presumably with the factory
  samples it describes [inferred].

**Panel [measured].** The OS's slot table is `*0x40249dd8` = 0x40249e38, 10
groups of 8 with 32-bit ids, read from a running machine; ids as the Rytm
MKII's plus the A4 MKI's BANK GROUP 0x60 and BANK A..D 0x61..0x64. Names
(`panel::RYTMMK1_KEYS`) from the panel photo, by id [inferred]:

| group | bits 0..7 |
|---|---|
| 0, 1 | T1..T8, T9..T16 |
| 2 | PAGE, TRIG, SYNTH, SMPL, FLTR, AMP, LFO, TEMPO |
| 3 | MUTE, FX, TRK, FUNC, RTRG, CHRO, PERF, SCNE |
| 4 | YES, NO, LEFT, DOWN, RIGHT, UP, SONG, CHAIN |
| 5 | RECORD, PLAY, STOP, BANK GROUP, BANK A..D (Trig Mute, Accent, Swing, Slide) |
| 6 | -, -, pushes E D C B A, LEVEL |
| 7 | -, -, -, -, -, pushes H G F |
| 8, 9 | pads BD SD RS CP BT LT MT HT, CH OH CY CB |

The parser (0x4008068a, two-byte messages; type = high nibble) takes `2g` key
groups (0x400803aa), `3e` encoders (0x40080618; wire -> knob map 0x40249fb8:
B C D LEVEL F G H A E), `9p` pad pressure, `Ap` pad strike with velocity
(threshold 0x0f at 0x40249dec), `Bp` pad release, through the pad map
0x40249dd0 (identity) - the MKII's pad messages - and `Cx` (key bits from slot 64 on [inferred]; not
used here). Types 4..8 are ignored. Host to panel: `2g` LED groups 0..11, `3g`
16 more groups (0x30..0x3f; the pads' lights and level bars [inferred]), `0f 01`,
`0f 08`, `9d 00` and `43 00` (not answered).

**SSI0 and sound (2026-10-12).** [measured] from the set-up code
(0x40006732..0x400069ba) and the TCDs: SSI0 and SSI1 run on one frame of 8
slots. Transmit: one 16-word frame per sample in an 8 KB ring at 0x80000000
(SMOD 13); SSI0's channel 50 sends words 0..7 (32 bytes, then +32), SSI1's
channel 54 words 8..15, so together they carry the Rytm MKII's 16-word DAC
frame. Receive: a 10-word frame at 0x80002000, SSI0's channel 48 words 0..5
(SRMSK 0xffffffc0), SSI1's channel 52 words 6..9 (SRMSK 0xffffffcc: slots 0, 1,
4, 5). Both use minor-loop offsets (section 10 above). The DMA scatter-gather
descriptors live at 0x4b02e520 / 0x4b02e540, past the 128 MB of DDR: the
uncached mirror of 0x4302e5xx (ACR0 caches 0x40000000..0x47ffffff only)
[inferred]; the bus mirrors DDR, so that works as is.

`io::Ssi0` models SSI0 on the Rytm MKI only: requests on channels 48 / 50 each
frame, STX words into DAC words 0..7 (SSI1's into 8..15), SRX words from the
analog model; with it on, request counts follow the slot masks, so SSI1
receives once a frame. The Rytm MKII's analog model (`analog.rs`) runs on the
MKI's control frame at **0x80004ce0** and FX parameters at **0x800066e6**
(the code writing frame word 0 from them, 0x40119ece, matches the MKII's
0x40121306). Its 12 receive words go out as words 0..5 on SSI0 and 6..9 on
SSI1 [inferred: the MKII's order, voices 0..7 then AUDIO IN]. Pads struck from
the panel sound through it as on the MKII (BD, SD, CH hits in `--analog-wav`
with the same envelopes as the MKII 1.73 on its card; LT silent on both).

**LEDs and window.** Key LEDs (`panel::RYTMMK1_LEDS`) [measured, pressing
keys and reading the groups back]: trigs 0..15, RECORD 16, SONG 17, CHAIN 18,
BANK A..D 24..27, the bank-group pair 28/29 [inferred: E..H / A..D], CHRO 32,
MUTE 33, SCNE 36, PERF 37, FX 39, FLTR 41, SMPL 43, SYNTH 45, TRIG 47, LFO 53,
AMP 55 (the page keys sit on odd bits, as on the A4 MKI). LED 20 is lit from
boot: the 1:4 page LED [inferred]. Groups 7..11 change with the pads (the
selected track, MUTE and CHRO modes light patterns of about 3 bits a pad) and
the `0x3g vv` values change with them too; neither is mapped. Encoders checked
on screen: A = TUN ("Tune"), E = TYP ("Mod Type"), F = MOD, H = LEV, LEVEL =
"Trk Level". The window (`rytm fw/Analog-Rytm_OS1.74.syx --card C.img`) draws
the MKI panel after the photo (`gui::layout_rytmmk1`) with the 122x32 LCD.

**Open (Rytm MKI):** the pad LEDs and level bars (groups 7..11, `0x3g`); the
factory-sample record ("NO FACTORY SAMPLES" at each boot); the panel queries
`43 00`, `9d 00`, `0f 01/08`; pad pressure curves (the MKII's `9p` stream is
accepted, not checked against a unit); the receive-word order; USB (full
speed) is not modelled; Master Volume is an analog pot.

## 11. Board photos [community photo, low resolution]

- **A4 MKI UI board** (blue): a separate panel board with one QFP chip mid-board,
  most likely the panel MCU, and many small SOIC/TSSOP parts (LED drivers or key
  shift registers, [inferred]); 10 encoders in a metal bracket, two pots (master
  volume and LEVEL), discrete through-hole single-colour LEDs, and a graphic LCD
  module on its own small PCB (the 122x32 module). So the panel MCU is separate,
  as on the MKII, consistent with the UART8 protocol above.
- **Rytm MKI UI board** (green, marked UI8202C): the 12-pad matrix, 8 encoders in
  a bracket and the same kind of LCD module.
- **Rytm MKI main board** (blue, "CPU8001" at its edge): a large BGA at lower right
  that appears to be marked ColdFire (consistent with MCF5441x), a ribbon cable to
  the UI board, a QFP mid-board and a dense analog section.
- Chip part numbers are not legible at these resolutions.

## Later: the Teensy voice expander on a Rytm MKI (noted 2026-10-13, not started)

Our Teensy setup targets the Rytm MKII (OS 1.73, Overbridge at USB high speed). Three things must change for an MKI:

1. **Bandwidth** [manual: Overbridge manual, appendix C].
   - The MKI is a USB Full Speed device (12 Mbit/s), limited to 8 channels per direction and 10 in total.
   - The MKII uses 12 per direction.
   - A realistic split is 6-8 Teensy voices into the Rytm plus 0-2 channels back (MAIN), with no per-voice stems.
2. **The Teensy host.**
   - `RytmHost` assumes the MKII's high-speed link: 368-byte blocks, 12 channels, alt setting 3.
   - The MKI needs a full-speed mode with its own block layout and channel count.
   - Its descriptors aren't dumped yet. Overwitch supports the MKI and is the reference.
3. **The OS mods.**
   - rytm-mods targets MKII OS 1.73: the trig reports, Overbridge inputs into each track's analog chain, machine names, and user slots.
   - All of them would need porting to MKI OS 1.74. This emulator boots that OS, so the port can be found and tested here.

Research step first, before building anything:
- the MKI's Overbridge channel map in each direction (Overwitch, and the emulator's USB model once added);
- whether host-to-Rytm audio can reach a track's analog chain on the MKI at all (the deciding question);
- how much of the 1.73 mods can be located in the 1.74 MKI OS.

Order: after the MKII hardware test.
