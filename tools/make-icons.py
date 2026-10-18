#!/usr/bin/env python3
"""Draw CDeckJ's icon and Steam artwork into assets/icon/.

The mark riffs on the Steam Deck's circle-and-arc: the circle is a jog wheel (with the
amber position marker from the app's jog), and the arc is a sweep of waveform bars in the
two deck colours. Needs: pip install cairosvg pillow
"""
import io, math, os, sys
import cairosvg
from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
OUT = os.path.join(ROOT, "assets", "icon")
FONT = os.path.join(ROOT, "assets", "fonts", "BarlowCondensed-SemiBold.ttf")

# App palette (src/ui/theme.rs).
CHROME, SURFACE, RAISED, INSET, BORDER = "#101318", "#161A20", "#1F242C", "#0A0C10", "#2A3039"
INK, ACCENT, SKY, VIOLET, CUE = "#E4E9F0", "#4D9CFF", "#5FC4F5", "#B497FF", "#F2A13B"


def lerp_hex(a, b, t):
    a, b = [int(a[i:i + 2], 16) for i in (1, 3, 5)], [int(b[i:i + 2], 16) for i in (1, 3, 5)]
    return "#" + "".join(f"{round(x + (y - x) * t):02X}" for x, y in zip(a, b))


def bar_colour(t):
    # Deck 1 sky blue at the top, through the accent, to deck 2 violet at the bottom.
    return lerp_hex(SKY, ACCENT, t / 0.5) if t < 0.5 else lerp_hex(ACCENT, VIOLET, (t - 0.5) / 0.5)


def wave_amp(t, i):
    """Bar height (0..1) for bar i at t along the arc: a kick on every fourth bar over a
    steady body, tapering to rounded ends."""
    rnd = ((i * 2654435761) % 1000) / 1000.0  # fixed pseudo-random texture
    kick = [1.0, 0.38, 0.62, 0.3][i % 4]
    taper = math.sin(math.pi * (0.03 + 0.94 * t)) ** 0.5
    return (0.18 + 0.66 * kick + 0.16 * rnd) * taper


def mark(cx, cy, s=1.0, small=False, style="blue"):
    """The mark, centred on the jog at (cx, cy), scaled by s (1.0 fits a 256 tile).

    A solid jog wheel and, around its right side, a mirrored waveform bent into an arc:
    the Steam Deck's circle-and-arc, made of DJ parts."""
    out = []
    R = 56 * s  # jog radius
    rc = R * 1.68  # waveform centre line
    half = R * 0.5  # peak half-thickness
    span = math.radians(184)
    top, bot = cy - rc - half, cy + rc + half
    out.append(f'<linearGradient id="wave" gradientUnits="userSpaceOnUse" x1="0" y1="{top:.1f}" x2="0" y2="{bot:.1f}">'
               f'<stop offset="0.1" stop-color="{SKY}"/><stop offset="0.5" stop-color="{ACCENT}"/>'
               f'<stop offset="0.9" stop-color="{VIOLET}"/></linearGradient>')
    if small:
        n = 60
        outer, inner = [], []
        for k in range(n + 1):
            t = k / n
            a = -span / 2 + span * t
            amp = math.sin(math.pi * t) ** 0.45 * 0.85
            outer.append((cx + (rc + half * amp) * math.cos(a), cy + (rc + half * amp) * math.sin(a)))
            inner.append((cx + (rc - half * amp) * math.cos(a), cy + (rc - half * amp) * math.sin(a)))
        d = "M" + " L".join(f"{x:.2f},{y:.2f}" for x, y in outer + inner[::-1]) + " Z"
        out.append(f'<path d="{d}" fill="url(#wave)"/>')
    else:
        # Mirrored bars, like the app's overview waveforms bent around the jog.
        n = 40
        pitch = span / n
        for k in range(n):
            t = (k + 0.5) / n
            a = -span / 2 + pitch * (k + 0.5)
            w = pitch * 0.3
            amp = wave_amp(t, k)
            ro, ri = rc + half * amp, rc - half * amp
            q = [(ro, a - w), (ro, a + w), (ri, a + w * ri / ro), (ri, a - w * ri / ro)]
            pts = " ".join(f"{cx + r * math.cos(b):.2f},{cy + r * math.sin(b):.2f}" for r, b in q)
            out.append(f'<polygon points="{pts}" fill="url(#wave)"/>')
    if style == "blue":
        out.append(f'<circle cx="{cx}" cy="{cy}" r="{R:.2f}" fill="url(#disc)"/>')
        if not small:
            for k, rr in enumerate([0.9, 0.8, 0.7, 0.6]):
                out.append(f'<circle cx="{cx}" cy="{cy}" r="{R * rr:.2f}" fill="none" stroke="#0A1A33" '
                           f'stroke-opacity="{0.22 - k * 0.03:.3f}" stroke-width="{1.5 * s:.2f}"/>')
        out.append(f'<circle cx="{cx}" cy="{cy}" r="{R * 0.36:.2f}" fill="{CHROME}"/>')
        out.append(f'<circle cx="{cx}" cy="{cy}" r="{R * 0.1:.2f}" fill="{INK}"/>')
    else:
        out.append(f'<circle cx="{cx}" cy="{cy}" r="{R:.2f}" fill="url(#platter)"/>')
        out.append(f'<circle cx="{cx}" cy="{cy}" r="{R - 3.2 * s:.2f}" fill="none" stroke="{ACCENT}" stroke-width="{6 * s:.2f}"/>')
        out.append(f'<circle cx="{cx}" cy="{cy}" r="{R * 0.36:.2f}" fill="url(#label)"/>')
        out.append(f'<circle cx="{cx}" cy="{cy}" r="{R * 0.1:.2f}" fill="{INK}"/>')
    mw, mh = (9 if small else 6.5) * s, (17 if small else 14) * s
    my = cy - R + (3 if small else 5) * s
    out.append(f'<rect x="{cx - mw / 2:.2f}" y="{my:.2f}" width="{mw:.2f}" height="{mh:.2f}" rx="{mw / 2:.2f}" fill="{CUE}"/>')
    return "\n".join(out)


