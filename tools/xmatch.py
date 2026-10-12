#!/usr/bin/env python3
"""Find a Digitakt OS 1.53 MAIN OS address's counterpart in another MAIN OS image.

Code: the function's first bytes become a pattern in which every operand that can
differ between builds is a wildcard - 32-bit words that look like guest addresses,
and branch displacements - and the pattern must match exactly once in the target.
It is grown until it does, or gives up.

Data: a global is found through code. Each 32-bit operand in the source image that
equals the address marks a referencing instruction; the code around it is matched in
the target as above and the operand read back at the same offset. Every reference
that matches uniquely votes; the answer is the address they agree on.

    xmatch.py SRC_MAIN_OS TARGET_MAIN_OS code ADDR [ADDR...]
    xmatch.py SRC_MAIN_OS TARGET_MAIN_OS data ADDR [ADDR...]
"""

import re
import struct
import sys
from collections import Counter

BASE = 0x40000400


def is_addr(v):
    return 0x40000000 <= v < 0x50000000 or 0x80000000 <= v < 0x80100000


def pattern(img, off, n):
    """A regex over img[off:off+n] with build-specific fields wildcarded."""
    mask = [False] * n
    i = 0
    while i + 2 <= n:
        w = struct.unpack_from(">H", img, off + i)[0]
        # bra/bsr/bcc: 8-bit displacement in the low byte, or a 16/32-bit one after
        if w >> 12 == 0x6:
            d8 = w & 0xFF
            if d8 == 0x00:
                for k in (2, 3):
                    if i + k < n:
                        mask[i + k] = True
            elif d8 == 0xFF:
                for k in range(2, 6):
                    if i + k < n:
                        mask[i + k] = True
            else:
                mask[i + 1] = True
        # jsr/jmp/lea/pea (d16,pc)
        if w in (0x4EBA, 0x4EFA) or (w & 0xF1FF) == 0x41FA or w == 0x487A:
            for k in (2, 3):
                if i + k < n:
                    mask[i + k] = True
        i += 2
    for j in range(0, n - 3, 2):
        if is_addr(struct.unpack_from(">I", img, off + j)[0]):
            for k in range(4):
                mask[j + k] = True
    out = b"".join(b"." if m else re.escape(bytes([img[off + k]])) for k, m in enumerate(mask))
    return re.compile(out, re.S), mask


def find_code(src, dst, addr, lens=(32, 48, 64, 96, 128, 192, 256)):
    off = addr - BASE
    for n in lens:
        if off + n > len(src):
            break
        rx, mask = pattern(src, off, n)
        if sum(not m for m in mask) < 12:
            continue
        hits = [m.start() for m in rx.finditer(dst) if m.start() % 2 == 0][:3]
        if len(hits) == 1:
            return hits[0] + BASE, n
        if not hits:
            return None, n
    return None, -1


def find_data(src, dst, addr, window=24):
    want = struct.pack(">I", addr)
    votes = Counter()
    refs = 0
    for m in re.finditer(re.escape(want), src):
        o = m.start()
        if o % 2:
            continue
        refs += 1
        start = max(0, o - window) & ~1
        rx, _ = pattern(src, start, min(window * 2, len(src) - start))
        hits = [h.start() for h in rx.finditer(dst) if h.start() % 2 == 0][:2]
        if len(hits) == 1:
            got = struct.unpack_from(">I", dst, hits[0] + (o - start))[0]
            votes[got] += 1
    return votes, refs


def main():
    src = open(sys.argv[1], "rb").read()
    dst = open(sys.argv[2], "rb").read()
    kind = sys.argv[3]
    for a in sys.argv[4:]:
        addr = int(a, 16)
        if kind == "code":
            got, n = find_code(src, dst, addr)
            print(f"0x{addr:08x} -> {'0x%08x' % got if got else 'NONE'}  (pattern {n} B)")
        else:
            votes, refs = find_data(src, dst, addr)
            best = votes.most_common(3)
            print(f"0x{addr:08x} -> {', '.join(f'0x{v:08x} x{c}' for v, c in best) or 'NONE'}  ({refs} refs)")


if __name__ == "__main__":
    main()
