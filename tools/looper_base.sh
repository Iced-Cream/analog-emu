#!/bin/bash
# Cold boot a looper/EXT firmware on a card and set it up for Overbridge looping:
# CLOCK SEND and TRANSPORT SEND on (GLOBAL > MIDI CONFIG > SYNC), USB CONFIG =
# OVERBRIDGE. Usage:
#   tools/looper_base.sh BUILT.syx [CARD.img [SNAPSHOT]]
# Defaults: card sections/looper.img, snapshot sections/looper.snap; the .syx is
# copied to fw/AR173_looper.syx. The window then runs with
#   ./target/release/rytm fw/AR173_looper.syx --card sections/looper.img \
#       --snapshot sections/looper.snap --usb-host --ob-link
#
# The settings are part of the working state, which the firmware writes to the
# card's raw state slots only at power-off (docs/RYTM-PORT.md "Power-off and the
# working state"). A card that has had a power-off (rytm --power-off, dtboot
# --power-off) cold-boots with them - also under another firmware build - and a
# card that never had one boots in the defaults. YES toggles CLOCK SEND and
# TRANSPORT SEND, so they are read after the cold boot (stock 1.73's working
# state: 0x407c2d30 clock send, 0x407c2d32 transport send, 0x405c8e4c USB config
# 1 = OVERBRIDGE [measured 2026-10-09, a diff of the state with and without them])
# and only what is off is switched on.
set -e
cd "$(dirname "$0")/.."
SYX=${1:?built .syx}; CARD=${2:-sections/looper.img}; SNAP=${3:-sections/looper.snap}
TMP=$(mktemp -d)
cp "$SYX" fw/AR173_looper.syx
[ -f "$CARD" ] || cp sections/led.img "$CARD"
./target/release/dtboot fw/AR173_looper.syx --instr 4400M --every 4400M --card "$CARD" \
  --press YES@2600M --press NO@3000M --press NO@3400M --press NO@3800M --save $TMP/cold.snap | grep -E "FAULT" || true
byte() { DTEMU_FW=fw/AR173_looper.syx ./target/release/dtpeek $TMP/cold.snap mem $1 1 | sed -n 's/^[0-9a-f]*: \([0-9a-f]*\).*/\1/p'; }
CS=$(byte 407c2d30); TS=$(byte 407c2d32); USB=$(byte 405c8e4c)
echo "after the cold boot: clock send $CS, transport send $TS, usb config $USB"
yes_if_off() { [ "$1" = "01" ] || echo YES; }
KEYS="GLOBAL DOWN DOWN DOWN YES YES DOWN $(yes_if_off $CS) DOWN DOWN $(yes_if_off $TS) NO NO"
KEYS="$KEYS DOWN DOWN DOWN DOWN YES DOWN YES $(yes_if_off $USB) NO NO NO"
P=(); t=4500
for k in $KEYS; do
  P+=(--press "$k@${t}M"); t=$((t+100))
done
./target/release/dtboot fw/AR173_looper.syx --load $TMP/cold.snap --card "$CARD" \
  --instr $((t+600-4400))M --every 100000M --save "$SNAP" "${P[@]}" | grep -E "saved|FAULT"
rm -rf $TMP
