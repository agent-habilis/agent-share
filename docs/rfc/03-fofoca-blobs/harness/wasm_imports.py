#!/usr/bin/env python3
"""Dump a .wasm module's import section.

The check RFC 03 needs is "no `import \"env\"` survives" — the same assertion
iroh-blobs' own wasm CI makes. wasm-tools isn't installed here, and the import
section is trivial to parse, so parse it rather than install a toolchain.
"""
import sys


def leb128(buf, i):
    result = 0
    shift = 0
    while True:
        b = buf[i]
        i += 1
        result |= (b & 0x7F) << shift
        if not (b & 0x80):
            return result, i
        shift += 7


KINDS = {0: "func", 1: "table", 2: "mem", 3: "global"}


def main(path):
    buf = open(path, "rb").read()
    assert buf[:4] == b"\0asm", "not a wasm module"
    i = 8  # magic + version
    imports = []
    while i < len(buf):
        sec_id = buf[i]
        i += 1
        size, i = leb128(buf, i)
        end = i + size
        if sec_id == 2:  # import section
            count, i = leb128(buf, i)
            for _ in range(count):
                mlen, i = leb128(buf, i)
                module = buf[i:i + mlen].decode("utf8", "replace")
                i += mlen
                flen, i = leb128(buf, i)
                field = buf[i:i + flen].decode("utf8", "replace")
                i += flen
                kind = buf[i]
                i += 1
                imports.append((module, field, KINDS.get(kind, str(kind))))
                # skip the type payload
                if kind == 0:
                    _, i = leb128(buf, i)
                elif kind == 1:
                    i += 1
                    flags, i = leb128(buf, i)
                    _, i = leb128(buf, i)
                    if flags & 1:
                        _, i = leb128(buf, i)
                elif kind == 2:
                    flags, i = leb128(buf, i)
                    _, i = leb128(buf, i)
                    if flags & 1:
                        _, i = leb128(buf, i)
                elif kind == 3:
                    i += 2
        i = end

    print(f"{path}: {len(buf)} bytes, {len(imports)} imports")
    env = [x for x in imports if x[0] == "env"]
    for m, f, k in imports:
        print(f"  import {m!r} {f!r} ({k})")
    if env:
        print(f"FAIL: {len(env)} 'env' imports present")
        return 1
    print("OK: no 'env' imports")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
