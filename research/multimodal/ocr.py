"""OCR fallback feasibility: Tesseract 5 (LSTM, eng) on rendered single-line UI labels.

Labels are real UI-documentation strings (help corpus headings, links, list items,
cut to <= 40 chars). They are rendered at desktop sizes in Cantarell (GNOME UI font)
and DejaVu Sans, dark-on-light and light-on-dark, at 1x and 2x scale, with and without
3x upscaling before OCR. Reports character error rate (CER), exact-match rate and
per-crop latency (amortised over a batch in one process, which approximates the
libtesseract API without process spawn).

Usage: python ocr.py
"""
from __future__ import annotations

import json
import os
import random
import subprocess
import tempfile
import time

import numpy as np
from PIL import Image, ImageDraw, ImageFont

from corpus import load

FONTS = {"cantarell": "/usr/share/fonts/opentype/cantarell/Cantarell-VF.otf",
         "dejavu": "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"}


def lev(a: str, b: str) -> int:
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def render(text: str, font: str, px: int, dark: bool, up: int) -> Image.Image:
    f = ImageFont.truetype(FONTS[font], px)
    l, t, r, b = f.getbbox(text)
    pad = max(4, px // 2)
    fg, bg = ((255, 255, 255), (36, 36, 36)) if dark else ((46, 52, 54), (250, 250, 250))
    im = Image.new("RGB", (r - l + 2 * pad, b - t + 2 * pad), bg)
    ImageDraw.Draw(im).text((pad - l, pad - t), text, font=f, fill=fg)
    if up > 1:
        im = im.resize((im.width * up, im.height * up), Image.LANCZOS)
    return im


def run_batch(imgs: list) -> tuple:
    with tempfile.TemporaryDirectory() as d:
        paths = []
        for i, im in enumerate(imgs):
            p = os.path.join(d, f"{i:04d}.png"); im.save(p); paths.append(p)
        lst = os.path.join(d, "list.txt")
        open(lst, "w").write("\n".join(paths) + "\n")
        t0 = time.perf_counter()
        subprocess.run(["tesseract", lst, os.path.join(d, "out"), "--psm", "7", "-l", "eng"],
                       check=True, capture_output=True, env={**os.environ, "OMP_THREAD_LIMIT": "1"})
        dt = (time.perf_counter() - t0) * 1e3 / len(imgs)
        txt = open(os.path.join(d, "out.txt")).read().split("\f")
    return [t.strip() for t in txt[:len(imgs)]], dt


def main() -> dict:
    it = load("C")
    pool = [t[:40].rsplit(" ", 1)[0] if len(t) > 40 else t
            for t, r in zip(it.text, it.role) if r in ("heading", "link", "listitem") and t]
    rng = random.Random(0)
    labels = rng.sample(pool, 120)
    res = {}
    for font in FONTS:
        for px, scale in [(11, 1), (13, 1), (22, 2)]:
            for dark in (False, True):
                for up in (1, 3):
                    if scale == 2 and up == 3:
                        continue
                    imgs = [render(s, font, px, dark, up) for s in labels]
                    out, ms = run_batch(imgs)
                    cer = sum(lev(a, b) for a, b in zip(out, labels)) / sum(len(s) for s in labels)
                    em = float(np.mean([a == b for a, b in zip(out, labels)]))
                    key = f"{font}_{px}px_{'dark' if dark else 'light'}_up{up}"
                    res[key] = {"cer": round(cer, 4), "exact": round(em, 3), "ms_per_crop": round(ms, 1)}
                    print(key, res[key], flush=True)
    return res


if __name__ == "__main__":
    r = main()
    os.makedirs("out", exist_ok=True)
    json.dump(r, open("out/ocr.json", "w"), indent=1)
