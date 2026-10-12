# Overbridge on the Analog Rytm MKII: channel map, signal flow, and our Teensy host's CPU

Do you have all of the Overbridge audio ins/outs throughout the signal chain mapped out? Is there a rough CPU estimate for the Overbridge
host under load?

Short answer: the 12 + 12 channel map is mapped and checked (firmware disassembly, the emulator,
and a stock Rytm driven from a Mac). Where each channel sits in the analog/digital chain is mostly
measured in the emulator and read from the OS 1.73 code, and only partly confirmed on hardware.
The Teensy's Overbridge host code costs about 3.5 % of its CPU, measured on the Teensy with the USB
bus and the Rytm simulated (section 4); 8 worst-case voices plus the host use 88 %. The first test of
the Teensy as host against the real Rytm has not run.

Tags: **[measured]** seen on hardware (marked "hardware"), in the emulator (rytm-emu), or on the
Teensy; **[code]** read in the OS 1.73 disassembly; **[manual]** from the Elektron manuals;
**[inferred]** reasoned, not shown. All addresses are OS 1.73 (MAIN OS sha256 28d9ef40...) and valid
only for that build. The Overwitch project (GPL-3) documents the same wire format; we read it for
understanding and wrote our own code.

`rytm-teensy` (the Teensy side) and `rytm-mods` (the firmware patches, mod 0024 etc.) are not public
yet; they are cited by file so the claims can be traced once they are.

## 1. USB modes and the Overbridge 2 link

USB CONFIG offers three mutually exclusive modes [manual: Rytm manual, USB CONFIG]:

| mode | VID:PID | audio | MIDI |
|---|---|---|---|
| OVERBRIDGE | 1935:0B48 (older 0010) | 12 in + 12 out, interrupt endpoints | USB MIDI, interface 5 |
| USB MIDI | 1935:0011 | none | yes |
| USB AUDIO/MIDI (class compliant) | 1935:1048 | 2 in + 2 out [manual: L/R pairs only]; descriptors not dumped, so exact channel count [inferred] | yes |

PIDs other than 0B48 come from Overwitch's udev table [community]. Converters are 48 kHz / 24-bit
[manual]. The port is USB 2.0 High Speed [manual]; the MKI is Full Speed.

Overbridge mode, as the stock firmware presents it [measured: hardware, descriptor dump from a Mac,
byte-identical to what the emulator gets from the same firmware]:

| item | value |
|---|---|
| device / configuration | 1935:0B48, high speed, 263-byte configuration descriptor, configuration 1, 6 interfaces |
| audio IN (Rytm -> host) | interface 1, alternate setting 3, interrupt endpoint 0x83, wMaxPacketSize 368, bInterval 1 |
| audio OUT (host -> Rytm) | interface 2, alternate setting 3, interrupt endpoint 0x03, wMaxPacketSize 368, bInterval 1 |
| MIDI | interface 5 (class 1/3), bulk 0x01 / 0x81, 512-byte packets |
| start-up | SET_INTERFACE alt 3 on 1 and 2, CLEAR_FEATURE(HALT) on 0x83 and 0x03: streaming begins at once, no vendor request needed [measured: hardware] |

Block (one 368-byte packet) [measured: hardware and emulator]:

| field | bytes | content |
|---|---|---|
| header magic | 0-1 | 0x0700 Rytm -> host, 0x07FF host -> Rytm |
| frame counter | 2-3 | big-endian, +7 per block |
| private | 4-31 | 28 bytes; zero from the Rytm in our captures, zeros from the host |
| audio | 32-367 | 7 frames x 12 channels x int32 big-endian (full-scale 32-bit words; the 24-bit converter data is left-justified [code]) |

Rates and bandwidth [computed from the above, 48 kHz]:

| quantity | value |
|---|---|
| blocks per second, each way | 48000 / 7 = 6857 |
| payload per direction | 2.52 MB/s (20.2 Mbit/s); the whole packet stream 368 B x 6857 |
| High Speed capacity | 480 Mbit/s; the polling limit is one 368 B packet per 125 us microframe (8000/s), so the Rytm NAKs about 1 poll in 7 |
| clocking | the Rytm is the master; the host answers each block it receives with one block back [measured: hardware, 20 s, one block out per block in, 48000.4 Hz, no counter gaps] |
| MIDI | works alongside the stream (device inquiry answered during a 5-minute stream) [measured: hardware] |

The host-to-Rytm direction is not a mirror image: it has MAIN FX in place of AUDIO IN, see section 2.
Gap bursts after 30-60 s were seen in 120-300 s runs from a loaded Mac in user space; the likely cause
is the Mac, not shown either way [measured, cause inferred].

