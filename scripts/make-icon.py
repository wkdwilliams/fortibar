#!/usr/bin/env python3
# Draws the macOS app icon (a padlock on a shield, matching the tray icon) and
# writes assets/FortiBar.icns. The .icns is committed, so building the app
# doesn't need this; rerun it only to change the icon. Needs Pillow and iconutil.
import subprocess
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageChops
S=4; N=1024*S
def vgrad(w,h,c1,c2):
    g=Image.new("RGBA",(1,h))
    for y in range(h):
        t=y/(h-1); g.putpixel((0,y),tuple(int(c1[i]+(c2[i]-c1[i])*t) for i in range(4)))
    return g.resize((w,h))
def mask_from(fn):
    m=Image.new("L",(N,N),0); fn(ImageDraw.Draw(m)); return m
def bez(p0,p1,p2,p3,n=60):
    return [tuple((1-t)**3*p0[i]+3*(1-t)**2*t*p1[i]+3*(1-t)*t*t*p2[i]+t**3*p3[i] for i in range(2)) for t in [k/n for k in range(n+1)]]
s=lambda v:v*S
img=Image.new("RGBA",(N,N),(0,0,0,0))
tile=lambda d:d.rounded_rectangle([s(100),s(100),s(924),s(924)],radius=s(185),fill=255)
tm=mask_from(tile)
# shadow
sh=Image.new("RGBA",(N,N),(0,0,0,0)); sh.putalpha(tm.point(lambda v:v*100//255))
sh=ImageChops.offset(sh,0,s(14)).filter(ImageFilter.GaussianBlur(s(16)))
img=Image.alpha_composite(img,sh)
bg=vgrad(N,N,(70,130,255,255),(14,40,140,255)); bg.putalpha(tm)
img=Image.alpha_composite(img,bg)
gl=vgrad(N,N,(255,255,255,70),(255,255,255,0)); gl.putalpha(ImageChops.multiply(tm,gl.getchannel('A')))
img=Image.alpha_composite(img,gl)
# shield
pts=[(512,200),(765,290),(765,505)]
pts+=bez((765,505),(765,650),(650,755),(512,825))
pts+=bez((512,825),(374,755),(259,650),(259,505))
pts+=[(259,290)]
poly=[(x*S,y*S) for x,y in pts]
sm=mask_from(lambda d:d.polygon(poly,fill=255))
# soft shadow under shield
ssh=Image.new("RGBA",(N,N),(5,20,90,0)); ssh.putalpha(sm.point(lambda v:v*110//255))
ssh=ImageChops.offset(ssh,0,s(10)).filter(ImageFilter.GaussianBlur(s(10)))
img=Image.alpha_composite(img,ssh)
sg=vgrad(N,N,(255,255,255,255),(196,215,255,255)); sg.putalpha(sm)
img=Image.alpha_composite(img,sg)
# lock
d=ImageDraw.Draw(img)
navy=(22,58,190,255)
cx=512
d.rounded_rectangle([s(cx-97),s(325),s(cx+97),s(520)],radius=s(97),fill=navy)
d.rounded_rectangle([s(cx-59),s(363),s(cx+59),s(520)],radius=s(59),fill=(226,235,255,255))
d.rounded_rectangle([s(cx-125),s(455),s(cx+125),s(650)],radius=s(40),fill=navy)
d.ellipse([s(cx-30),s(512),s(cx+30),s(572)],fill="white")
d.rounded_rectangle([s(cx-14),s(550),s(cx+14),s(615)],radius=s(12),fill="white")
img=img.resize((1024,1024),Image.LANCZOS)

out = Path(__file__).resolve().parent.parent / "assets" / "FortiBar.icns"
with tempfile.TemporaryDirectory() as tmp:
    iconset = Path(tmp) / "FortiBar.iconset"
    iconset.mkdir()
    for size in (16, 32, 128, 256, 512):
        img.resize((size, size), Image.LANCZOS).save(iconset / f"icon_{size}x{size}.png")
        img.resize((size * 2, size * 2), Image.LANCZOS).save(iconset / f"icon_{size}x{size}@2x.png")
    subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(out)], check=True)
print(f"Wrote {out}")
