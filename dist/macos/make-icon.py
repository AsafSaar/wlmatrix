#!/usr/bin/env python3
"""Generate the macOS app icon (dist/macos/icon.png, 1024x1024).

A Matrix-rain motif on a dark rounded-rect tile: columns of half-width katakana
fading from dim green to a bright near-white leader. Run locally (needs Pillow +
a CJK font); the PNG it writes is committed and converted to .icns at build time
by make-app.sh. Regenerate with:  python3 dist/macos/make-icon.py
"""
import random
from PIL import Image, ImageDraw, ImageFont

S = 1024
MARGIN = 48           # transparent padding around the tile
RADIUS = 228          # ~macOS squircle-ish corner
BG = (8, 14, 9)       # near-black with a green tint
KATAKANA = [chr(c) for c in range(0xFF66, 0xFF9E)]
FONT_PATH = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"

random.seed(7)  # deterministic icon

img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(img)

# Rounded tile background.
d.rounded_rectangle([MARGIN, MARGIN, S - MARGIN, S - MARGIN], radius=RADIUS, fill=BG + (255,))

# Clip the rain to the tile by drawing onto a separate layer then masking.
rain = Image.new("RGBA", (S, S), (0, 0, 0, 0))
rd = ImageDraw.Draw(rain)

cell = 96
font = ImageFont.truetype(FONT_PATH, 84)
inner = (MARGIN + 36, S - MARGIN - 36)
span = inner[1] - inner[0]
ncols = (span + cell) // cell - 1          # full columns that fit
gutter = (span - (ncols - 1) * cell) // 2  # center the block horizontally
cols = [inner[0] + gutter + i * cell for i in range(ncols)]
for cx in cols:
    head = random.randint(2, 8)            # which row holds the bright leader
    length = random.randint(4, 9)
    for row in range(0, 11):
        cy = inner[0] + row * cell
        if cy > inner[1] - cell:
            break
        ch = random.choice(KATAKANA)
        dist = head - row                  # 0 = leader, >0 = trail above it
        if dist < 0 or dist > length:
            continue
        if dist == 0:
            color = (210, 255, 215, 255)   # bright leader
        else:
            t = 1.0 - dist / length
            g = int(90 + 165 * t)
            color = (20, g, 50, int(80 + 175 * t))
        rd.text((cx, cy), ch, font=font, fill=color)

# Mask the rain to the rounded tile.
mask = Image.new("L", (S, S), 0)
ImageDraw.Draw(mask).rounded_rectangle(
    [MARGIN, MARGIN, S - MARGIN, S - MARGIN], radius=RADIUS, fill=255
)
img.paste(rain, (0, 0), Image.composite(rain.split()[3], Image.new("L", (S, S), 0), mask))

# Subtle inner border to lift it off dark wallpapers.
d.rounded_rectangle(
    [MARGIN, MARGIN, S - MARGIN, S - MARGIN], radius=RADIUS, outline=(0, 255, 70, 70), width=4
)

out = __file__.rsplit("/", 1)[0] + "/icon.png"
img.save(out)
print("wrote", out)
