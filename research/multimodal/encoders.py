"""Thin ONNX Runtime wrappers around small local encoders (CPU only, no network at run time).

Models are fetched once by fetch_models.py into .models/. Every encoder returns
L2-normalised float32 rows, so cosine similarity is a dot product.

Porting note: the same .onnx files load in Rust through the `ort` crate
(ONNX Runtime bindings) and the tokenizer.json files through the `tokenizers`
crate; preprocessing below is the full contract (resize, crop, normalise).
"""
from __future__ import annotations

import os
import time

import numpy as np
import onnxruntime as ort
from PIL import Image
from tokenizers import Tokenizer

HERE = os.path.dirname(os.path.abspath(__file__))
MODELS = os.path.join(HERE, ".models")


def _session(path: str, threads: int) -> ort.InferenceSession:
    so = ort.SessionOptions()
    so.intra_op_num_threads = threads
    so.inter_op_num_threads = 1
    so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    return ort.InferenceSession(path, so, providers=["CPUExecutionProvider"])


def _norm(x: np.ndarray) -> np.ndarray:
    x = np.asarray(x, dtype=np.float32)
    return x / np.maximum(np.linalg.norm(x, axis=1, keepdims=True), 1e-9)


# ---------------------------------------------------------------------------
# Image preprocessing
def _resize_crop(img: Image.Image, size: int, resample) -> Image.Image:
    img = img.convert("RGB")
    w, h = img.size
    s = size / min(w, h)
    img = img.resize((max(size, round(w * s)), max(size, round(h * s))), resample)
    w, h = img.size
    left, top = (w - size) // 2, (h - size) // 2
    return img.crop((left, top, left + size, top + size))


class VisionTextModel:
    """CLIP-family dual encoder. `kind` in {clip, clip-q8, siglip-q8, mobileclip-s0}."""

    SPECS = {
        "clip": ("clip-vit-base-patch32", "vision_model.onnx", "text_model_quantized.onnx", 224,
                 [0.48145466, 0.4578275, 0.40821073], [0.26862954, 0.26130258, 0.27577711], Image.BICUBIC),
        "clip-q8": ("clip-vit-base-patch32", "vision_model_quantized.onnx", "text_model_quantized.onnx", 224,
                    [0.48145466, 0.4578275, 0.40821073], [0.26862954, 0.26130258, 0.27577711], Image.BICUBIC),
        "siglip-q8": ("siglip-base-patch16-224", "vision_model_quantized.onnx", "text_model_quantized.onnx", 224,
                      [0.5, 0.5, 0.5], [0.5, 0.5, 0.5], Image.BICUBIC),
        "mobileclip-s0": ("mobileclip_s0", "vision_model.onnx", "text_model.onnx", 256,
                          [0.0, 0.0, 0.0], [1.0, 1.0, 1.0], Image.BILINEAR),
    }

    def __init__(self, kind: str, threads: int = 4):
        d, vis, txt, size, mean, std, rs = self.SPECS[kind]
        self.kind = kind
        base = os.path.join(MODELS, d)
        self.vs = _session(os.path.join(base, "onnx", vis), threads)
        self.ts = _session(os.path.join(base, "onnx", txt), threads)
        self.tok = Tokenizer.from_file(os.path.join(base, "tokenizer.json"))
        self.size, self.rs = size, rs
        self.mean = np.array(mean, np.float32).reshape(1, 3, 1, 1)
        self.std = np.array(std, np.float32).reshape(1, 3, 1, 1)
        self.siglip = kind.startswith("siglip")
        self.fixed_text_len = 77 if kind.startswith("mobileclip") else None

    def pixels(self, imgs: list) -> np.ndarray:
        arr = np.stack([np.asarray(_resize_crop(im, self.size, self.rs), np.float32) / 255.0 for im in imgs])
        arr = arr.transpose(0, 3, 1, 2)
        return ((arr - self.mean) / self.std).astype(np.float32)

    def encode_images(self, imgs: list, batch: int = 32) -> np.ndarray:
        out = []
        for i in range(0, len(imgs), batch):
            px = self.pixels(imgs[i:i + batch])
            r = self.vs.run(None, {"pixel_values": px})
            out.append(r[1] if self.siglip else r[0])
        return _norm(np.concatenate(out))

    def encode_texts(self, texts: list) -> np.ndarray:
        out = []
        for t in texts:
            ids = self.tok.encode(t.lower() if self.siglip else t).ids
            if self.siglip:
                pad = self.tok.token_to_id("</s>") or 1
                ids = (ids + [pad] * 64)[:64]
            elif self.fixed_text_len:
                ids = (ids + [0] * self.fixed_text_len)[:self.fixed_text_len]
            r = self.ts.run(None, {"input_ids": np.array([ids], np.int64)})
            out.append(r[1] if self.siglip else r[0])
        return _norm(np.concatenate(out))


class TextEncoder:
    """Sentence encoder with mean pooling. `kind` in {e5-small-q8 (multilingual), minilm-q8 (English)}."""

    SPECS = {"e5-small-q8": ("multilingual-e5-small", "model_quantized.onnx", True),
             "minilm-q8": ("all-MiniLM-L6-v2", "model_quantized.onnx", False)}

    def __init__(self, kind: str = "e5-small-q8", threads: int = 4, max_len: int = 64):
        d, f, self.e5 = self.SPECS[kind]
        self.kind = kind
        self.s = _session(os.path.join(MODELS, d, "onnx", f), threads)
        self.tok = Tokenizer.from_file(os.path.join(MODELS, d, "tokenizer.json"))
        self.tok.enable_truncation(max_len)
        pad_id = self.tok.token_to_id("<pad>") if self.e5 else self.tok.token_to_id("[PAD]")
        self.tok.enable_padding(pad_id=pad_id or 0)

    def encode(self, texts: list, batch: int = 64, prefix: str = "passage: ") -> np.ndarray:
        out = []
        for i in range(0, len(texts), batch):
            chunk = [(prefix if self.e5 else "") + t for t in texts[i:i + batch]]
            enc = self.tok.encode_batch(chunk)
            ids = np.array([e.ids for e in enc], np.int64)
            am = np.array([e.attention_mask for e in enc], np.int64)
            h = self.s.run(None, {"input_ids": ids, "attention_mask": am,
                                  "token_type_ids": np.zeros_like(ids)})[0]
            m = am[..., None].astype(np.float32)
            out.append((h * m).sum(1) / np.maximum(m.sum(1), 1e-9))
        return _norm(np.concatenate(out))


def timeit(fn, reps: int = 20, warm: int = 3) -> tuple:
    for _ in range(warm):
        fn()
    ts = []
    for _ in range(reps):
        t0 = time.perf_counter()
        fn()
        ts.append((time.perf_counter() - t0) * 1e3)
    ts = np.array(ts)
    return float(np.median(ts)), float(np.percentile(ts, 99))
