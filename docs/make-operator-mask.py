#!/usr/bin/env python3
"""Generate docs/operator-mask.png — the original "operator" figure used in the
hidden-image demo (a fedora + long-coat silhouette, entirely original artwork,
no third-party imagery).

The mask is written DARK-figure-on-BRIGHT-background on purpose: with
`mask_contrast` the rain outside the figure stays bright while the figure dims,
so it reads as a silhouette cut out of the rain (the Matrix "operator view").
Regenerate with:  python3 docs/make-operator-mask.py
"""
from PIL import Image, ImageDraw, ImageFilter, ImageOps

W, H = 600, 900
img = Image.new("L", (W, H), 0)
d = ImageDraw.Draw(img)
F = 255  # draw the figure white-on-black, then invert at the end

# fedora
d.ellipse([186, 150, 414, 192], fill=F)                       # brim
d.rounded_rectangle([250, 88, 350, 162], radius=16, fill=F)   # crown
# head + neck
d.ellipse([262, 168, 338, 246], fill=F)
d.rectangle([286, 236, 314, 268], fill=F)
# shoulders
d.rounded_rectangle([212, 262, 388, 322], radius=28, fill=F)
# long coat: dramatic A-line flare to a wide billowing hem
d.polygon([(220, 292), (206, 330), (150, 770),
           (450, 770), (394, 330), (380, 292)], fill=F)
# upper-arm bulk down the sides
d.rounded_rectangle([200, 300, 246, 560], radius=22, fill=F)
d.rounded_rectangle([354, 300, 400, 560], radius=22, fill=F)
# wide planted stance: legs + feet
d.polygon([(250, 770), (236, 880), (286, 880), (292, 770)], fill=F)
d.polygon([(308, 770), (314, 880), (364, 880), (350, 770)], fill=F)
d.ellipse([224, 868, 300, 900], fill=F)
d.ellipse([300, 868, 376, 900], fill=F)

img = img.filter(ImageFilter.GaussianBlur(1.6))  # soft edges sample cleanly
out = __file__.rsplit("/", 1)[0] + "/operator-mask.png"
ImageOps.invert(img).save(out)  # -> dark figure on a bright background
print("wrote", out, img.size)
