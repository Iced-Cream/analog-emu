#!/usr/bin/env python3
"""The Elektron USB SysEx protocol that Transfer and elektroid speak, for tests.

Framing as in elektroid (github.com/dagargo/elektroid, src/connectors/elektron.c):
F0 00 20 3C 10 00, then the message 7-bit packed (each group of up to 7 bytes is
preceded by a byte holding their top bits, first byte's in bit 6), then F7. A
message is [seq hi, seq lo, ?, ?, command, args...]; the reply carries the same
sequence number and command | 0x80.

    ekproto.py encode SEQ HEXCMD [HEXARGS]     -> request bytes (hex) on stdout
    ekproto.py decode FILE                     -> each SysEx in FILE, decoded
"""
import sys

HEADER = bytes([0xF0, 0x00, 0x20, 0x3C, 0x10, 0x00])


def encode(msg: bytes) -> bytes:
    out = bytearray()
    for j in range(0, len(msg), 7):
        chunk = msg[j:j + 7]
        top = 0
        for k in range(7):
            top <<= 1
            if k < len(chunk) and chunk[k] & 0x80:
                top |= 1
        out.append(top)
        out += bytes(b & 0x7F for b in chunk)
    return HEADER + bytes(out) + b"\xF7"


def decode(sysex: bytes) -> bytes:
    src = sysex[len(HEADER):-1]
    out = bytearray()
    for i in range(0, len(src), 8):
        top = src[i]
        for k, b in enumerate(src[i + 1:i + 8]):
            out.append(b | (0x80 if top & (0x40 >> k) else 0))
    return bytes(out)


def request(seq: int, cmd: bytes) -> bytes:
    return encode(bytes([seq >> 8, seq & 0xFF, 0, 0]) + cmd)


def split(data: bytes):
    i = 0
    while (s := data.find(b"\xF0", i)) != -1:
        e = data.find(b"\xF7", s)
        if e == -1:
            break
        yield data[s:e + 1]
        i = e + 1


if __name__ == "__main__":
    if sys.argv[1] == "encode":
        cmd = bytes.fromhex(sys.argv[3] + (sys.argv[4] if len(sys.argv) > 4 else ""))
        print(request(int(sys.argv[2]), cmd).hex())
    else:
        for sx in split(open(sys.argv[2], "rb").read()):
            if sx.startswith(HEADER):
                m = decode(sx)
                print(f"seq {m[0] << 8 | m[1]} cmd {m[4]:#04x} ({len(m)} B): {m[5:].hex(' ')}")
                print("   text:", "".join(chr(c) if 32 <= c < 127 else "." for c in m[5:]))
            else:
                print("other sysex:", sx.hex(" "))
