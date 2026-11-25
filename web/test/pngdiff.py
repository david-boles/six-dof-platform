"""Compares two PNG screenshots of the same size (8-bit RGB/RGBA, as Chromium
writes them): how many pixels differ, where (bounding box), and their average
color in each image. For checking renders without looking at them.

Usage: python3 pngdiff.py <a.png> <b.png>
Standard library only (PNG = zlib-compressed rows, each with a filter type).
"""
import struct
import sys
import zlib


def decode(path):
    data = open(path, "rb").read()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", f"{path}: not a PNG"
    pos, idat = 8, b""
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        chunk = data[pos + 8:pos + 8 + length]
        if kind == b"IHDR":
            w, h, depth, ctype, _, _, interlace = struct.unpack(">IIBBBBB", chunk)
        elif kind == b"IDAT":
            idat += chunk
        pos += 12 + length
    assert depth == 8 and ctype in (2, 6) and interlace == 0, "unsupported PNG"
    bpp = 3 if ctype == 2 else 4
    raw = zlib.decompress(idat)
    stride = w * bpp
    rows, prev, i = [], bytearray(stride), 0
    for _ in range(h):
        f, line = raw[i], bytearray(raw[i + 1:i + 1 + stride])
        i += 1 + stride
        # Undo the row filter (each byte was stored relative to its neighbors).
        for x in range(stride):
            a = line[x - bpp] if x >= bpp else 0
            b = prev[x]
            c = prev[x - bpp] if x >= bpp else 0
            if f == 1:
                line[x] = (line[x] + a) & 255
            elif f == 2:
                line[x] = (line[x] + b) & 255
            elif f == 3:
                line[x] = (line[x] + (a + b) // 2) & 255
            elif f == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[x] = (line[x] + (a if pa <= pb and pa <= pc else (b if pb <= pc else c))) & 255
        rows.append(line)
        prev = line
    return w, h, [[tuple(r[x * bpp:x * bpp + 3]) for x in range(w)] for r in rows]


def main():
    w1, h1, a = decode(sys.argv[1])
    w2, h2, b = decode(sys.argv[2])
    assert (w1, h1) == (w2, h2), "different sizes"
    diff = [
        (x, y)
        for y in range(h1)
        for x in range(w1)
        if sum(abs(p - q) for p, q in zip(a[y][x], b[y][x])) > 30
    ]
    print(f"{w1}x{h1}: {len(diff)} pixels differ")
    if diff:
        xs, ys = [p[0] for p in diff], [p[1] for p in diff]
        print(f"bounding box x {min(xs)}..{max(xs)}, y {min(ys)}..{max(ys)}")
        for name, img in (("first", a), ("second", b)):
            avg = [sum(img[y][x][k] for x, y in diff) // len(diff) for k in range(3)]
            print(f"average color there in {name}: {avg}")


main()
