# Generate ShiDrive app icons (ico/png) with pure Python (no PIL).
import math, struct, zlib, os

def clamp(v): return max(0, min(255, int(v)))

def draw(size):
    # Steering wheel on dark rounded square
    px = [[(0, 0, 0, 0)] * size for _ in range(size)]
    s = size
    # background rounded rect
    r = int(s * 0.22)
    bg = (24, 26, 33, 255)
    acc = (66, 165, 245, 255)      # blue
    acc2 = (129, 199, 132, 255)    # green accent dot
    ring = (236, 239, 244, 255)
    for y in range(s):
        for x in range(s):
            # rounded rect test
            cx = min(max(x, r), s - 1 - r)
            cy = min(max(y, r), s - 1 - r)
            if (x - cx) ** 2 + (y - cy) ** 2 <= r * r:
                px[y][x] = bg
    c = s / 2
    R = s * 0.36          # wheel outer radius
    rw = s * 0.085        # ring width
    hub = s * 0.10
    sw = max(2, int(s * 0.05))  # spoke width
    for y in range(s):
        for x in range(s):
            dx, dy = x + 0.5 - c, y + 0.5 - c
            d = math.hypot(dx, dy)
            if abs(d - R) <= rw / 2:
                px[y][x] = ring
            elif d <= hub:
                px[y][x] = ring
            else:
                # three spokes: down, upper-left, upper-right
                ang = math.atan2(dy, dx)  # -pi..pi, 0 = right, pi/2 = down
                spokes = [math.pi / 2, math.pi + math.pi / 6, -math.pi / 6]
                for a in spokes:
                    # distance from point to spoke line segment
                    ex, ey = math.cos(a) * (R - rw), math.sin(a) * (R - rw)
                    t = max(0.0, min(1.0, (dx * ex + dy * ey) / (ex * ex + ey * ey)))
                    dseg = math.hypot(dx - ex * t, dy - ey * t)
                    if dseg <= sw / 2 and d <= R:
                        px[y][x] = ring
    # accent dot top-right of ring (like a gauge light)
    ad = s * 0.30
    ax, ay = c + ad * 0.72, c - ad * 0.72
    ar = s * 0.055
    for y in range(s):
        for x in range(s):
            if math.hypot(x + 0.5 - ax, y + 0.5 - ay) <= ar:
                px[y][x] = acc2
    return px

def to_bgra(px):
    h, w = len(px), len(px[0])
    row = b""
    rows = []
    for y in range(h - 1, -1, -1):
        row = b""
        for x in range(w):
            r, g, b, a = px[y][x]
            row += bytes((b, g, r, a))
        rows.append(row)
    return b"".join(rows)

def bmp_entry(size, px):
    data = to_bgra(px)
    and_stride = ((size + 31) // 32) * 4
    and_data = b"\x00" * (and_stride * size)
    header = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, len(data) + len(and_data), 0, 0, 0, 0)
    return header + data + and_data

def make_ico(path, sizes):
    images = [bmp_entry(sz, draw(sz)) for sz in sizes]
    with open(path, "wb") as f:
        f.write(struct.pack("<HHH", 0, 1, len(images)))
        offset = 6 + 16 * len(images)
        for sz, img in zip(sizes, images):
            b = 0 if sz >= 256 else sz
            f.write(struct.pack("<BBBBHHII", b, b, 0, 0, 1, 32, len(img), offset))
            offset += len(img)
        for img in images:
            f.write(img)
    print("wrote", path)

def make_png(path, size):
    px = draw(size)
    raw = b""
    for y in range(size):
        raw += b"\x00"
        for x in range(size):
            r, g, b, a = px[y][x]
            raw += bytes((r, g, b, a))
    def chunk(typ, data):
        c = struct.pack(">I", len(data)) + typ + data
        return c + struct.pack(">I", zlib.crc32(typ + data) & 0xFFFFFFFF)
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)
    print("wrote", path)

out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "icons")
os.makedirs(out, exist_ok=True)
make_ico(os.path.join(out, "icon.ico"), [256, 64, 48, 32, 16])
make_png(os.path.join(out, "icon.png"), 256)
make_png(os.path.join(out, "32x32.png"), 32)
make_png(os.path.join(out, "128x128.png"), 128)
