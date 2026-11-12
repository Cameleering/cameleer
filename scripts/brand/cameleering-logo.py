"""Cameleering logo (the ledger tile): writes the SVG masters and renders the PNGs with Pillow.
usage: python logo.py <repo-root>
"""
import os, sys
from PIL import Image, ImageDraw, ImageFont

root = sys.argv[1]
out = os.path.join(root, "site", "brand", "cameleering")
os.makedirs(out, exist_ok=True)

INK, GOLD, RULE, WHITE = "#18203a", "#d99a22", "#33406a", "#ffffff"
F = "C:/Windows/Fonts/"

# geometry on a 120-unit grid
TILE = (8, 8, 112, 112, 26)
RULES = [46, 66, 86]
CURVE = [((20, 86), (38, 86), (40, 36), (60, 36)), ((60, 36), (76, 36), (78, 74), (92, 74))]
DOT = (96, 74, 6)
CURVE_D = "M20 86C38 86 40 36 60 36C76 36 78 74 92 74"


def svg_mark(full_bleed=False, dark=False):
    tile_fill, dot = (WHITE, INK) if dark else (INK, WHITE)
    rect = '<rect width="120" height="120" fill="%s"/>' % tile_fill if full_bleed else \
        '<rect x="8" y="8" width="104" height="104" rx="26" fill="%s"/>' % tile_fill
    rules = "" if dark else '<path d="M22 46H98M22 66H98M22 86H98" stroke="%s" stroke-width="2"/>' % RULE
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 120" role="img" aria-label="Cameleering">'
            '<title>Cameleering</title>%s%s'
            '<path d="%s" fill="none" stroke="%s" stroke-width="8" stroke-linecap="round"/>'
            '<circle cx="96" cy="74" r="6" fill="%s"/></svg>\n') % (rect, rules, CURVE_D, GOLD, dot)


def svg_lockup(dark=False):
    text = WHITE if dark else INK
    mark = svg_mark(dark=False).split(">", 1)[1].rsplit("</svg>", 1)[0].replace("<title>Cameleering</title>", "")
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 520 120" role="img" aria-label="Cameleering">'
            '<title>Cameleering</title><g>%s</g>'
            '<text x="132" y="78" font-family="\'Schibsted Grotesk\', \'Segoe UI\', system-ui, sans-serif" '
            'font-weight="800" font-size="52" letter-spacing="-0.5" fill="%s">Cameleering</text></svg>\n') % (mark, text)


for name, body in [("cameleering-mark.svg", svg_mark()), ("cameleering-mark-dark.svg", svg_mark(dark=True)),
                   ("cameleering-avatar.svg", svg_mark(full_bleed=True)),
                   ("cameleering-lockup.svg", svg_lockup()), ("cameleering-lockup-dark.svg", svg_lockup(dark=True))]:
    open(os.path.join(out, name), "w", encoding="utf-8", newline="\n").write(body)


def bez(p0, p1, p2, p3, n=200):
    pts = []
    for i in range(n + 1):
        t = i / n
        a = (1 - t) ** 3; b = 3 * (1 - t) ** 2 * t; c = 3 * (1 - t) * t * t; d = t ** 3
        pts.append((a * p0[0] + b * p1[0] + c * p2[0] + d * p3[0], a * p0[1] + b * p1[1] + c * p2[1] + d * p3[1]))
    return pts


def render(size, full_bleed=False, simple=False):
    S = 16  # supersample
    W = size * S
    k = W / 120.0
    img = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    if full_bleed:
        d.rectangle((0, 0, W, W), fill=INK)
    else:
        x0, y0, x1, y1, r = TILE
        if simple:
            x0, y0, x1, y1, r = 4, 4, 116, 116, 28
        d.rounded_rectangle((x0 * k, y0 * k, x1 * k, y1 * k), radius=r * k, fill=INK)
    if not simple:
        for y in RULES:
            d.line((22 * k, y * k, 98 * k, y * k), fill=RULE, width=max(1, int(2 * k)))
    sw = (13 if simple else 8) * k
    pts = [q for seg in CURVE for q in bez(*seg)]
    pts = [(x * k, y * k) for x, y in pts]
    for x, y in pts:
        d.ellipse((x - sw / 2, y - sw / 2, x + sw / 2, y + sw / 2), fill=GOLD)
    dx, dy, dr = DOT
    if simple:
        dx, dr = 97, 10
    d.ellipse(((dx - dr) * k, (dy - dr) * k, (dx + dr) * k, (dy + dr) * k), fill=WHITE)
    return img.resize((size, size), Image.LANCZOS)


for s in (512, 192, 64):
    render(s).save(os.path.join(out, "cameleering-mark-%d.png" % s), optimize=True)
render(32, simple=True).save(os.path.join(out, "cameleering-mark-32.png"), optimize=True)
render(180, simple=False).save(os.path.join(out, "cameleering-apple-touch-icon.png"), optimize=True)
render(512, full_bleed=True).save(os.path.join(out, "cameleering-avatar-512.png"), optimize=True)

# social preview 1280x640
W, H = 1280, 640
og = Image.new("RGB", (W, H), WHITE)
d = ImageDraw.Draw(og)
d.rectangle((0, 0, W, 8), fill=GOLD)
mark = render(220)
og.paste(mark, (80, 90), mark)
d.text((330, 120), "Cameleering", font=ImageFont.truetype(F + "segoeuib.ttf", 92), fill=INK)
d.text((334, 240), "Carry more on less.", font=ImageFont.truetype(F + "seguisb.ttf", 44), fill=INK)
d.text((84, 380), "Make software carry the same load on less time, memory, size and", font=ImageFont.truetype(F + "segoeui.ttf", 32), fill="#5b6479")
d.text((84, 426), "dependencies, and prove every gain against the original.", font=ImageFont.truetype(F + "segoeui.ttf", 32), fill="#5b6479")
d.line((84, 560, W - 84, 560), fill="#e4e8f0", width=2)
d.text((84, 576), "No claim without a number.", font=ImageFont.truetype(F + "seguisb.ttf", 26), fill="#1f4fff")
t = "cameleer.ing/cameleering"
f = ImageFont.truetype(F + "seguisb.ttf", 26)
d.text((W - 84 - d.textlength(t, font=f), 576), t, font=f, fill=INK)
og.save(os.path.join(out, "cameleering-og.png"), optimize=True)
print("wrote", sorted(os.listdir(out)))