## 2. Channel maps

### Rytm -> host (12 channels)

Built by the firmware routine at 0x40005c6e from the audio receive buffer at 0x80007440 verbatim
[code], so the channels are what the ColdFire hears *back* from the analog board through the codec
ADC. Every channel therefore sits after the analog stage that produced it.

| ch | content | tapped at |
|---|---|---|
| 0, 1 | MAIN L/R | the analog main output, after the master distortion and compressor, through the ADC [code; model comment, hardware-consistent] |
| 2-9 | the eight voices, in track order: BD, SD, RS/CP, BT, LT, MT/HT, CH/OH, CY/CB | each physical voice after its analog filter and amp, before pan and before the analog mix [code; mod 0013 builds on this and is hardware-verified] |
| 10, 11 | AUDIO IN L/R (sampler/USB input) | the raw ADC input, before any effect [measured: emulator] |

Notes:
- A hit on SD appears on channel 3 only [measured: emulator]. Voices that share a physical voice
  (RS/CP, MT/HT, CH/OH, CY/CB) share a channel.
- The voice order in the receive buffer is a firmware table (physical voice -> slot, 0x401ef1d0),
  not track order; the USB packet puts them in track order [code].
- The Overbridge plug-in's own channel list names 10 outputs plus MAIN; the wire carries 12 [manual
  vs measured: hardware].
- The delay and reverb returns are not in the stock stream; they only reach the stream inside MAIN
  (after the codec round trip) [code].

With rytm-mods 0024-ob-mix (`fxret=1`): channels 10/11 carry the **FX return**, delay + reverb
summed, instead of AUDIO IN [measured: emulator only; has not run on a Rytm]. The stem is read from
the digital delay/reverb buffers before the return mixers, so it is clean (no host term) and it is
ahead of the same effect heard in MAIN by the codec round trip (130 frames in the emulator, the real
latency is unknown). Trade: while it is on, AUDIO IN is not on USB (it still reaches the sampler).
Polarity and level against MAIN are open (section 5).

### Host -> Rytm (12 channels)

| ch | content | enters at |
|---|---|---|
| 0, 1 | MAIN L/R | the POST FX return, DAC words 14/15 (the stereo "MAIN" input) [measured: emulator]. POST is believed to bypass the master distortion and compressor [inferred, strong] |
| 2, 3 | MAIN FX L/R | the PRE FX return, DAC words 12/13, which join the analog mix before the master distortion and compressor [measured: emulator] |
| 4-11 | BD, SD, RS/CP, BT, LT, MT/HT, CH/OH, CY/CB inputs | the voice's digital layer (the DAC channel a sample or FM machine plays on), so before the analog filter, overdrive and amp [code, disassembly of `voice_output_mix` at 0x4010ea2c; hardware: tones sent to one voice come back only on that voice's channel] |

Detail for the voice inputs [code, measured on hardware where stated]:
- The voice to channel table is at 0x401bea90: physical voices 0..7 (BD, BT, SD, LT, CH/OH, MT/HT,
  CY/CB, RS/CP) take channels 4, 7, 5, 8, 10, 9, 11, 6.
- Eight simultaneous tones (200..1000 Hz, one per voice, -26 dBFS) each came back on their own voice's
  return channel with the others at least 66 dB down [measured: hardware, via a Mac host].
- A voice input only sounds while that voice's VCA is open, so the track needs a trig with long
  length [manual; measured: hardware, BT silent until its pad was hit].
- The voice input is scaled by the voice's gain word (0x80007000 + 8v) and the sample layer is
  weighted -0.5 beside it [code; musical meaning inferred]. Our digital machines in rytm-mods zero
  that gain on purpose; an Overbridge/Teensy machine must leave it open.
- The Overbridge manual lists four stereo inputs per plug-in with destinations Main, Main FX or
  Track 1-12 [manual]; the firmware table above is how per-track routing is realised.

## 3. Signal flow of the Rytm MKII

Architecture from the Elektron manual's diagram plus our disassembly. D = digital (ColdFire, EMAC
code), A = analog board.

