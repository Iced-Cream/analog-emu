#!/usr/bin/env bash
# Turn one encoder by DETENTS from the main-page snapshot, strike the BD pad,
# and print BD's control-frame fields a few blocks after the hit.
#   tools/bdprobe.sh ENC DETENTS [DELAY_INSTR]
set -euo pipefail
cd "$(dirname "$0")/.."
export DTEMU_FW=${DTEMU_FW:-fw/Analog-Rytm_MKII_OS1.73.syx}
enc=$1; det=$2; delay=${3:-600k}
turns=()
n=$(( det < 0 ? -det : det )); sign=$(( det < 0 ? -1 : 1 ))
# 8 counts per detent, sent 16 counts at a time, 2M instructions apart
counts=$(( n * 8 )); t=4400
while [ $counts -gt 0 ]; do c=$(( counts > 16 ? 16 : counts )); turns+=(--turn "$enc:$(( c * sign ))@${t}Mx1/1M"); t=$((t+2)); counts=$((counts-c)); done
./target/release/dtboot "$DTEMU_FW" --load sections/led0.snap --instr $((t-4300+60))M --every 9G --card sections/led.img ${turns[@]+"${turns[@]}"} --press BD@$((t+40))M:20M --save sections/probe.snap >/dev/null 2>&1
./target/release/dtboot "$DTEMU_FW" --load sections/probe.snap --instr "$delay" --every 9G --card sections/led.img --save sections/probe2.snap >/dev/null 2>&1
python3 - <<'PY'
import subprocess,struct
out=subprocess.run(["./target/release/dtpeek","sections/probe2.snap","mem","800063c0","984"],capture_output=True,text=True).stdout
f=bytearray()
for l in out.splitlines():
    if ":" in l: f+=bytes.fromhex(l.split(":",1)[1].replace(" ",""))
w=lambda o: struct.unpack_from(">H",f,o)[0]
def rec(o):
    r=f[o:o+8]; v=struct.unpack(">I",r[4:8])[0]
    return f"d{struct.unpack('>h',r[0:2])[0]:6} s{r[2]-256 if r[2]>127 else r[2]:4} t{r[3]-256 if r[3]>127 else r[3]:4} e{(v>>16)&15:2} m{v&0xffff:5}"
print(f"T1 {w(90):5} T2 {w(92):5} L1 {w(94):5} L2 {w(96):5} | p0 {rec(360)} | p1 {rec(368)} | a2 {rec(376)} | a3 {rec(384)}")
PY
