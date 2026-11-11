"""Cut a PNG to its top-left <width> x <height> (no dependencies; 8-bit RGB/RGBA as Chromium writes)."""
import struct
import sys
import zlib

path, width, height = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
data = open(path, "rb").read()
assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
pos, chunks = 8, []
while pos < len(data):
    n, kind = struct.unpack(">I4s", data[pos : pos + 8])
    chunks.append((kind, data[pos + 8 : pos + 8 + n]))
    pos += 12 + n
ihdr = chunks[0][1]
w, h, depth, color = struct.unpack(">IIBB", ihdr[:10])
assert depth == 8 and color in (2, 6), f"unsupported PNG (depth {depth}, color {color})"
px = 4 if color == 6 else 3
width, height = min(w, width), min(h, height)
if (w, h) == (width, height):
    sys.exit(0)
row = 1 + w * px  # each row starts with its filter byte
raw = zlib.decompress(b"".join(c for k, c in chunks if k == b"IDAT"))

# Undo the row filters (cutting changes each pixel's neighbours), then store rows unfiltered.
prev, rows = bytearray(w * px), []
for y in range(height):
    f, line = raw[y * row], bytearray(raw[y * row + 1 : (y + 1) * row])
    for i in range(len(line)):
        a = line[i - px] if i >= px else 0
        b = prev[i]
        c = prev[i - px] if i >= px else 0
        if f == 1:
            line[i] = (line[i] + a) & 255
        elif f == 2:
            line[i] = (line[i] + b) & 255
        elif f == 3:
            line[i] = (line[i] + (a + b) // 2) & 255
        elif f == 4:
            p = a + b - c
            pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
            line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
    rows.append(b"\0" + bytes(line[: width * px]))
    prev = line


def chunk(kind, body):
    return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))


out = data[:8] + chunk(b"IHDR", struct.pack(">II", width, height) + ihdr[8:])
out += b"".join(chunk(k, c) for k, c in chunks[1:] if k not in (b"IDAT", b"IEND"))
out += chunk(b"IDAT", zlib.compress(b"".join(rows), 9)) + chunk(b"IEND", b"")
open(path, "wb").write(out)