```
 TRIG / SEQ --> ColdFire: envelopes, LFOs, trig logic (D) --> control frame (CVs, VCA words) --> A
                                                                          |
  per voice v (8 physical voices):                                        v
   [D] sample / FM / noise / Overbridge host input (ch 4-11)  --> DAC word v (digital layer)
   [A] oscillators + percussion generator ---+
                                             +--> overdrive --> multimode filter --> amp (VCA)
   [D] digital layer (DAC word v) -----------+                                          |
                                                                                        +--> ADC: Rytm -> host ch 2-9
                                                                                        |   (post filter + amp, pre pan)
                                                                                        v
                                                                                       pan --> analog mix ---+
                                                                                              (per-voice
   [D] send buffers (delay, reverb) <-- ADC voice words (previous blocks)                      route-to-main)
   [D] delay (line in DRAM), reverb --> return mixers --> DAC                                     |
          PRE  pair (words 12/13) ---------------------------------------------------> into the analog mix
                  also: host MAIN FX (ch 2/3), mod 0025 AUDIO IN copy                            |
                                                                                                 v
   EXT IN L/R (analog, no ADC path to the CPU) -----------------------------------> master DISTORTION
                                                                                      (EXT IN joins here, before comp.)
                                                                                                 |
                                                                                                 v
                                                                                           COMPRESSOR (analog, FX track)
                                                                                                 |
          POST pair (words 14/15): host MAIN (ch 0/1), sampler monitor, metronome  ----> (after the comp.) [inferred]
                                                                                                 |
                                                                                                 v
                                                                                           MAIN OUT L/R --> ADC: Rytm -> host ch 0/1
   AUDIO IN L/R --> ADC raw words 8/9 --> Rytm -> host ch 10/11 (stock) or sampler; no gain control in the firmware
```

Facts behind the diagram:
- Delay and reverb are digital, run on the ColdFire in the audio interrupt, with the returns mixed
  back through the DAC [code; manual]. PRE returns go through the master distortion and compressor,
  POST returns do not [code: routing flag per effect; manual calls the parameters Dist/Comp Routing].
- The compressor is analog and sits after the analog mix and the master distortion; the firmware
  never reads its gain reduction back [code; hardware measurements through EXT IN].
- EXT IN joins after the distortion and before the compressor [measured: hardware].
- There is no input gain for AUDIO IN or EXT IN in the firmware; the only digital gain stock offers
  is USB TO MAIN (0/+6/+12/+18 dB) for USB audio [code, strings].
- Mod 0013 (hardware-verified) takes each voice slot after the analog filter and amp, high-passes it
  and sums it into the PRE return pair; mod 0025 adds a gained copy of AUDIO IN to the same pair so
  that an external source can reach the compressor (emulator only; the stock input reaches it only
  through the analog EXT path).
- Disagreement to note: the emulator's analog model adds POST before the compressor (a guess in its
  comment); the firmware evidence says after [inferred]. This does not change the USB channels.

## 4. The Teensy 4.1 as Overbridge host, and its CPU

