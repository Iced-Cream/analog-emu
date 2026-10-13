# Porting to the Elektron Syntakt (survey)

2026-10-13. Syntakt OS 1.42 (`fw/Syntakt_OS1.42.syx`, from
`https://www.elektron.se/wp-content/uploads/2026/10/Syntakt_OS1.42_dist.zip`, linked
from elektron.se's Syntakt support page; .syx 2572064 B, sha256
`ded3c59fdb909eb9e2c3307b30a486452c8fa6947b12c14e0ce35a187b7f37c1`; zip 28063237 B,
sha256 `0556e748...4e77`). Compared against Digitakt OS 1.53 and Analog Rytm MKII
OS 1.73. Everything is static analysis: `fwinfo`, disassembly with `tools/cfdis.py`,
strings, `tools/xmatch.py`, plus the OS 1.30 user manual and the owner's photos
of the front panel and the main board. Sections 1-9 are the survey, written before
anything was booted; **stage 1 (2026-10-13) boots OS 1.42 to its main screen** with a
stand-in for the second CPU: section 10. **Stage 2 runs the second CPU for real** (section 7
on a second emulated ColdFire): the digital tracks 1-8 sound. Section 11. **Stage 3 models the
analog tracks 9-12 and the analog FX block** from the 500-word voice frame: section 12.
Each claim is tagged **[measured]** (read from the image, the code or a tool's
output), **[inferred]**, or **[community]** (manual, photo or public source, not
checked against code).

## Summary

The Syntakt is the same platform family as our MKII units, but it has **two CPUs**:

- **CPU1, the "CF"** (main OS, section 3): an MCF5441x that runs the RTOS, the
  UI, the sequencer, the codec, USB, the +Drive (eMMC), the panel and DIN MIDI.
  It is a Digitakt-style OS. The RTOS and middleware match the Digitakt by
  `xmatch` (section 6), and the panel MCU firmware is byte-identical to the
  Digitakt's and the Rytm MKII's.
- **CPU2, the "DSP"** (section 7, 383760 B): a second MCF5441x with its own DDR.
  It runs a small bare-metal program, with no RTOS, that renders the eight digital
  voices (tracks 1-8). CPU1 boots it over SPI: section 6 is a serial-boot loader,
  section 7 the program.
- **The CPU link is a 64 KB dual-port mailbox in an FPGA**, on FlexBus CS0 of both
  CPUs (CPU1 at `0x10000000`, CPU2 at `0x00000000`). An IRQ line and a timer-capture
  pin carry the timing. The DSP configures that FPGA itself from a bitstream packed
  inside section 7, which depacks to exactly the XC3S50A's size.
