"""Dump the import/export sections of a WASM module.

Used to check, from the outside, that the sandbox guest imports only what the
allowlist permits. Reading the import section is the whole basis of the
capability check, so being able to see it without trusting our own code is
worth the twenty lines.

Usage: python wasm_sections.py <module.wasm>
"""

import sys


def uleb(data, i):
    result = 0
    shift = 0
    while True:
        byte = data[i]
        i += 1
        result |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return result, i
        shift += 7


def name(data, i):
    length, i = uleb(data, i)
    return data[i : i + length].decode("utf-8", "replace"), i + length


KIND = {0: "func", 1: "table", 2: "memory", 3: "global"}


def sections(data):
    i = 8  # magic + version
    while i < len(data):
        sid = data[i]
        i += 1
        size, i = uleb(data, i)
        yield sid, data[i : i + size]
        i += size


def main(path):
    with open(path, "rb") as handle:
        data = handle.read()
    if data[:4] != b"\0asm":
        raise SystemExit("not a wasm module")

    for sid, body in sections(data):
        if sid == 2:
            count, i = uleb(body, 0)
            print(f"imports ({count}):")
            for _ in range(count):
                module, i = name(body, i)
                field, i = name(body, i)
                kind = body[i]
                i += 1
                if kind == 0:
                    _, i = uleb(body, i)
                elif kind == 1:
                    i += 1
                    flags = body[i]
                    i += 1
                    _, i = uleb(body, i)
                    if flags & 1:
                        _, i = uleb(body, i)
                elif kind == 2:
                    flags = body[i]
                    i += 1
                    _, i = uleb(body, i)
                    if flags & 1:
                        _, i = uleb(body, i)
                elif kind == 3:
                    i += 2
                print(f"  {module}.{field}  [{KIND.get(kind, kind)}]")
        elif sid == 7:
            count, i = uleb(body, 0)
            names = []
            for _ in range(count):
                field, i = name(body, i)
                kind = body[i]
                i += 1
                _, i = uleb(body, i)
                names.append(f"{field}[{KIND.get(kind, kind)}]")
            print(f"exports ({count}): {' '.join(sorted(names))}")
        elif sid == 5:
            print("memory section present")
        elif sid == 11:
            print("data section present")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit(__doc__.strip().splitlines()[-1])
    main(sys.argv[1])