What the host does (library `RytmHost`, on PJRC's USBHost_t36, no isochronous needed):
- Enumerates the Rytm (VID 0x1935, high speed, interfaces 1 and 2 at alternate setting 3 with 368-byte
  interrupt endpoints), claims them, sets the alternate settings, clears halt on 0x83 and 0x03.
- Keeps 4 IN and up to 4 OUT interrupt transfers queued, 2 blocks each (14 frames, 0.29 ms):
  about 3400 completions a second each way, all in the USB host interrupt [design].
- Per IN block: checks the magic, the counter (counts gaps) and tracks the per-channel peak across
  7 frames x 12 samples. The Rytm -> host audio is a callback only; it is not used for sound today.
- Per OUT block: for each of the 7 frames writes the 8 synth voices (int16 -> int32) to channels 4-11
  from a FIFO that `loop()` keeps at a target of 256 frames (5.3 ms); an empty FIFO sends silence and
  counts an xrun. One OUT block per IN block, so the Rytm is the clock and no resampler is needed.
- MIDI is the library's `MIDIDevice_BigBuffer` on interface 5 (SysEx reports, clock, sample browsing).
- Memory: the host build costs about 14 KB of ITCM and 33 KB of RAM1 (FIFO 8 KB, transfer buffers
  6 KB) [measured: compile lines, docs/HOST-BRINGUP.md in rytm-teensy]. The full voice build with the
  host: 157.8 KB ITCM in 5 blocks, 45.3 KB RAM1 free for locals/stack [measured: compile].

CPU, using 100 % = the whole 600 MHz core in real time [Teensy 4.1, measured with the cycle counter,
`plaits_bench` and the voice loop's cpu log]:

| load | cost | source |
|---|---|---|
| one Plaits voice, average model | 5.8 % (6-op FM 5.8 %, 3-5 % for most others) | [measured] |
| worst model, MODAL | 10.3-10.8 % | [measured] |
| 8 voices, average mix | about 46 % | [measured / computed] |
| 8 x Plaits MODAL, the worst case | 86 % average, 87-88 % peak block, no dropouts over serial | [measured] |
| 8 x 6-op FM | 38.6 % | [measured] |
| mixed 2 FM, 2 Juno, 2 sampler, Plaits, DigiDrum, 150 BPM | about 39 %, peak 46 % | [measured] |
| 8 x DigiDrum | 11-12 % | [measured] |
| 8 x sample streaming (STRING) | 39 %, card reads add about 1 % (1.5 MB/s) | [measured] |
| trig peaks, one block | 6-op FM patch load up to 18 %, Plaits SPEECH 35 % | [measured] |

These figures are the voice engines measured over the serial bridge, with no USB host code running.

**The host itself: about 3.5 % of the core** [measured on the Teensy, Rytm and USB bus simulated].

A bench sketch (`teensy/obhost_bench` in rytm-teensy) runs the real host code at the real rate
without a Rytm. RytmHost's transfer-completion code and USBHost_t36's queue and followup functions
run unchanged on the USB host interrupt, measured with the cycle counter, 3 runs within 0.3 %.
- Rate: 48000 / 7 = 6857 blocks a second each way; at 2 blocks a transfer that is 3429 IN and 3429 OUT
  completions a second, run as 6857 separate interrupts (the worst case).
- Cost per completion: IN 5270 cycles average (max 5445, about 9 us), OUT 221 cycles; about 1370
  cycles a block. Nearly all of it is per-sample work: the byte-wise big-endian conversion and peak
  meter of incoming blocks (about 2500 cycles, not used by the voice loop, so it can be trimmed) and
  filling outgoing blocks (2417).
- Host path alone: 3.5 % of the 600 MHz core.
- With voices: 8 x Plaits MODAL (the worst case) 84.7 % alone, 88.0 % with the host path, so 12 %
  headroom, no xruns; 7 voices 77.3 %.
- Interrupt jitter: the host build has no audio interrupt (loop() renders into a 5.3 ms FIFO); the
  longest the host path holds off loop() is about 9 us.
- Simulated, not measured: the USB controller (a timer marks transfers done), the interrupt handler's
  periodic-followup loop (copied line for line), and the Rytm itself. Not covered: the controller's
  DMA and bus contention, NAKs and errors, USB MIDI traffic, `Task()`; estimated under 1 % together
  [inferred].
- The first real test of the host build against the Rytm is pending; it logs, every 2 s, the IN/OUT
  rate, FIFO level (min/max), xruns, bad blocks, counter gaps and the longest loop time.
- Latency, host -> Rytm [inferred]: FIFO target 256 frames (5.3 ms) + the OUT transfers in flight (up
  to 56 frames) + the Rytm's own buffer, about 6.5 ms plus the Rytm's; the FIFO target can be lowered
  until xruns appear.

On the Rytm's side, Overbridge costs the ColdFire little in our traces: the emulated firmware streams
at 48 kHz with the whole audio interrupt running at its normal 1500 blocks/s [measured: emulator]. A
real figure for the ColdFire's load with Overbridge on (versus off) is missing.

## 5. Open questions and what is not mapped yet

- The 28 private header bytes, Rytm -> host: all zero in our captures; unknown if any mode uses them.
  Candidate side channel for control data [inferred].
- The FX return stem (0024) on a real Rytm: level against MAIN's FX, polarity (positive stem should
  be in phase with MAIN's FX [inferred]; the emulator shows the opposite sign, which is a model
  assumption), and the real codec round trip (130 frames in the emulator).
- Where POST really joins (before or after the compressor): needs a hardware null test.
- The level an ADC input at line level gives in the AUDIO IN words (no documented full scale).
- Whether host-to-Rytm MAIN uses USB TO MAIN gain, and the exact gain terms (about 0.163 for the host
  term in the PRE mixer, from the code; each is an emulator-checked value, not heard on a Rytm).
- Per-voice digital-layer scaling (-0.5 sample weighting, the gain word at 0x80007000) when Overbridge
  is connected.
- Real CPU of the Teensy host (section 4), and of a Rytm's ColdFire with Overbridge on.
- Not covered: Analog Four MKII's map (same protocol family, different profile), the 1.74 firmware
  (adds an Outbox 8 main-out routing option [release notes]), Digitakt II / Digitone II (3-byte
  samples, 42 channels [community]).
- Hardware checked so far is a stock Rytm MKII on OS 1.73 with a Mac as host, over a USB 3 hub; the
  Teensy host and the 0024/0025 mods have not run on it.
