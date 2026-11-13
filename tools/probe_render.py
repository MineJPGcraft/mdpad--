"""Rasterise a diagram primitive dump for a quick visual check.

Not part of the build. The ignored test `diagram::layout::tests::dump_diagram_prims`
writes `target/diagram_probe.json`; point this script at it to get a PNG:

    cargo test --bin mdpad-- dump_diagram_prims -- --ignored
    python tools/probe_render.py target/diagram_probe.json target/diagram_probe.png

Text is drawn with the system font, so glyph shapes approximate the editor's;
the point is to check geometry (bars, axes, cards, rules), not typography.
"""

import json
import sys
from PIL import Image, ImageDraw, ImageFont

SCALE = 3  # supersample so thin rules and joins stay visible


def flat(color):
    """Blend an RGBA fill over white.

    The canvas is RGB, so a translucent fill would otherwise be drawn at full
    saturation and make a 10% quadrant tint look like a solid block.
    """
    if color is None:
        return None
    r, g, b, a = color
    if a >= 255:
        return (r, g, b)
    t = a / 255.0
    return (
        round(r * t + 255 * (1 - t)),
        round(g * t + 255 * (1 - t)),
        round(b * t + 255 * (1 - t)),
    )


def load_font(size):
    candidates = [
        ("C:/Windows/Fonts/msyh.ttc", 0),
        ("C:/Windows/Fonts/cambria.ttc", 1),
        ("C:/Windows/Fonts/cambria.ttc", 0),
        ("C:/Windows/Fonts/seguisym.ttf", 0),
        ("C:/Windows/Fonts/arial.ttf", 0),
    ]
    for path, index in candidates:
        try:
            return ImageFont.truetype(path, max(int(size * SCALE), 4), index=index)
        except Exception:
            continue
    return ImageFont.load_default()


def draw_prim(d, p, band_h, ox, oy):
    """Draw one primitive. `ox`/`oy` already include the baseline offset."""
    k = p["k"]
    if k == "rect":
        x0, y0, x1, y1 = p["r"]
        box = [(ox + x0 * SCALE, oy + y0 * SCALE), (ox + x1 * SCALE, oy + y1 * SCALE)]
        if p["fill"]:
            d.rectangle(box, fill=flat(p["fill"]))
        if p["stroke"]:
            c = flat(p["stroke"]["c"])
            d.rectangle(box, outline=c, width=max(1, int(p["stroke"]["w"] * SCALE)))
    elif k == "line":
        (ax, ay), (bx, by) = p["a"], p["b"]
        c = flat(p["stroke"]["c"])
        d.line(
            [(ox + ax * SCALE, oy + ay * SCALE), (ox + bx * SCALE, oy + by * SCALE)],
            fill=c,
            width=max(1, int(p["stroke"]["w"] * SCALE)),
        )
    elif k == "path":
        pts = [(ox + x * SCALE, oy + y * SCALE) for x, y in p["pts"]]
        if p["fill"]:
            d.polygon(pts, fill=flat(p["fill"]))
        if p["stroke"]:
            c = flat(p["stroke"]["c"])
            w = max(1, int(p["stroke"]["w"] * SCALE))
            if len(pts) > 1:
                d.line(pts, fill=c, width=w, joint="curve")
            if p["closed"] and len(pts) > 2:
                d.line([pts[-1], pts[0]], fill=c, width=w)
    elif k == "circle":
        cx, cy = p["p"]
        r = p["r"] * SCALE
        box = [ox + cx * SCALE - r, oy + cy * SCALE - r, ox + cx * SCALE + r, oy + cy * SCALE + r]
        d.ellipse(box, fill=flat(p["fill"]) if p["fill"] else None)
    elif k == "ellipse":
        cx, cy = p["p"]
        rx, ry = p["r"]
        box = [ox + (cx - rx) * SCALE, oy + (cy - ry) * SCALE, ox + (cx + rx) * SCALE, oy + (cy + ry) * SCALE]
        d.ellipse(box, fill=flat(p["fill"]) if p["fill"] else None)
    elif k == "text":
        w, h = p["w"] * SCALE, p["h"] * SCALE
        fx, fy = p["af"]
        x = ox + p["pos"][0] * SCALE - fx * w
        y = oy + p["pos"][1] * SCALE - fy * h
        # Faint box: the metric the layout actually used (ApproxMeasure).
        d.rectangle([x, y, x + w, y + h], outline=(228, 228, 238))
        d.text((x, y), p["s"], font=load_font(p["size"]), fill=flat(p["c"]))


def main():
    data = json.load(open(sys.argv[1], "r", encoding="utf-8"))
    rows = []
    for entry in data:
        w, h = entry["size"]
        rows.append(entry)

    pad = 14
    label_w = 200
    total_h = sum((r["size"][1] + pad * 2) * SCALE for r in rows) + pad * SCALE * 2
    max_w = max(r["size"][0] for r in rows) + pad * 2
    img = Image.new("RGB", (label_w * SCALE + int(max_w * SCALE), int(total_h)), (255, 255, 255))
    d = ImageDraw.Draw(img)
    label_font = load_font(9)

    oy = pad * SCALE
    for entry in rows:
        w, h = entry["size"]
        band_h = (h + pad * 2) * SCALE
        ox = label_w * SCALE + pad * SCALE
        d.text((6, oy + 4), entry["expr"], font=label_font, fill=(30, 30, 30))
        # baseline marker
        by = oy + (pad + entry["baseline"]) * SCALE
        d.line([(label_w * SCALE, by), (img.width - 2, by)], fill=(235, 200, 200))
        for p in entry["prims"]:
            draw_prim(d, p, band_h, ox, oy + pad * SCALE)
        d.line([(0, oy), (img.width, oy)], fill=(240, 240, 240))
        oy += band_h

    img = img.resize((img.width // SCALE, img.height // SCALE), Image.LANCZOS)
    out = sys.argv[2]
    img.save(out)
    print(f"wrote {out} ({img.width}x{img.height})")

    # Per-expression crops, so single rows can be inspected at full size.
    import os

    os.makedirs("target/rows", exist_ok=True)
    top = 0
    for i, entry in enumerate(rows):
        band = (entry["size"][1] + 2 * pad)
        crop = img.crop((0, top, img.width, min(top + band, img.height)))
        crop = crop.resize((crop.width * 3, crop.height * 3), Image.LANCZOS)
        crop.save(f"target/rows/row_{i:02d}.png")
        top += band
    print(f"wrote {len(rows)} row crops")


if __name__ == "__main__":
    main()