- **The analog side** (tracks 9-12 and the analog FX block) is driven like the
  Rytm MKII: a SPI voice frame on DSPI1 / eDMA ch 15, with the same marker words.
  The frame is 500 words (the Rytm's is 492). Section 1 is an XC3S200A-sized
  bitstream, as on the Rytm MKII.

| | Digitakt | Rytm MKII | Syntakt |
|---|---|---|---|
| CPUs | 1 | 1 | **2** (CF + DSP), both MCF5441x |
| ELE3 sections | 5 | 5 | **8** (adds 1, 4, **6, 7**) |
| SysEx device byte | `0x0A` | `0x0C` | **`0x16`** |
| MAIN OS entry | `0x400004E8` | | **`0x400004E8`** (same code) |
| UI card type | 4 | 7 | **`0x0B`** |
| Display | 128x64 OLED | 128x64 OLED | 128x64 OLED |
| Voice SPI frame | none | 492 words | **500 words** |
| Codec SSI1 frame | | 16 words | **8 words of 24 bits** |
| Digital voices | CPU | CPU | **CPU2, via the FPGA mailbox** |

## 1. Container [measured]

`fwinfo` reads the file as is. It is ELE3, build `"0097"`, version `"1.42"`, with 8
sections. META says `260930 11:17:25`.

| id | role | stored | expanded | dest field | sha256 (expanded, prefix) | vs Rytm MKII / DT |
|---|---|---|---|---|---|---|
| 5 | META | raw 15 | 15 | 0 | `7d97057d` | |
| 2 | BOOTSTRAP | packed 15408 | 30782 | `0x04000000` (version word) | `9fa34a83` | DT `0x03000900`, Rytm `0x07020000` |
| 1 | FPGA bitstream (CPU1 side) | packed 49252 | **149516** | 0 | `c920a4cb` | same size as the Rytm's, different content |
| 3 | MAIN OS | packed 1442812 | 3442576 | `0x40000400` | `1a9723b1` | |
| 4 | UPDATER | raw 32776 | 32768 | `0x80000400` | `6c162682` | like the DT's (different bytes) |
| 6 | **DSP serial-boot loader** | raw 1744 | 1744 | 0 | `eec1379d` | **new** |
| 7 | **DSP program** | raw 383760 | 383760 | 0 | `daf6451c` | **new** |
| 8 | PANEL_MCU | packed 103416 | 159948 | 0 | `6943b2f2` | **identical** to DT 1.53 and Rytm 1.73 |

- **Section 1** starts with 32 bytes of `0xFF`, then the Xilinx sync word `AA99`
  and `30A1 0007 ...`. 149516 B is exactly the XC3S200A bitstream length
  (1,196,128 bits) [community: Xilinx UG332 table].
- **Section 6** is an MCF5441x serial-boot image. Its header is
  `ff01b201 610015a1`. From offset 8 it is loaded at `0x80000000` (SRAM): the
  initial SP is `0x80010000` and the PC `0x8000065a`.
  - It holds the DDR controller init: 45 references to `0xFC0B8xxx`.
  - It pulls up to 2 MB to `0x40000400` (magic `0x12345678`) over its DSPI0 and
    jumps through the word at `0x40000400`.
- **Section 7** is linked at `0x40000400`. Its word 0 is the entry, `0x40001070`.
  Its entry code is the same as MAIN OS's: SP `0x48000000`, PPMCR clock enables,
  CACR `0xa50ce100`, ACR0 `0x4007e020`.

## 2. CPU and peripherals

### CPU1 ("CF", MAIN OS) [measured]

- **Entry** `0x400004E8`, the same as the Digitakt:
  - it stores its caller's argument (the boot flags) at `0x4029a778`;
  - SP `0x48000000`, CACR `0xa50ce100`, ACR0 `0x4007e020` (128 MB);
  - there is no bootstrap-version check like the MKI's.
- **DDR:** both CPUs set the same ACR0 and SP, so each OS behaves as 128 MB, with
  the usual uncached mirror at `0x48000000`.
  - The board photo shows three DDR2 packages [community photo]: one Samsung
    K4T1G16 (1 Gb x16) and two Nanya NT5TU128M8 (1 Gb x8).
  - How they split between the CPUs is not settled. One CPU might have two x8
    parts for a 16-bit bus, which would be 256 MB. Neither OS uses that much.
    `bus.rs`'s 128 MB mirroring fits both images as written [inferred].
- **Peripheral references** (32-bit operands per 16 KB block; "ST CF" is MAIN OS,
  "ST DSP" is section 7, "ST ldr" is section 6):

| block | ST CF | ST DSP | ST ldr | Rytm MKII | DT |
|---|---|---|---|---|---|
| DSPI0 `0xFC05C000` (SPI flash / DSP boot link) | 314 | 14 | 15 | 312 | 315 |
| DSPI1 `0xFC03C000` (voice SPI / DSP FPGA config) | 62 | 13 | 0 | 62 | 48 |
| DSPI2 `0xEC038000` (**slave**: DSP boot server) | 10 | 1 | 0 | 1 | 0 |
| eDMA + TCDs | 319 + 296 | 14 + 14 | 0 | 261 + 245 | 240 + 229 |
| USB OTG | 154 | 0 | 0 | 149 | 147 |
| DDR controller | 0 | 0 | **45** | 0 | 0 |
| SSI1 | 23 | **0** | 0 | 29 | 23 |
| eSDHC | 131 | 0 | 0 | 131 | 128 |
| FlexBus | 35 | 5 | 0 | 29 | 29 |
| GPIO | 266 | 41 | 12 | 236 | 219 |
| UART8 / UART9 | 24 / 22 | 1 / 0 | 0 | 21 / 22 | 26 / 22 |

- **FlexBus on CPU1** (`0x40082510`):
  - CS0: CSAR `0x10000000`, CSMR `0x10001` (64 KB), CSCR `0x100780`. This is the
    DSP mailbox.
  - CS1: CSAR `0`, CSMR `0x10001`, CSCR `0xfd80`, with no use found yet.
- **Self-test strings** add to the Digitakt's: `PCBA0046%c%d`, FPGA (`INITB NEVER
  WENT HIGH`, `DONE NEVER WENT HIGH`), DSP (`TIMEOUT`, `CF MEMORY`, `DSP MEMORY`,
  `DSP BUFFER SIZE`), SUPERCAP, eMMC (`DAT0<->IRQ3`, `EXT CSD`, `WRONG DEVICE
  MANUFACTURER`), the codec clocks, analog `CTRL_REG00..66`, and `CV_SH0_*`,
  `CV_SH1_*` (sample-and-hold CV channels).
- **The UI card type is `0x0B`** (`0x40080434`; otherwise "WRONG UI CARD").

### CPU2 ("DSP", section 7) [measured unless tagged]

- **Bare metal.** The vector table is filled with an `rte` stub at `0x40000404`.
  There is no RTOS context switch.
- **Init** (`0x40000db4`):
  1. sets the VBR;
  2. sets FlexBus CS0 at `0` (64 KB, CSCR `0x100780`; `0x40000548`);
  3. clears 64 MB of BSS (`0x4004f6e0..0x4404f980`);
  4. depacks the FPGA bitstream at `0x40041060` to `0x40042000` with an aPLib-style
     routine (`0x400005a2`, the same scheme as `firmware::depack`). It depacks to
     **54664 B, exactly the XC3S50A bitstream length** [measured size; the part
     name is from the community photo];
  5. configures that FPGA with Xilinx slave-serial on DSPI1 (`0x40000706`):
     - 16-bit words go out through PUSHR `0x9000xxxx`;
     - GPIO `0xEC09401C` bit 0 is INIT_B and `0xEC09401D` bit 7 is DONE
       [inferred from the error codes];
     - it returns 1, 2, 3 or 4, which match CPU1's "INITB NEVER WENT HIGH", "INITB
       WENT LOW DURING / AFTER DATA", and "DONE NEVER WENT HIGH" strings;
  6. runs a mailbox handshake (below);
  7. enters the audio tick.
- **The audio tick:**
  - DTIM0 runs in input-capture mode (DTMR `0xc1`, `0x40000e40`), so it is clocked
    by an external pin, the FPGA's frame clock [inferred]. Its ISR is installed at
    vector 96.
  - The block ISR at `0x40000934`:
    1. copies 432 B (`0x1b0`) of parameters from the mailbox at `0` to SRAM
       `0x80008000`;
    2. runs the voice code;
    3. starts eDMA ch 47 (TCD `0xFC0455E0`). It moves 64 x 16 B = 1024 B from SRAM
       `0x80003840` to mailbox `+0x1b0`. That is the rendered audio block
       [inferred].
  - Then it toggles a GPIO (`0xEC09401E` / `0xEC09402A`, `0x4000090e`), which is
    the IRQ to CPU1 [inferred].
- **DSPI0** reads the boot stream: command `0x03` plus a 24-bit address on PCS1,
  then it waits for a `0xAA` sync byte before reading the length and the data
  (`0x40000b92`).

### The CPU1 to DSP link [measured]

- **Booting the DSP** (`0x40082c04`):
  1. CPU1 fetches sections 6 and 7 from its own SPI-flash container by id
     (`0x40116246(6, ...)` and `(7, ...)`, through `flash_read` `0x40116d7a`).
  2. It builds two boot streams: `0xFF` x 8 + `0x0F` + section 6 at `0x4f8af2c1`,
     and `0xAA` + u32 length + section 7 at `0x4f84f2c1`.
  3. It serves them from **DSPI2 in slave mode** (MCR `0x00010c00`) through **eDMA
     ch 29** (TCD `0xFC0453A0`, destination PUSHR byte `0xEC038037`).
  4. GPIOs `0xEC09404A` / `0xEC094025` / `0xEC09400D` drive the DSP's reset and
     boot-mode lines [inferred].
- **The mailbox handshake** (CPU1 ISR `0x40083034` on vector 68, INTC0 source 4,
  probably EPORT IRQ4 [inferred]; set-up at `0x40083288`):
  1. CPU1 writes `0xAAAAAAAA` / `0x55555555` to `0x10000000` / `+4` and reads them
     back ("CF MEMORY").
  2. CPU1 writes `+2 = 1` and `+0 = 0x4230` ("B0").
  3. The DSP answers `'HO' 'HA'` (`0x484F`, `0x4841`) at `+0` / `+2`, with its
     version at `+4`, or `'NO' 'NO'` on a memory failure. It writes progress codes
     at `+8`.
  4. CPU1 writes `+2 = 2`, then `3`.
  5. The DSP reports `0xA5A5`.
  6. CPU1 sets the DSP state `0x41bbf448 = 6` (OK). States 1-5 select "DSP BOOT
     FAILURE (...)" (`0x4000b37a`).
- **Per block:** CPU1 eDMA ch 47 reads the DSP's output from `0x100001b0` into
  SRAM `0x80003d50` (`0x400a47fa`).

### Codec, analog voices, storage, USB

- **SSI1 codec** [measured]: CCR `|= 0x16000`, bit 18 cleared, `|= 0x700`
  (`0x40000fb4`). That is **8 words of 24 bits** per frame, like the A4. There is
  one SSI; the DSP has no SSI.
- **Analog voice frame** [measured]:
  - builder `0x401021b4`, sender `0x4010223e`;
  - `0x8000AAAA`, then **500** (`0x1f4`) words `0x8001wwww`, then `0x08005555`;
  - double buffers at `0x800051e0 + 0x830*k`;
  - DSPI1, eDMA ch 15 (TCD `0xFC0451E0`), the same scheme as the Rytm MKII.
  - It feeds 3 analog drum voices, 1 analog cymbal and the analog FX block. The
    frame's contents are mapped in section 12.
- **+Drive:** eSDHC with eMMC, as on the Digitakt and Rytm MKII (the same
  `sd_*` profile block matches).
- **USB:** "hi-speed USB 2.0" [community: manual]. Strings `OVERBRIDGE`, `USB
  AUDIO/MIDI`; class-compliant audio can send MAIN or tracks 9-12 [community:
  manual]. The Rytm's `usb_audio_copy` / `voice_output_mix` do not match.
- **DIN MIDI on UART9**, the panel MCU on UART8: the same counts as the Digitakt.

## 3. Public hardware facts

- **Main board** PCBA0046D, (c)2021 [community photo, the owner's unit]:
  - **two Freescale MCF54415CMJ250** (250 MHz MCF5441x), each with its own 24 MHz
    crystal;
  - **two Xilinx Spartan-3A**, XC3S200A (VQ100) and XC3S50A (VQ100);
  - Samsung K4T1G16 and two Nanya NT5TU128M8 DDR2;
  - a large analog section of op-amps, DACs and S&H;
  - a Coiltronics VPH4-0047TR supply transformer;
  - a 2x20 header to the UI board.
- The OS agrees on every count it can check [measured]:
  - two ColdFire programs;
  - two bitstreams, of XC3S200A and XC3S50A size;
  - `PCBA0046%c%d` in the self-test.
- **Manual (OS 1.30)** [community]:
  - 128x64 OLED;
  - 48 kHz, 24-bit D/A and A/D;
  - tracks 1-8 Digital, 9-11 Analog Drum, 12 Analog Cymbal;
  - an analog FX block with its own FX track;
  - MIDI In/Out/Thru with DIN sync;
  - startup menu by FUNC + power: TRIG 1 test mode, 2 empty reset, 3 factory reset,
    4 OS upgrade.
- Elektron's other two-ColdFire designs (the Digitone family) are said to split
  UI and synthesis the same way [community, unverified].

## 4. Voices: the digital and analog split

| tracks | voice type | rendered by | path to the outputs |
|---|---|---|---|
| 1-8 | Digital | **CPU2** (EMAC, soft float tables) | mailbox `+0x1b0` -> CPU1 eDMA ch 47 -> CPU1 mix -> SSI1 codec |
| 9-11 | Analog Drum | analog circuits | CPU1 voice frame (DSPI1, 500 w) -> XC3S200A [inferred] -> DACs / CV S&H |
| 12 | Analog Cymbal | analog circuits | the same |
| FX | Analog FX block (filter, drive) | analog | routed per track (FUNC+FX), controlled by the voice frame [inferred] |

Delay and reverb are digital, probably on CPU1 or the DSP [unresolved].

## 5. Profile (`src/symbols.rs`) for MAIN OS 1.42

Matched by `tools/xmatch.py` from Digitakt 1.53 (and Rytm 1.73 for the MIDI
fields) **[measured: unique masked code match / data votes]**:

| field | Syntakt 1.42 | field | Syntakt 1.42 |
|---|---|---|---|
| `entry` | `0x400004E8` | `ctx_switch` | `0x40000410` |
| `flash_read` | `0x40116D7A` | `task_create` | `0x40001640` |
| `sem_pend` / `pend_b` | `0x40001792` / `0x4000171E` | `sem_post` | `0x400018D4` |
| `queue_send` / `queue_recv` | `0x40001C0E` / `0x40001CBE` | `current_tcb` / `prio_heads` | `0x454E49A8` (18 votes) / `0x454E4968` |
| `panel_diff` | `0x4011391A` | `fb_front` / `fb_back` | `0x402D7110` / `0x402D7114` |
| `frame_sem` | `0x43BBF678` | `display_sem` / `completion_sem` | `0x444BAD58` / `0x444DB2B4` |
| `pend_call` / `depack_copy` | `0x40116384` / `0x40118334` | `abort_loop` | `0x4011AA42` |
| `sd_flag`, `sd_status`, `sd_capacity` | `0x444BA6FC`, `0x444BA72C`, `0x444BA720` | `sd_cmd/data/dma_sem` | `0x444BA748` / `0x444BA740` / `0x444BA738` |
| `uart8_ring_ptr` / `_consume_idx` | `0x405D37C8` / `0x405D37E8` (weak) | `transport_state` | `0x43BD8424` |
| `panel_button_names` | `0x401D7400` | `first_boot_error` | `0x406533FC` |
| `progress_done` / `_total` | `0x444BB1E0` / `0x444BB1DC` | `usb_midi_queue` | `0x44491554` (1 vote) |
| `midi_pool`, `_map`, `_sem` | `0x455146F8`, `0x4551226C`, `0x45512264` | `midi_short_ring` / `_idx` | `0x455122EC` (1) / `0x443DB264` |
| `usb_midi_send` / `_short` | `0x40004A20` / `0x40004974` | `sysex_send` | `0x40106976` |

Not matched, so they need hand work:

- `mainloop`;
- `intro_pit3_isr` / `intro_done` / `intro_park`: the intro differs;
- `mounted`;
- `midi_event`;
- `usb_audio_copy`, `voice_output_mix`, `ob_in_*`;
- `idle_spins`, which are measured at first boot.

There is no `boot_blob`.

The new link addresses a stand-in or the second CPU needs:

- DSP state `0x41bbf448` (6 = ok);
- DSP-link ISR `0x40083034`;
- DSP boot `0x40082c04`;
- mailbox test `0x40083288`;
- the section fetch `0x40116246`;
- the voice frame builder `0x401021b4`;
- the boot flags `0x4029a778`.

A **DSP profile** (section 7) is small:

- entry `0x40001070`;
- init `0x40000db4`;
- block ISR `0x40000934`;
- tick ISR `0x40000442`;
- FPGA config `0x40000706`;
- boot loader `0x40000b92`;
- idle loop near `0x40000e46`.

Section 7 has no META hash of its own, so the profile check would hash section 7
next to MAIN OS.

## 6. Panel keys and LEDs

The OS's button-name table (`0x401D7400`, 48 entries) **has the same order as the
Digitakt's**. Only `SAMPLE` -> `FX` and `SRC` -> `SYN` differ [measured]. The
panel MCU image is identical too, so the Digitakt's key codes and window
geometry should carry over [inferred]. The UI card answer must say type `0x0B`.

The front-panel naming, from the owner's photo [community] and the manual:

| # (name table) | OS name | panel legend | FUNC / secondary |
|---|---|---|---|
| 0-7 | `1`..`8` | trig 1-8, "Digital / Mute" | mute (tracks 1-8) |
| 8-11 | `9`..`12` | trig 9-12, "Analog / Mute" | mute (tracks 9-12) |
| 12-15 | `13`..`16` | trig 13-16 | M1 Retrig, M2 Velocity, M3 Mod A, M4 Mod B |
| 16 | `BANK` | BANK | Mute Mode |
| 17 | `PTN` | PTN | Metronome |
| 18 | `TRK` | TRK (LED below) | Keyboard |
| 19 | `FUNC` | FUNC | |
| 20 | `PATTERN MENU` | `...` | Sound (browser, settings) |
| 21 | `GLOBAL` | gear | Save Proj |
| 22 | `FX` | FX | Routing (analog FX block routing) |
| 23 | `TEMPO` | metronome icon | Tap Tempo |
| 24 | `RECORD` | REC (circle) | Copy |
| 25 | `PLAY` | PLAY (triangle) | Clear |
| 26 | `STOP` | STOP (square) | Paste |
| 27 | `TRIG` | TRIG | Quantize |
| 28 | `SYN` | SYN | Machine |
| 29 | `FLTR` | FLTR | Delay |
| 30 | `AMP` | AMP | Reverb |
| 31 | `LFO` | LFO | Mixer |
| 32 | `YES` | YES | Save Ptn |
| 33 | `NO` | NO | Reload Ptn |
| 34 | `LEFT` | `<` | µTime- |
| 35 | `UP` | `^` | Rtrg/Oct+ |
| 36 | `DOWN` | `v` | Rtrg/Oct- |
| 37 | `RIGHT` | `>` | µTime+ |
| 38 | `PAGE` | PAGE | Fill / Scale |
| 39-46 | `A`..`H` | knobs A-H (encoders) | |
| 47 | `LEVEL/DATA` | Level/Data encoder | Sound Browser (push) |
| - | - | Main Volume | analog pot, not a panel input [inferred] |
| - | - | "Modifier / Setup" | the up/down arrows' FUNC labels |

LEDs: the 16 trig keys are RGB (photo: red, yellow, white, blue). The page LEDs are
1:4, 2:4, 3:4 and 4:4. TRK has a LED. FUNC, the transport keys and the parameter
keys are lit. The exact LED numbering must be measured in the emulator, as for the
Digitakt [inferred].

## 7. Likely first boot obstacles

1. **No profile and no `Device::Syntakt`.** The container parses as is. The UI
   card must answer `0x0B`.
2. **The DSP boot.** With nothing behind DSPI2 and the mailbox:
   - the eDMA ch 29 slave stream never drains;
   - `0x10000000` reads back whatever was written. That passes the memory test,
     but the handshake never sees `'HO' 'HA'`.

   The OS then shows "DSP BOOT FAILURE (TIMEOUT)" or "(CF)" from the main path
   (`0x4000b37a`), and it may stop there [inferred].
3. **The mailbox FPGA window** (CPU1 `0x10000000`, 64 KB) needs a model: shared
   RAM plus the IRQ (vector 68) per audio block. CPU1's eDMA ch 47 reads it as a
   FlexBus source.
4. **The FPGA configuration** (section 1, CPU1 side): the same as the Rytm MKII
   path. INIT_B and DONE must read "configured".
5. **SUPERCAP and MRAM checks**, as on the Rytm (GPIO).
6. **The SSI1 frame size** (8 words): `Io::ssi_requests` already follows CCR.
7. **The analog voice frame** of 500 words: the Rytm's analog model assumes 492.
   A stub that only drains it is enough at first.
8. **The window:** a Digitakt layout with the labels above, and 12 tracks.

## 8. Work estimate and order

**Reusable as is:**

- CPU, JIT, EMAC, INTC, PIT, DTIM, eDMA, UART, eSDHC/eMMC, `ekfs`;
- the ELE3 parser and `fwinfo`;
- the RTOS profile mechanism and `xmatch` (most of the profile is above);
- the panel model (the same MCU image), OLED drawing, the Digitakt key order;
- the SPI-flash container slot, so `flash_read` serves sections 6 and 7 unchanged;
- DIN MIDI and USB MIDI senders, snapshots.

**New work, route A: a high-level DSP stand-in (first boot):**

- `Device::Syntakt`, the profile, UI card `0x0B`, and the window layout: about 1
  session.
- A mailbox FlexBus window with a scripted DSP:
  - answer the memory test, `HO/HA`, a version, then `0xA5A5`;
  - raise IRQ 68 at the block rate;
  - return silence at `+0x1b0`;
  - drain the DSPI2 slave stream.

  About 1 session.
- Voice-frame and analog-control stubs (500 words), and the FPGA and supercap
  GPIOs: small.
- **Estimate: about 2-3 sessions to the main screen with keys.** The digital tracks
  stay silent.

**New work, route B: run the real DSP (digital sound):**

- A **second CPU instance**: `Machine` becomes one CPU plus a bus per core, run in
  lockstep slices on one clock.
  - The DSP bus needs only DDR, SRAM, INTC, DTIM, GPIO, DSPI0/1, eDMA ch 47 and
    FlexBus.
  - Snapshots must carry both cores.
  - The JIT and `jitcheck` already work per core.

  About 2-3 sessions.
- **Boot:** skip the SBF and section 6. Load section 7 at `0x40000400` in the DSP's
  DDR and start at its entry with section 6's DDR state. Alternatively serve the
  real DSPI2 to DSPI0 stream byte for byte (more faithful, a little more work).
- **The XC3S50A configuration** on the DSP's DSPI1 (INIT_B and DONE GPIO): small.
- **Timing:** a DTIN0 frame clock to the DSP and the IRQ back to CPU1, tied to the
  codec frame rate (48 kHz, 64-frame blocks [inferred]).
- **Host cost:** two 250 MHz cores, and the DSP is heavy EMAC code. Expect the
  window to run below real time until the DSP's hot loops are profiled. This is
  the main risk.
- **Estimate: 4-6 sessions after route A to digital sound.**

**Analog voices:** a new model for 3 drums, a cymbal and the FX block (or reuse the
Rytm's analog BD/SD/CY ideas), from a 500-word frame mapped by hand. **3-5
sessions**, independent of route B.

**Recommended order:**

1. the profile, `Device::Syntakt` and the window;
2. route A to the main screen. Capture the real handshake timing and the
   mailbox layout with watchpoints;
3. map the 500-word voice frame and the mailbox parameter block (432 B) statically
   and against the UI;
4. route B, the second core. Start interpreted, then the JIT, and keep `jitcheck`
   green for both images;
5. the analog voice model.

## 9. What the user could supply

- **High-resolution close-ups** of both CPU areas and both FPGAs, with the nearby
  DDR packages. They would settle which DDR belongs to which CPU (128 vs 256 MB),
  and which FPGA sits on the mailbox bus.
- **A video of the startup LED test** (the FUNC + power test mode, TRIG 1) and of
  the "DSP" lines in its log, to name the expected results and LED order.
- **Overbridge channel counts** as the plug-in shows them (tracks and inputs), for
  the USB audio model.
- **A short recording per analog machine** at default settings (BD, SD, cymbal,
  FX block), as reference for the analog model.
- **Nothing is needed for route A.** The OS file is enough.

## 10. Stage 1: the main screen with a stand-in DSP (2026-10-13) [measured unless tagged]

`dtboot fw/Syntakt_OS1.42.syx --instr 4G` (no card, or a `dtcard`-formatted one) ends on
the main screen: `A01 ADRIANA`, 135.0 BPM, track 1's SYN page (TUNE SWEP PNCH DEC / LEV
SMOD TMOD MENV OVER), no popup. 16 tasks; SSI1 runs (about 45 k frames per emulated
second, 32-frame audio blocks at about 1400 a second); the DSP state `0x41bbf448` reads 6
(OK) from about 0.75 G instructions. Branch `syntakt`. What it took:

- **Profile** `syntakt_142` (`src/symbols.rs`): section 5's xmatch fields as they are.
  Filled by hand: `midi_event` = `0x4449152C`, the semaphore the MIDI task (`0x40103e0a`)
  pends on in a booted run, `usb_midi_queue` - 0x28 as on the Rytm (a USB MIDI note goes
  in without a fault; its effect is not checked). Still 0: `mainloop`, the intro hooks,
  `mounted`, `usb_audio_copy`, `voice_output_mix`, `ob_in_*`.
- **DTIM2** must count from DTIN, as on the Rytm MKII: the main task waits on its
  counter (`0x40000f66`) before anything else. Started by the bootstrap state (DTMR 7,
  12.288 MHz [inferred]).
- **The analog voice frame**: the sender (`0x4010223e`) first waits for DSPI1 SR TCF
  (`0x4010222a`); TCF reads set (`Io::forced`), so the 500-word frames go out by eDMA
  ch 15 into PUSHR as plain memory. Accepted, not modelled.
- **Board revision byte** at address 0, FlexBus CS1 (64 KB at 0): bits 0-4 the PCB
  revision r, 5-7 a variant v; the self-test prints `PCBA0046` `'A'+r`, `v+1`
  (`0x40081c7a`). r < 3, or r = 3 with v < 3, gives "TOO OLD HARDWARE FOR THIS OS"; 0x82,
  0x23, 0x43 "OLD HARDWARE. SOUNDS ..." (`0x4000aec0`). The emulator puts 0x63 there
  (PCBA0046D4; the letter from the owner's board photo, the digit [inferred]).
- **Calibration** (`Machine::syntakt_calibration`, `DTEMU_NO_CAL=1` for none: "ANALOG
  CALIBRATION MISSING [-n]" and RUN WARM-UP CALIBRATION?). SPI flash `0x300000`
  (factory copy `0x340000`, checked by `0x4006ec12`), 0x22d40 B, read by `0x4006ecb8`:
  magic `RBOZ`, +4 size 0x22d30, +8 format 1, +12 checksum of the 0x22d30 bytes from
  +16 (sum of byte_i XOR i, `0x4006eb5c`). `0x4006edca` reports calibrated when u16
  +0x10 = 1 and the flag bytes +0x17, +0x18, +0x19, +0xda, +0x14f, +0x15c, +0x1885,
  +0x2fae are all nonzero; a zero one returns -2 .. -9. Boot flag bit 19 forces the
  prompt. The synthetic record sets just those; the values the analog engines would
  read are zero [synthetic; nothing divides by them in 4 G instructions].
- **UI card** 0x0B with the Digitakt's UI firmware versions (2, 0): no panel re-flash.
- **No card** works, and so does `dtcard X.img format`. The OS reads 64 raw slots of the
  card at sector `(i + 8) * 1024` (`0x401133ce` from `0x40082334`, i = 0..63, a header
  checked by `0x401132ec`) into the blob it sends the DSP (below); on a blank or
  `dtcard` card all are empty, a 256-byte blob of 64 zero lengths [the slots' content -
  presumably the DSP's sample or wavetable data - inferred].

### The second CPU and its link (`src/syntakt.rs`)

Read from both sides' code (CPU1 `0x40082c04..0x40083286`, `0x400a2092`; section 7
`0x40000db4..0x4000106e`, `0x40000b20`, `0x4000090e`, `0x40000934`):

- **Kick, CPU1 to DSP**: CPU1 toggles GPIO port A bit 4 (`0x40082f6a`: PPDSDR_A
  `0xEC094018` = 0x10 / PCLRR_A `0xEC094024` = 0xEF). The DSP takes every edge on DTIN0
  (DTIM0 capture on both edges, DTMR `0xc1`, vector 96). During boot its sync routine
  (`0x40000b20`) raises its own IRQ edge and waits for a kick; once running the same
  vector is the block ISR (`0x40000934`), and CPU1's audio interrupt kicks once a block
  while `0x43bdd824` (DSP running) is set (`0x400a2092`). So the "frame clock" on DTIN0
  is CPU1's GPIO, not the FPGA [measured; corrects section 2's inference].
- **IRQ, DSP to CPU1**: the DSP toggles a GPIO (`0x4000090e`); CPU1 takes each edge as
  EPORT IRQ4 (EPPAR `|= 0x300`, both edges; vector 68 at `0x40000110`; ISR `0x40083034`
  clears EPFR bit 4).
- **Boot stream**: CPU1 writes `FF x 8` and section 6 with its byte 1 set to `0F` (1756
  bytes; captured, `DTBOOT_DSP_STREAM=F`) out of DSPI2 in slave mode on eDMA ch 29;
  its done ISR (`0x400831d6`, INTC1 source 21) re-arms ch 29 with `AA`, u32 length and
  section 7 (0x6000 bytes a major loop) and sets `0x41bb32b8`. Port C bit 4 is cleared
  at the start of each attempt (`0x40082c6e`): the DSP's reset [inferred].
- **Handshake** (CPU1 link state `0x41bb32b0`, the DSP's progress word at mailbox +8):
  1. The DSP tests the mailbox (+0/+4 = `AAAAAAAA`/`55555555`, ten reads), writes `'HO'
     'HA'` and its version word (section 7 `0x4004005a`, 0 in 1.42) at +0/+2/+4 (`'NO'
     'NO'` on failure), +8 = 4, and syncs. CPU1 s0: once the section 6 stream is done,
     kick, s1.
  2. The DSP wakes; +0 is not `'B0'` yet, so it starts again (test, `HO HA`, sync). CPU1
     s1: `HO HA` seen, version to `0x4029aacc`, post task `0x40083288`, s2. That task
     writes `AAAAAAAA`/`55555555`, reads +0 back 20 times 100 ms apart, then +2 = 1,
     +0 = `'B0'` (`0x4230`), kick.
  3. The DSP sees `'B0'`: +4 = `0xA5A5`, +8 = 9, sync. CPU1 s2: +4 kept
     (`0x41bbf40c`), +2 = 2, +0xC = 0, kick, s3.
  4. Per kick while +2 = 2 the DSP copies 2 KB from +0x800 to block +0xC of its data
     area (`0x4004f750`) and syncs. CPU1 s3: task `0x40082f90` copies block n of the blob
     at `0x4d84f2c0` (`AA`, u32 length, data from +5; built by `0x40082334` from the card
     slots above) into +0x800, kicks; after the last block, s4.
  5. CPU1 s4: +2 = 3, kick, s5. The DSP: +2 = 3 ends the data; voice init, sync.
  6. CPU1 s5: if the word kept in step 3 is `0xA5A5`, DSP state 6 (OK), `0x43bdd824` = 1;
     s6 then just counts IRQs. The DSP installs its block ISR: per kick it copies 432 B
     of parameters from +0, renders and DMAs 1 KB of audio to +0x1B0 (CPU1 ch 47 takes
     it to SRAM `0x80003d50`), and raises an IRQ edge.
  A timer callback (`0x400828cc`) counts while the state is 0 and gives TIMEOUT (state 2)
  after 18 ticks without a change of s or +8; states 1-5 are retried twice
  (`0x41bbf444`).
- **The stand-in** (`StandIn`) is that DSP flow as a script: it takes the section 6
  stream (ch 29's first major loop; section 7's is not clocked), says `HO HA` 10 M
  instructions later, answers each kick after 4000 instructions as steps 2-6 say, and
  leaves +0x1B0 silent. Timings are [inferred]. Seen in a 4 G run: 24946 kicks, 24947
  IRQs, 2 data blocks (the garbage first one and block 0), DSP state 6 by 0.75 G.

**Interface for stage 2.** `Io::dsp: Option<Box<DspLink>>` holds what both CPUs share:
`mbox` (64 KB, CPU1's 0x10000000 via `Bus::read_slow`/`write_slow`), the kick line
(`set_kick_line`, from CPU1's port A writes), the IRQ (`raise` -> `irq_at` -> EPFR bit 4 in
`Io::service`), and `boot_stream`. A real second core replaces `StandIn`: a second `Cpu`
+ `Bus` (DDR 128 MB, SRAM, INTC, DTIM, GPIO, DSPI0/1, eDMA ch 47, FlexBus CS0 at 0 mapping
the same `mbox`), run in lockstep slices; each CPU1 kick becomes a DTIN0 edge on DTIM0
(capture, vector 96), its port G GPIO toggles (`0xEC09401E` / `0xEC09402A`) call
`raise`, and section 7 is loaded at `0x40000400` directly (or `boot_stream` is served
on its DSPI0 to run section 6 too). Snapshots would have to carry both cores (the link
is in `Io` and is saved; the stand-in's state with it).

### Panel, LEDs and the window

- **Keys** (`panel::SYNTAKT_KEYS`): the Digitakt's wiring; checked on screen by holding
  each from the booted snapshot: BANK (SELECT BANK A-H), PTN (SELECT PTN), MENU `...`
  (SONG MODE popup), GLOBAL (SETTINGS), FX (the FX track), TEMPO (the tempo screen),
  TRIG, FLTR, AMP, LFO (their pages; SYN is the default page), PUSH_D (shows the value,
  34). TRK, FUNC, arrows, YES/NO, PLAY, STOP, PAGE change nothing on screen alone.
- **Encoders**: wire 0..7 move knobs A..H (top row A-D), 8 the track level [measured].
  On the tempo screen five 2-count turns did nothing, five 4-count or three 8-count
  turns moved the BPM one step; `counts_per_detent` is 8 as on the Rytm [inferred].
- **LEDs** (RGB, the Digitakt's protocol; `DTBOOT_LEDS=rgb`), by diffing colours with
  each key held [measured]: trigs 0..15 (at rest T1 dim red: the track; T13 orange, T14..16
  grey), BANK 16, PTN 17, TRIG 18, SYN 19 (white on its page), FLTR 20, AMP 21, LFO 22,
  STOP 24 (white while stopped), TEMPO 25, YES 26, UP 28, DOWN 30 (yellow in the song
  mode popup), MENU 33, FX 35, RECORD 36 (red), PLAY 37 (green); the TRK LED is **39**,
  red in keyboard mode (FUNC + TRK). Pattern pages 43, 42, 41, 40 (RECORD lights 43,
  then PAGE 42). [inferred from the Digitakt's map, no change seen]: PAGE 23, NO 27,
  LEFT 29, RIGHT 31, FUNC 32, GLOBAL 34.
- **Window**: `rytm fw/Syntakt_OS1.42.syx --card C.img` draws the panel after the photo
  (`gui::hw`, `layout_syntakt`): dark panel and keys, trig numbers in their LED colour,
  red second functions, Main Volume pot (not modelled), Level/Data with Sound Browser,
  page LEDs and the TRK LED. MIDI ports `syntakt-emu in/out`. Audio: the codec's DAC
  words 0/1 as on the Digitakt [inferred]; everything is silent for now.

### Overbridge and USB facts

- [measured] MAIN OS carries four USB device descriptors (`0x4033918c..0x403394db`),
  VID 0x1935: PIDs 0xFFFF, **0x001F** (class 0: MIDI), **0x0B4A** and **0x104A** (class
  0xEF composite). The configuration descriptors are not stored whole (built at run
  time [inferred]). Strings `OVERBRIDGE`, `OVERBRIDGE SYNC`, `ANALOG FX BLOCK`.
- [community: Overwitch, `res/devices.json` and `udev/91-usb-elektron.hwdb`, GPL-3]:
  Overbridge PID 0x0B4A (also 0x001E), MIDI 0x001F, Audio/MIDI 0x104A; Overbridge
  protocol version 2, int32 samples. Host to Syntakt 8 channels: Main L/R, Analog FX
  L/R, Track 9..12 Input. Syntakt to host 20: Main L/R, Track 1..12, Analog FX L/R,
  Delay/Reverb L/R, Input L/R. The three PIDs above agree with the image; 0x001E is not
  in MAIN OS; the channel lists are not checked.

### Open after stage 1

- The real DSP: done in stage 2 (section 11).
- The card's 64 DSP data slots (`0x401133ce`): their format, and what a factory card
  holds there.
- The 500-word analog frame and the analog voices; the calibration record's values.
- `mounted`, `mainloop`, the intro hooks, USB audio / Overbridge.

## 11. Stage 2: the second CPU runs section 7 (2026-10-13) [measured unless tagged]

Branch `syntakt2`. `dtboot fw/Syntakt_OS1.42.syx --instr 4G` boots with a real second CPU
by default and ends on the same main screen as stage 1 (the PNG is byte-identical); the
DSP state `0x41bbf448` reads 6 (OK) by 0.9 G instructions (the stand-in's 0.75 G).
`DTEMU_SYNTAKT_DSP=stub` brings the stand-in back.

### How the two CPUs run (`src/machine.rs`, `src/syntakt.rs`)

- **The core**: `Machine::dsp`, a second `Cpu` with its own `Bus` (128 MB DDR, the 64 KB
  SRAM, its own `Io`: INTC, PIT, DTIM, eDMA, GPIO). Block cache over section 7 and the
  compiler on its own thread, as CPU1 has them. Its profile (`DspProfile`) is refused
  unless section 7's sha256 is `daf6451c...` (OS 1.42).
- **The start**: when CPU1's DSPI2 stream of section 6 completes (eDMA ch 29's first major
  loop), the core starts 1 M instructions later [inferred: the loader's time] with section 7
  copied to `0x40000400`, PC at its entry `0x40001070`, SP at section 6's `0x80010000`, SR
  `0x2700`. Section 6 itself (DDR init, the SPI pull of section 7) is not run: CPU1 only
  re-arms ch 29 for 0x6000 bytes of the `AA, length, section 7` stream (`0x400831d6`), so
  how the loader gets the rest is not settled; nothing on CPU1 waits for that stream.
  PCLRR_C bit 4 low (CPU1's reset of the DSP) drops the core; a retry starts a new one.
- **Time**: one clock (instructions at `ips`). CPU1 runs a slice of 10 k instructions
  (`DSP_SLICE`), then the DSP runs to the same clock. CPU1's kicks (port A bit 4 edges)
  are queued with their clocks and the DSP's DTIM0 captures each at its clock (DTER CAP,
  DTCR; vector 96). The DSP's IRQ edges (port G bit 2: `0x4000090e` writes PPDSDR_G = 4 or
  PCLRR_G = 0xFB) reach CPU1's EPORT IRQ4 at the start of its next slice, so at most one
  slice late (50 us at 200 M a second; a block is about 130 k instructions). Deterministic:
  `jitcheck` compares both CPUs, identical from cold boot through 1.5 G and from a playing
  snapshot through 1 G more.
- **The IRQ line starts high** [inferred]: section 7's copy of it (`0x40040054`) starts at 1,
  so its first toggle writes PCLRR_G; with the line taken as low at reset that first edge
  (the one CPU1's handshake waits for) never came.
- **The mailbox**: CPU1's `DspLink::mbox`, moved into the DSP's `DspSide` for each of its
  slices and answered at the DSP's FlexBus CS0 (0..0xFFFF), the CPU's and its eDMA's.
- **The XC3S50A**: INIT_B (PPDSDR_E bit 0) and DONE (PPDSDR_F bit 7) read high, so the
  configuration (`0x40000706`) returns 0 after its 27332 words [inferred: configured at
  once]. PROG_B is port H bit 4.
- **Idle**: `0x4000106e` (`bra.b *` after init: everything runs in the block ISR) and
  `0x40000b34` (the sync routine's wait) are hooked and skip the clock to the next event.
- **Snapshots** carry the second CPU after the main body (DDR, SRAM, registers, its `Io`
  and link end); a snapshot without it still loads. Stage 1 Syntakt snapshots do not (the
  link's fields changed); other devices' snapshots are unchanged.

### What the DSP does, as seen running

- Boot: two 64 MB clears, the FPGA depack and configuration, the mailbox test, `HO HA`
  (+8 = 4), then the handshake of section 10 with CPU1, the card blob's blocks (none on a
  blank card), voice init (+8 = 0xC), and the block ISR `0x40000934` per kick.
- **The block**: CPU1 eDMA ch 47 first writes 432 B of parameters into the mailbox at +0
  (from SRAM `0x80002000`), kicks, and later reads the 1 KB at +0x1B0 into SRAM
  `0x80003d50` (`0x400a47fa`, `0x400a4af4`). The DSP copies +0..+0x1AF to SRAM
  `0x80008000`, renders **track t's 32 samples into `0x80003840 + 128 t`** (signed 32-bit;
  `0x400019d6` per track, `0x40000a78..0x40000aa2`), and DMAs them to +0x1B0 (ch 47). So the
  block is track-major, 8 x 32 words. `--dsp-wav` writes it as 8 channels.
- It raises its IRQ per block only once (the flag `0x40040050` starts at 1 and the ISR
  clears it); CPU1 counts but does not need them.
- **Load**: about 62 % of the DSP's clock is spent in the block ISR, idle or playing (it
  renders all 8 voices every block); the hottest blocks are the oscillator loops
  `0x400029da` (31 %) and `0x400036b2` (9 %), EMAC throughout. That is about 2.5 k
  instructions a frame, near half of a 250 MHz part at 48 kHz [inferred].

### From the DSP to the speakers

CPU1 mixes the 8 tracks (`0x4008f7ea..0x4008fde0`, with 0x80003950, presumably its ADC
input, and the delay/reverb) into two stereo buses and writes them into the codec frame
(SSI1 TX buffer SRAM `0x80000000`, 64 frames of 8 words, eDMA ch 54):

| DAC word | from | what [inferred unless noted] |
|---|---|---|
| 2, 7 | `0x8000dad0` pair (`0x4008fd10`, `0x4008fd18`) | one digital bus |
| 3, 5 | `0x8000dfd0` pair (`0x4008fdca`, `0x4008fdd2`) | the other; the default project's tracks play here |
| 6 | written elsewhere | a noise-like signal in both DSP modes (with the stand-in too), not a speaker signal |
| 0, 1, 4 | | 0 (word 0: a rare single spike) |

FUNC + FX (ROUTING) and trig 1 moved track 1 from words 3/5 to 2/7 [measured], so the
buses are "through the analog FX block" and "straight to the main mix"; which is which,
and which word of a pair is left, are not settled. `Machine::speaker_pair` (the window,
`dtboot --main-wav`) sums them: L = 7 + 5, R = 2 + 3, i.e. the analog FX block passes its
input through [inferred]. The analog tracks and the FX block's own sound are not modelled.
(Stage 3, section 12: 2/7 is the FX block's input, word 6 track 12's digital layer and words
0, 1, 4 those of tracks 11, 10, 9; with the analog model on, `speaker_pair` is the main pair
5/3 alone and the modelled analog side is added to it.)

### Measurements (`dtboot`, OS 1.42, no card)

- **Per track** (MIDI note 60 velocity 100 on channels 1..8 = tracks 1..8, pattern stopped,
  from 4.2 G): every track sounds, peaks -19.8 to -23.6 dBFS of the DSP's 32-bit range
  (tracks 1..8: -21.9, -20.7, -23.6, -21.4, -22.0, -19.8, -22.0, -21.5), each on its own
  channel of `--dsp-wav` and in the speakers' mix.
- **Pitch**: track 2 (SD VINTAGE) for notes 48, 57, 60, 69, 72 measures 130.9, 220.1,
  261.7, 440.3, 523.6 Hz 150 ms after the onset (within 1 cent of the notes; 10 cents sharp
  at 50 ms, its pitch sweep). Track 8 (SY TONE) measures 262.5, 441.3, 523.9, 880.7,
  1045.1 Hz: one octave above the notes within 6 cents (its tuning). On a 48 kHz basis.
- **The default pattern** (PLAY): tracks 1, 2 and 4 play, about -20 dBFS peaks.
- **Speed**: a cold boot to 6 G with PLAY at 4 G takes 9.95 s with the second CPU and 5.11 s
  with the stand-in (302 % vs 587 % of real time on the development host); from a
  playing snapshot 243 %. The window (headless, null audio device) holds SPEED 100 % with 0
  dropouts. Interpreter only, a jitcheck run shows the compiler at about 2x.

### Open after stage 2

- Section 6 and the real `AA, length, section 7` transfer (how the loader gets 383760 B when
  CPU1 serves 0x6000).
- Which bus is the FX block's, left/right within a pair, word 6; the analog FX block (stage 3:
  2/7 feed the block, word 6 is track 12's digital layer; left/right still open).
- The card's 64 DSP data slots (`0x401133ce`): their format (sample or wavetable data for
  the digital machines [inferred]).
- The 500-word analog frame and the analog voices: done in stage 3 (section 12); the
  calibration record's values.
- Overbridge / USB audio (per-track outputs: the DSP block has them all).


## 12. Stage 3: the analog tracks 9-12 and the analog FX block (2026-10-13) [measured unless tagged]

Branch `syntakt3`. `src/stanalog.rs` models the analog side from the 500-word voice frame;
the analog tracks sound in the window, `dtboot --main-wav` (the DAC's main pair plus the
analog side) and `--analog-wav` (the analog side alone). `DTEMU_NO_ANALOG=1` turns it off.

### How it was decoded

- **The frame** is built by `0x401021b4` from the u16 array at SRAM `0x80003160` (500 words)
  into one of the two buffers (`0x443db260` toggles); the sender points eDMA ch 15's source
  at the buffer it sends, so the model reads the buffer the TCD names, once per new frame.
  The per-voice part is written by `0x4009590c` (one call a voice from `0x40095e1c`), the FX
  block's cutoff words by `0x40095ef0..0x40095f40` (`clamp(2 p - 2 (q - 0x4000)) - 0x8000`).
- **Tooling**: `DTEMU_ST_FRAME_TRACE=1` prints the changed words per new frame;
  `DTEMU_ST_FRAME_DUMP=FILE` appends the 500 words every 32 codec frames (the diffs below
  come from those dumps). Each analog machine was selected on track 9 (FUNC + SYN, the list,
  YES) or track 12 and saved as a snapshot; then each knob A..H was turned 20 detents and a
  note played (MIDI channel 9 or 12), and the frames diffed against the untouched run. The
  FLTR, AMP and FX pages and the routing menu (FUNC + FX) the same way.
- **The codec side**: `DTEMU_ST_RX_TONE=k[:AMP]` puts a 440 Hz sine on receive word k,
  `DTEMU_ST_RX_MASK=HEX` zeroes the receive words whose bits are clear.

### The frame map (v = 0, 1, 2 the analog drum voices of tracks 9, 10, 11; v = 3 the cymbal voice of track 12)

| words | what | how found |
|---|---|---|
| v, 5 + v | VCA level x pan, L and R (0x3ea at VOL 110, centre, OVER 0, LEV 51; PAN splits them, VOL and LEV scale both, OVER lowers both; routing to the FX block raises them) | AMP knobs, routing |
| 10 + v | filter cutoff, `(FREQ * 256 - 0x4000) * 2` signed (0x8000 at FREQ 0, clamp 0x7e00), plus key tracking: +0x1528 an octave of the note | FLTR FREQ, notes 36..96 |
| 14 + v | filter resonance (0 at RESO 0, 0x6d2a..0x79a1 at 127 depending on the voice) | FLTR RESO |
| 18 + 4v, 19 + 4v | oscillator 1 / 2 frequency, **8 units a Hz** (note 60 at TUNE 0: 0x082c = 261.5 Hz; an octave doubles it; DET moves osc 2 only; BEND is written into both per block) | notes, TUNE, DET, BEND |
| 20 + 4v, 21 + 4v | oscillator 1 / 2 level: 0x1fff nominal, up to 0x7fff as OVER rises (BAL trades them; BD FM's FM A is osc 2's) | BAL, OVER, FM A |
| 30, 34 | cymbal: colour (NCOL, CH/OH CLASSIC; HH BASIC's TONE) and the level into its overdrive (OVER) | knobs |
| 35, 36 | FX block VCA L/R (0x08a4 at DRIVE 1.00, VOL 110; DRIVE lowers them, PAN splits them) | FX SYN, AMP |
| 37, 38 | FX block cutoff L/R, `2 FREQ * 256 - 0x8000` signed | FX FLTR FREQ, code |
| 39 | FX block resonance, signed: 0xfa71 at RESO 0, 0x02c1 at 127 with LP4; shifted per type (0xeae9, NO1 0xbff1) | FX FLTR RESO, TYPE |
| 80 high / 80 low / 81 high / 81 low | the switch byte of voice 0 / 1 / 2 / 3: filter type in bits 4-6 (drum voices) or 0-2 (cymbal): **1 LP, 2 BP, 3 HP, 4 notch (NO), 5 low shelf (LS), 6 peak (PK)**, 7 high shelf and 0 off [these two inferred by order]; on the drum voices bits 0-3 the oscillator nibble (BD HARD's WAV moves 0101 -> 1101, BD SHARP's 0101 -> 1001); on the cymbal bits 4-5 the banks' mode (0 hats, 1 metallic hats, 2 CB / CY METALLIC, 3 CB METALLIC) and bit 3 set on the CLASSIC / LAB / CY CLASSIC / CB CLASSIC machines | FLTR TYPE on each track, screens |
| 82 bits 8-11 | FX block filter type: **0xe LP4, 0xd LP2, 0xb BP, 0xa HP, 0x8 NO1, 0x9 NO2, 0xc LPN, 0xf OFF** (HP by elimination) | FX FLTR TYPE, screens |
| 82 bit 12, 82 bit 3, 83 bit 10, 83 bit 11 | track 9, 10, 11, 12 routed to the FX block (FUNC + FX) | routing menu |
| 84 + 4v, 104 + 4v | the voice's amp envelope (AMP ATK, HOLD, DEC), two copies (L and R as on the Rytm [inferred]); start command 0x4000 on the trig | AMP knobs |
| 124 + 4v | filter envelope record; its first word is the ENV depth (signed) | FLTR ATK..REL, ENV |
| 140 + 16v .. 155 + 16v | drum voice records: osc 1 pitch, osc 2 pitch, osc 1 amp, osc 2 amp (BD FM: STIM -> pitch 1, FM S -> pitch 2, DEC -> amp 1, FM D -> amp 2; SY DUAL VCO: DEC1/DEC2 -> amp 1/2; BD HARD's SNAP the pitch 2 record's start level, STIM its rates) | knobs |
| 192, 196, 200 | cymbal path records (CY CLASSIC starts all three; CH CLASSIC 196 and 200; CB 196 and 200 with a 20 -> -120 strike, as the Rytm's CB record 6) | knobs |
| 204 | cymbal body record: every cymbal machine starts it at 127 and DEC sets its decay | DEC |
| 208, 212 | FX block amp records L/R (`4000 007f b016 49d9` at rest: open) | FX AMP |
| 216, 220 | FX block filter envelope records, first word the ENV depth | FX FLTR ENV |
| 308..319, 320..331 | cymbal oscillator banks A and B, six 32-bit fields each: the low 22 bits **at 43.62 units a Hz, as on the Rytm** (CH CLASSIC loads the Rytm's 8956..34952, CB CLASSIC 23531 / 34751 = 539.5 / 796.7 Hz); bit 31 is HH BASIC's RST | TUNE, RST |
| 352..367, 368..411 | constants from the calibration record (0x3ab8, 0x1d5c, 0xf0e7 0x1e71; zero calibration here); 368 + 8v drop to 0 on a trig | not used |

The record format is the Rytm MKII's (analog.rs `Record`: command or depth word, start and
target bytes, mode nibble, exponent and mantissa of the rate), and the cymbal banks and their
scale are the Rytm's too: the Syntakt's analog voice hardware is the Rytm's design
[inferred from the shared formats]. The machines (manual appendix A.3, A.4) are not
switches of their own: each machine's code fills the same words differently, so the model
renders the hardware words and every machine follows. UT NOISE and UT IMPULSE leave the
analog oscillators off and change nothing in the frame: they are the digital layer alone.

### The codec

- **Receive** [measured, CPU1 `0x400962ac`, `0x4008ee86`, injected tones]: CPU1 copies the
  8 words into a 20-word frame at `0x80004760` (words 0..3 scaled by EMAC, 4..7 negated and
  shifted), then into `0x80003950` channel by channel: **receive words 0..3 are tracks
  9..12** (a tone on word k passes only while track 9 + k plays), each run through CPU1's
  own copy of its amp envelope (`0x4008e9f0`) and fed to the delay and reverb sends
  (`0x4008f7ea`; a -20 dBFS tone on word 0 comes back in the main pair at about -41 dB RMS,
  PAN does not move it); **4/5 the FX block's return** (processed with the FX track's
  parameters, `a3 + 0x734`) [inferred from the code]; 6/7 the inputs [inferred].
- **Sign**: SRCR (0x48b) sets RXEXT; CPU1 reads words 0..3 as signed 32-bit. The SSI model
  zero-extended them, so every negative sample of a track read as a large positive one and
  the reverb clipped; it now sign-extends on the Syntakt (`Ssi::rx_sign_extend`).
- **DAC** [measured]: words **4, 1, 0, 6 are the digital layers of tracks 9, 10, 11, 12**
  (BD HARD's TICK, SY DUAL VCO's BEND transient, UT NOISE's noise: a note on the track with
  the machine puts them there), so the "noise-like" word 6 of section 11 is track 12's UT
  NOISE. Words 3/5 stay the digital main pair and 2/7 the digital tracks routed to the FX
  block.
- **Routing**: FUNC + FX on track 9 flips word 82 bit 12 and changes words 0/5; the receive
  word's way into the sends does not change. So the analog tracks reach the outputs in
  analog: the VCA L/R to the main bus or to the FX block, as the routing bit says; the main
  outputs sum the DAC's main pair, the analog voices and the FX block [inferred].

### The model (`src/stanalog.rs`) [structure from the map; curves inferred]

- **Drum voice**: oscillator 1 and 2 at their words (8 units/Hz) plus their pitch records
  (25.7 Hz a level, the Rytm model's sweep scale, unsigned levels), each times its amp record
  (logarithmic, 6 dB a 16 levels, -128 silent) and its level word (0x1fff = 1.0); waveform
  from the oscillator nibble's two bits per oscillator (01 sine, 11 triangle, 10 saw, 00
  sine); phases reset by the trig.
- **Cymbal voice**: the running fields of both banks as squares (two running: a cowbell),
  through three paths (records 192/196/200, linear level: band 3.4 kHz, band 7 kHz, high-pass
  9 kHz, shifted by the colour; band 1.2 kHz for the cowbell) plus the body (high-pass 6 kHz,
  band 1.2 kHz for the cowbell), all times the body record 204, times word 34 / 4096.
- **Each voice**: plus its digital layer (DAC word), a tanh overdrive, the analog filter (a
  TPT state-variable: LP, BP, HP, notch, shelves, peak; cutoff on the Rytm's law, a semitone
  a FREQ step, 440 Hz at 60, plus depth x level/127 of the envelope; resonance word / 0x7000),
  AC coupling (5 Hz), x 0.25: that is receive word v; then the VCA (the amp records times
  words v and 5 + v over 2048) to the main bus or the FX block.
- **FX block**: input = DAC 7/2 (L/R [inferred]) + the routed voices; tanh drive; per side a
  state-variable filter (LP4 as two in series) at words 37/38 plus the envelope; the type
  from word 82; times the amp records and words 35/36 over 0x08a4; x 0.25 to the outputs and
  (negated) to receive words 4/5.
- Silent voices and an idle FX block produce exact zeros, so with the analog tracks silent
  the speakers' mix and every DAC word are bit-identical to stage 2's.

### Measurements (dtboot, OS 1.42, no card, from a booted snapshot)

- **Each track sounds** (MIDI note 60 on channels 9..12, the default project, `--analog-wav`):
  track 9 (SY DUAL VCO, TUNE 12, its filter at FREQ 22) -29.1 dBFS peak at 522.2 Hz; track 10
  (TUNE 0, DET -12, a 150 ms amp attack) -41.0 dBFS at 131.1 Hz (oscillator 2, an octave
  down); track 11 (OVER 57, BEND 15) -15.1 dBFS at 289.7 Hz (the bend); track 12 (UT NOISE)
  -31.8 dBFS. The default project's filters are dark; opened, every machine is louder:
- **Every machine** on track 9 (FLTR FREQ turned up) peaks between -18 and -26 dBFS: BD HARD 106.7
  Hz (osc 2's word 0x356 = 106.75 Hz), BD CLASSIC..ACOUSTIC 91..110 Hz, the SDs noisy with
  their tones above 1 kHz, RS -23/-26, UT NOISE / IMPULSE -26, SY DUAL VCO 525.6 Hz, SY RAW
  261.5 Hz, SY CHIP 625 Hz. On track 12: CH/OH CLASSIC -15.7/-18.9, CH/OH METALLIC
  -13.3/-20.0, HH BASIC -18.0, HH LAB -17.6, CY CLASSIC -26.8, CY METALLIC -14.7, CY RIDE
  -30.5, CB CLASSIC -16.5 dBFS at 796.7 Hz (the bank's second field), CB METALLIC -20.5, UT
  NOISE -18.7, UT IMPULSE -21.1.
- **Pitch** (track 10, SY DUAL VCO, BAL to oscillator 1, filter open; 50..250 ms after the
  note, on a 48 kHz basis): notes 36, 48, 57, 60, 69, 72, 84 measure 65.38, 130.75, 219.88,
  261.50, 439.87, 523.12, 1046.38 Hz: -0.7, -0.8, -0.9, -0.8, -0.5, -0.4, -0.2 cents. Track 9
  at TUNE 12.00, note 60: 523.2 Hz (C5).
- **Envelopes follow their knobs** (time to -40 dB from the peak): BD HARD DEC turned down /
  default / up 60 / 70 / 85 ms; CH CLASSIC DEC 20 / 40 / 90 ms; SY DUAL VCO DEC1 85 / 95 ms.
- **FX block** (CH CLASSIC on track 12 routed to it; spectral centroid of the first 100 ms):
  dry 10.3 kHz at -14.7 dBFS; through the block at defaults (LP4, FREQ 127) 9.4 kHz at -21.6;
  FX FREQ down 100 steps 7.6 kHz, down 300 -66 dBFS; type HP 17.0 kHz at -45 dBFS, BP 14.0
  kHz; DRIVE up -15.4 dBFS; FX VOL down -38.7 dBFS.
- **Speed**: the window (headless, `--audio-null`, the demo pattern playing with tracks 11
  and 12 and the digital tracks) holds SPEED 100 % with 0 dropouts; `jitcheck` from a
  playing snapshot identical through 0.5 G (2x).
- **Unchanged**: with tracks 9..12 muted (FUNC + trig 9..12) the stage 2 demo's
  `--main-wav`, `--dsp-wav` and `--wav` are byte-identical to stage 2's; so are the end
  screens of every device.

### Open after stage 3

- Which oscillator switches mean FM, ring modulation and sync (BD FM, BD PLASTIC's TYPE, SY
  DUAL VCO's CNFG, SY RAW's ring modulator; word 80 bit 15, word 81 bits 15, 6, 7 on track
  9's notes): not decoded, so those machines mix their oscillators instead.
- The oscillator nibble's waveform codes beyond sine / triangle, the cymbal mode bits and
  CY RIDE's types; the shelf and peak filters' gain; the hardware's curves and levels
  (overdrive, resonance, the VCA's unity, the FX drive, which word of a DAC pair is left).
- Words 10..13's slow drift after a note (an LFO on the cutoff [inferred]) is followed but
  not explained; the calibration words (352..411) and what a calibrated unit sends there.
- Receive words 6/7 (the inputs) are left at zero.
