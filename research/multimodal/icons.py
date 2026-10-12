"""Unlabeled-button naming and same-control recognition on real GNOME (Adwaita) symbolic icons.

Ground truth comes from the freedesktop icon names. Icons are rasterised the way they
appear on screen (a 24 px glyph centred in a 36 px button crop at 1x, doubled at 2x),
in a light and a dark theme, then:

  A. zero-shot naming: image embedding vs text embeddings of a closed vocabulary of
     ~70 UI action labels (prompt ensemble), top-1 / top-3 accuracy;
  B. same-control recognition: query = dark theme 1x, gallery = light theme 2x, over
     all ~640 distinct icons; image encoders vs a theme-invariant ink-mask baseline;
  C. encoder latency per crop (batch 1 and batch 32).

Usage: python icons.py [--threads N]
"""
from __future__ import annotations

import glob
import io
import json
import os
import sys

import cairosvg
import numpy as np
from PIL import Image

from encoders import VisionTextModel, timeit

ROOT = "/usr/share/icons/Adwaita/symbolic"

LABELS = {
    "add": ["list-add"], "remove": ["list-remove"], "delete": ["edit-delete", "user-trash"],
    "close": ["window-close"], "stop loading": ["process-stop"], "search": ["system-search", "edit-find"],
    "main menu": ["open-menu"], "more options": ["view-more", "view-more-horizontal"],
    "back": ["go-previous"], "forward": ["go-next"], "up": ["go-up"], "down": ["go-down"],
    "home": ["go-home", "user-home"], "refresh": ["view-refresh"], "play": ["media-playback-start"],
    "pause": ["media-playback-pause"], "stop": ["media-playback-stop"], "record": ["media-record"],
    "next track": ["media-skip-forward"], "previous track": ["media-skip-backward"],
    "save": ["document-save"], "open file": ["document-open"], "new document": ["document-new"],
    "print": ["document-print", "printer"], "copy": ["edit-copy"], "paste": ["edit-paste"], "cut": ["edit-cut"],
    "undo": ["edit-undo"], "redo": ["edit-redo"], "edit": ["document-edit"], "send": ["document-send", "mail-send"],
    "attachment": ["mail-attachment"], "reply": ["mail-reply-sender"], "new message": ["mail-message-new"],
    "bookmark": ["bookmark-new", "user-bookmarks"], "favorite": ["star-new"],
    "settings": ["preferences-system"], "help": ["help-browser", "system-help"],
    "information": ["dialog-information"], "warning": ["dialog-warning"], "error": ["dialog-error"],
    "locked": ["changes-prevent", "dialog-password"], "unlocked": ["changes-allow"],
    "zoom in": ["zoom-in"], "zoom out": ["zoom-out"], "fullscreen": ["view-fullscreen"],
    "volume": ["audio-volume-high"], "mute": ["audio-volume-muted"], "microphone": ["audio-input-microphone"],
    "camera": ["camera-photo"], "video camera": ["camera-video"], "bluetooth": ["bluetooth"],
    "wi-fi": ["network-wireless"], "battery": ["battery-full"], "folder": ["folder"],
    "downloads": ["folder-download"], "music": ["folder-music", "audio-x-generic"],
    "pictures": ["folder-pictures", "image-x-generic"], "calendar": ["x-office-calendar"],
    "user account": ["avatar-default", "system-users"], "keyboard": ["input-keyboard"], "mouse": ["input-mouse"],
    "phone": ["phone"], "power off": ["system-shutdown"], "log out": ["system-log-out"], "computer": ["computer"],
    "location": ["mark-location"], "terminal": ["utilities-terminal"], "web browser": ["web-browser"],
    "sidebar": ["sidebar-show"], "grid view": ["view-grid"], "list view": ["view-list"],
    "sort": ["view-sort-ascending"], "shuffle": ["media-playlist-shuffle"], "repeat": ["media-playlist-repeat"],
}
PROMPTS = ["a {} icon", "an icon for {}", "a {} button", "the {} symbol"]


def icon_files() -> dict:
    out = {}
    for f in sorted(glob.glob(f"{ROOT}/*/*.svg")):
        b = os.path.basename(f)
        if "rtl" in b:
            continue
        out.setdefault(b.replace("-symbolic.svg", ""), f)
    return out


