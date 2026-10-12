#!/bin/bash
# Build Braids (Mutable Instruments, MIT) for the Rytm's MCF54418 and link it at
# 0x40400000 for cfbench. Fetches the sources at pinned commits into vendor/
# (git-ignored); nothing from them is committed here.
set -e
D=$(cd "$(dirname "$0")" && pwd); V=$D/vendor; E=$V/eurorack
if [ ! -d "$E/.git" ]; then
  git clone --quiet https://github.com/pichenettes/eurorack.git "$E"
  git -C "$E" checkout --quiet 08460a69a7e1f7a81c5a2abcc7189c9a6b7208d4
  git -C "$E" submodule update --init stmlib
  git -C "$E/stmlib" checkout --quiet e3bd7c9cc00e4364166f9905c0509b6ffd0535ec
fi
cd "$D"; mkdir -p build
FLAGS=(-isystem "$D/shim" -mcpu=54418 -O2 -fno-exceptions -fno-rtti -ffreestanding -fno-threadsafe-statics -fno-use-cxa-atexit -ffunction-sections "-I$E" -DTEST)
OBJS=()
for f in bench.cc libc.c $E/braids/macro_oscillator.cc $E/braids/digital_oscillator.cc $E/braids/analog_oscillator.cc $E/braids/resources.cc $E/stmlib/utils/random.cc; do
  o=build/$(basename "${f%.*}").o
  if [[ $f == *.c ]]; then m68k-elf-gcc -mcpu=54418 -O2 -ffreestanding -fno-builtin -c "$f" -o "$o"
  else m68k-elf-g++ "${FLAGS[@]}" -c "$f" -o "$o"; fi
  OBJS+=("$o")
done
m68k-elf-g++ -mcpu=54418 -nostdlib -T link.ld -Wl,--gc-sections -Wl,-e,bench_init -Wl,-u,bench_render "${OBJS[@]}" -lgcc -o build/bench.elf
m68k-elf-objcopy -O binary build/bench.elf build/bench.bin
m68k-elf-nm build/bench.elf | grep -E " (bench_init|bench_render|out)$"
m68k-elf-size build/bench.elf