DEFS = f'''<defs>
  <linearGradient id="tile" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="#1A1F27"/><stop offset="1" stop-color="{CHROME}"/>
  </linearGradient>
  <radialGradient id="platter" cx="0.42" cy="0.36" r="0.75">
    <stop offset="0" stop-color="#232A34"/><stop offset="1" stop-color="{INSET}"/>
  </radialGradient>
  <radialGradient id="label" cx="0.38" cy="0.32" r="0.8">
    <stop offset="0" stop-color="#7DB8FF"/><stop offset="1" stop-color="#2F6FD0"/>
  </radialGradient>
  <radialGradient id="disc" cx="0.38" cy="0.3" r="0.85">
    <stop offset="0" stop-color="#8CC4FF"/><stop offset="0.55" stop-color="{ACCENT}"/><stop offset="1" stop-color="#2D68C4"/>
  </radialGradient>
  <radialGradient id="glow" cx="0.5" cy="0.5" r="0.5">
    <stop offset="0" stop-color="{ACCENT}" stop-opacity="0.22"/><stop offset="1" stop-color="{ACCENT}" stop-opacity="0"/>
  </radialGradient>
</defs>'''


def icon_svg(small=False, style="blue"):
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256">
{DEFS}
<rect x="0" y="0" width="256" height="256" rx="56" fill="url(#tile)"/>
<rect x="1" y="1" width="254" height="254" rx="55" fill="none" stroke="#FFFFFF" stroke-opacity="0.06" stroke-width="2"/>
{mark(96, 128, 1.0, small, style)}
</svg>'''


def render(svg, w, h=None):
    png = cairosvg.svg2png(bytestring=svg.encode(), output_width=w, output_height=h or w)
    return Image.open(io.BytesIO(png)).convert("RGBA")


def save(img, name):
    path = os.path.join(OUT, name)
    img.save(path, optimize=True)
    print(f"{name:28s} {img.size[0]}x{img.size[1]}  {os.path.getsize(path) // 1024} KB")


def wordmark(draw, xy, size, anchor="ls"):
    f = ImageFont.truetype(FONT, size)
    x, y = xy
    draw.text((x, y), "CDeck", font=f, fill=INK, anchor=anchor)
    w = draw.textlength("CDeck", font=f) if anchor.startswith("l") else 0
    draw.text((x + w, y), "J", font=f, fill=ACCENT, anchor=anchor)


def scene(w, h, cx, cy, s, glow=True, bg=True):
    """Mark on the app's background (no tile), as an RGBA image."""
    body = mark(cx, cy, s)
    back = f'<rect width="{w}" height="{h}" fill="url(#bgv)"/>' if bg else ""
    halo = f'<circle cx="{cx}" cy="{cy}" r="{150 * s}" fill="url(#glow)"/>' if glow else ""
    svg = f'''<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">
{DEFS.replace("</defs>", f'<linearGradient id="bgv" x1="0" y1="0" x2="0.3" y2="1"><stop offset="0" stop-color="#1A1F27"/><stop offset="1" stop-color="{INSET}"/></linearGradient></defs>')}
{back}{halo}{body}</svg>'''
    return render(svg, w, h)


def main():
    os.makedirs(OUT, exist_ok=True)
    big = icon_svg()
    open(os.path.join(OUT, "cdeckj.svg"), "w").write(big)
    for size in (512, 256, 128, 64):
        save(render(big, size), f"cdeckj-{size}.png")
    open(os.path.join(OUT, "cdeckj-small.svg"), "w").write(icon_svg(small=True))
    for size in (48, 32, 16):
        save(render(icon_svg(small=True), size), f"cdeckj-{size}.png")

    # Steam library art. Portrait capsule.
    img = scene(600, 900, 290, 380, 2.0)
    d = ImageDraw.Draw(img)
    f = ImageFont.truetype(FONT, 132)
    tw = d.textlength("CDeckJ", font=f)
    wordmark(d, (300 - tw / 2, 720), 132)
    save(img, "steam-capsule.png")
    # Wide capsule.
    img = scene(920, 430, 230, 215, 1.45)
    d = ImageDraw.Draw(img)
    wordmark(d, (430, 258), 128)
    save(img, "steam-wide.png")
    # Hero (Steam lays the logo over it): a large, faint jog and waveform off to the right.
    img = scene(1920, 620, 1480, 310, 3.6, glow=True)
    shade = Image.new("RGBA", img.size, (10, 12, 16, 0))
    grad = Image.linear_gradient("L").rotate(90).resize(img.size)
    shade.putalpha(grad.point(lambda v: int(v * 0.62)))
    img = Image.alpha_composite(img, shade)
    save(img, "steam-hero.png")
    # Logo: mark and wordmark on transparent.
    img = scene(1000, 300, 150, 150, 1.0, glow=False, bg=False)
    d = ImageDraw.Draw(img)
    wordmark(d, (300, 196), 150)
    save(img, "steam-logo.png")


if __name__ == "__main__":
    sys.exit(main())