def render(svg: str, glyph: int, crop: int, fg, bg) -> Image.Image:
    png = cairosvg.svg2png(url=svg, output_width=glyph, output_height=glyph)
    a = np.asarray(Image.open(io.BytesIO(png)).convert("RGBA"), np.float32)[..., 3:] / 255.0
    canvas = np.ones((crop, crop, 3), np.float32) * np.array(bg, np.float32)
    o = (crop - glyph) // 2
    canvas[o:o + glyph, o:o + glyph] = canvas[o:o + glyph, o:o + glyph] * (1 - a) + a * np.array(fg, np.float32)
    return Image.fromarray(canvas.clip(0, 255).astype(np.uint8))


LIGHT = ((46, 52, 54), (250, 250, 250))
DARK = ((255, 255, 255), (36, 36, 36))


def ink(img: Image.Image, n: int = 32) -> np.ndarray:
    """Theme-invariant baseline: |pixel - background| luminance, resized, L2-normalised."""
    g = np.asarray(img.convert("L"), np.float32)
    g = np.abs(g - np.median(g[0]))
    v = np.asarray(Image.fromarray(g).resize((n, n), Image.BILINEAR), np.float32).ravel().copy()
    v -= v.mean()
    return v / max(np.linalg.norm(v), 1e-9)


def main(threads: int = 4) -> dict:
    files = icon_files()
    names = list(files)
    res = {"n_icons": len(names), "n_labels": len(LABELS), "threads": threads}
    light2 = [render(files[n], 48, 72, *LIGHT) for n in names]
    light1 = [render(files[n], 24, 36, *LIGHT) for n in names]
    dark1 = [render(files[n], 24, 36, *DARK) for n in names]

    # Naming task items: (icon index, label index)
    lab = list(LABELS)
    items = [(names.index(icn), li) for li, l in enumerate(lab) for icn in LABELS[l] if icn in files]
    res["n_naming_items"] = len(items)

    # Baseline B with ink masks
    G = np.stack([ink(i) for i in light2]); Q = np.stack([ink(i) for i in dark1])
    res["same_control_top1_inkmask"] = float(np.mean((Q @ G.T).argmax(1) == np.arange(len(names))))

    for kind in ["clip-q8", "clip", "siglip-q8", "mobileclip-s0"]:
        m = VisionTextModel(kind, threads)
        r = {}
        T = []
        for l in lab:
            e = m.encode_texts([p.format(l) for p in PROMPTS]).mean(0)
            T.append(e / np.linalg.norm(e))
        T = np.stack(T)
        for tag, imgs in [("light1x", light1), ("light2x", light2), ("dark1x", dark1)]:
            idx = [i for i, _ in items]
            E = m.encode_images([imgs[i] for i in idx])
            S = E @ T.T
            truth = np.array([li for _, li in items])
            order = np.argsort(-S, 1)
            r[f"naming_top1_{tag}"] = float(np.mean(order[:, 0] == truth))
            r[f"naming_top3_{tag}"] = float(np.mean((order[:, :3] == truth[:, None]).any(1)))
        Gi = m.encode_images(light2); Qi = m.encode_images(dark1)
        r["same_control_top1"] = float(np.mean((Qi @ Gi.T).argmax(1) == np.arange(len(names))))
        px = [light1[0]]
        r["img_ms_b1_p50"], r["img_ms_b1_p99"] = timeit(lambda: m.encode_images(px), reps=30)
        b32 = light1[:32]
        p50, _ = timeit(lambda: m.encode_images(b32), reps=5, warm=1)
        r["img_ms_per_crop_b32"] = p50 / 32
        r["text_ms_b1_p50"], _ = timeit(lambda: m.encode_texts(["a search icon"]), reps=30)
        res[kind] = r
        print(kind, json.dumps(r), flush=True)
    return res


if __name__ == "__main__":
    th = int(sys.argv[sys.argv.index("--threads") + 1]) if "--threads" in sys.argv else 4
    out = main(th)
    os.makedirs("out", exist_ok=True)
    with open(f"out/icons_t{th}.json", "w") as f:
        json.dump(out, f, indent=1)
    print(json.dumps(out, indent=1))
