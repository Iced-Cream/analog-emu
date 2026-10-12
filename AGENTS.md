# AGENTS.md

Guide for AI coding agents (and humans) working in this repository.

## What this is

rytm-emu is an Analog Rytm MKII emulator in Rust. It boots the Rytm's own OS (stock or patched) on emulated hardware so you can see and drive its UI before anything is flashed. The analog voice hardware is stubbed. It is a fork of [rouletrro](https://github.com/Ondrysak/rouletrro), a Digitakt MK1 emulator, which still runs. The plan and evidence are in `docs/RYTM-PORT.md`, and upstream's README is in `docs/UPSTREAM-README.md`.

References to `rytm-mods` and `rytm-teensy` point to companion projects (OS patches and a Teensy sound expander). They are not part of this repository, and nothing here needs them to build or run.

## Setup

- Install Rust (stable, via rustup). On Linux the MIDI and window crates may need ALSA and X11/Wayland dev packages.
- **Firmware is not included and must never be committed.** The user supplies their own Elektron OS file, e.g. `fw/Analog-Rytm_MKII_OS1.73.syx` (the default path) or `fw/Digitakt_OS1.53.syx`. `*.syx`, extracted sections, snapshots and card images are git-ignored. Keep it that way.
- Guest addresses are per firmware build. `src/symbols.rs` refuses a profile unless the MAIN OS hash matches, so only the builds listed there are supported.

## Build and test

```
cargo build --release
cargo test --release            # unit tests + EMAC reference cases (~2 min)
```

Run `cargo test --release` before you finish a change.

## Run

```
./target/release/dtcard rytm.img format                  # create the +Drive card image (once)
./target/release/dtcard rytm.img add sound.wav           # add samples (emulator closed; seen after a cold boot)
./target/release/rytm [FW.syx] --card rytm.img           # the window (default fw: fw/Analog-Rytm_MKII_OS1.73.syx)
./target/release/rytm FW.syx --card rytm.img --power-off # closing the window powers off (no snapshot saved)
./target/release/a4 [FW.syx] [--card a4.img]             # the window as an Analog Four (defaults: fw/Analog-Four_MKII_OS1.56.syx, a4.img)
./target/release/dtboot FW.syx --instr 4G --press YES@3000M   # headless: fixed budget, prints the screen, exits; keys by name
./target/release/dtboot FW.syx --load SNAP --card C.img --power-off 5G   # power off: the OS saves its state to the card
./target/release/fwinfo FW.syx [-o DIR]                  # list/extract the firmware's sections (ELE3, and the MKI units' ELE2)
./target/release/rytm fw/Analog-Four_Analog-Keys_OS1.56.syx --card COPY.img   # the Analog Four MKI (docs/MK1-PORT.md); a blank card works
./target/release/rytm fw/Analog-Rytm_OS1.74.syx --card NEW.img               # the Analog Rytm MKI: a blank card (it lays out its own ekFS v1)
DTEMU_FW=FW.syx ./target/release/dtpeek SNAP mem ADDR LEN   # inspect a snapshot's memory
```

Environment switches:
- `DTEMU_NO_JIT=1` runs the interpreter only, which is the reference.
- `DTEMU_STRICT=1` faults on unmapped pages and SRAM mirrors; `=log` only lists them.

The window creates the virtual MIDI ports `rytm-emu in` and `rytm-emu out` (`a4-emu ...` on an A4). See `README.md` for MIDI, samples, keys and the USB/Overbridge options.
Window mouse: click presses, shift-click latches; on a knob drag/wheel turns, a click pushes, ctrl-drag is push + turn (`gui::KnobDrag`).

## Architecture

- `src/cpu.rs`: ColdFire V4e interpreter plus EMAC. This is the reference.
- `src/fast.rs`: predecoded blocks.
- `src/jit.rs`: the Cranelift block compiler, held bit-exact to the interpreter by `jitcheck`.
- `src/bus.rs`: the memory map and peripheral dispatch.
- `src/io.rs`, `edma.rs`, `esdhc.rs`, `panel.rs`: the peripheral models.
- `src/machine.rs`: boot setup and high-level stand-ins.
- `src/symbols.rs`: the per-build guest-address profiles, and `Device` (Digitakt, Rytm/A4 MKII, Rytm/A4 MKI), which selects the board wiring.
- `src/nor.rs`, `src/mk1.rs`: the MKI units' parallel NOR flash and CS1 memory, and the codec's SPI control port; `io::Ssi0`: the Rytm MKI's second codec port.
- `src/ekfs.rs`: the +Drive filesystem.
- `src/snapshot.rs`: snapshots.
- `src/bin/`: the tools (`rytm`, `a4` (the `rytm` window with A4 defaults), `dtboot`, `dtcard`, `dtpeek`, `fwinfo`, `jitcheck`, `cfbench`, `cfdiff`).
- `tools/`: scripts (disassembler, diff and probe helpers).

## Rules

- Never commit firmware or anything derived from it: `*.syx`, sections, snapshots, card images, dumps.
- Don't distribute Elektron code. Patches are applied by the user to their own OS file.
- Guest addresses are per build. Verify every address in that exact image, and mark findings as measured or inferred.
- The interpreter is the reference. Any JIT change must pass `jitcheck`.
- Nothing here flashes real hardware. Keep it that way.
