#!/usr/bin/env python3
"""Build a whisper.cpp "tiny" model with RANDOM weights.

Purpose: integration tests of local speech recognition (model loading and
transcription through whisper.cpp) on machines without access to real models.
The transcription is meaningless text. It reuses the header, mel filters and
vocabulary of whisper.cpp's `models/for-tests-ggml-tiny.bin`. Usage:

    python3 make_tiny_whisper.py <whisper.cpp>/models/for-tests-ggml-tiny.bin <output.bin>
"""
import struct
import sys

import numpy as np

src, out = sys.argv[1], sys.argv[2]
data = open(src, "rb").read()
magic, n_vocab, a_ctx, a_state, a_head, a_layer, t_ctx, t_state, t_head, t_layer, n_mels, ftype = struct.unpack(
    "<12i", data[:48]
)
assert magic == 0x67676D6C, "not a ggml whisper file"
# ftype = 1: matrices and convolutions in f16 (like the real models), the rest f32.
header = struct.pack("<12i", magic, n_vocab, a_ctx, a_state, a_head, a_layer, t_ctx, t_state, t_head, t_layer, n_mels, 1)
rest = data[48:]

rng = np.random.default_rng(0)
f = open(out, "wb")
f.write(header)
f.write(rest)


def tensor(name, *ne, scale=0.02):
    n = int(np.prod(ne))
    half = name.endswith(".weight") and len(ne) >= 2
    arr = (rng.standard_normal(n) * scale).astype("<f2" if half else "<f4")
    if name.endswith("ln.weight") or "_ln.weight" in name or name.endswith("ln_post.weight"):
        arr[:] = 1.0
    f.write(struct.pack("<3i", len(ne), len(name), 1 if half else 0))
    f.write(struct.pack(f"<{len(ne)}i", *ne))
    f.write(name.encode())
    f.write(arr.tobytes())


tensor("encoder.positional_embedding", a_state, a_ctx)
tensor("encoder.conv1.weight", 3, n_mels, a_state)
tensor("encoder.conv1.bias", 1, a_state)
tensor("encoder.conv2.weight", 3, a_state, a_state)
tensor("encoder.conv2.bias", 1, a_state)
tensor("encoder.ln_post.weight", a_state)
tensor("encoder.ln_post.bias", a_state)
for i in range(a_layer):
    p = f"encoder.blocks.{i}."
    tensor(p + "mlp_ln.weight", a_state)
    tensor(p + "mlp_ln.bias", a_state)
    tensor(p + "mlp.0.weight", a_state, 4 * a_state)
    tensor(p + "mlp.0.bias", 4 * a_state)
    tensor(p + "mlp.2.weight", 4 * a_state, a_state)
    tensor(p + "mlp.2.bias", a_state)
    tensor(p + "attn_ln.weight", a_state)
    tensor(p + "attn_ln.bias", a_state)
    tensor(p + "attn.query.weight", a_state, a_state)
    tensor(p + "attn.query.bias", a_state)
    tensor(p + "attn.key.weight", a_state, a_state)
    tensor(p + "attn.value.weight", a_state, a_state)
    tensor(p + "attn.value.bias", a_state)
    tensor(p + "attn.out.weight", a_state, a_state)
    tensor(p + "attn.out.bias", a_state)
tensor("decoder.positional_embedding", t_state, t_ctx)
tensor("decoder.token_embedding.weight", t_state, n_vocab)
tensor("decoder.ln.weight", t_state)
tensor("decoder.ln.bias", t_state)
for i in range(t_layer):
    p = f"decoder.blocks.{i}."
    for pre in ("", "cross_"):
        tensor(p + pre + "attn_ln.weight", t_state)
        tensor(p + pre + "attn_ln.bias", t_state)
        tensor(p + pre + "attn.query.weight", t_state, t_state)
        tensor(p + pre + "attn.query.bias", t_state)
        tensor(p + pre + "attn.key.weight", t_state, t_state)
        tensor(p + pre + "attn.value.weight", t_state, t_state)
        tensor(p + pre + "attn.value.bias", t_state)
        tensor(p + pre + "attn.out.weight", t_state, t_state)
        tensor(p + pre + "attn.out.bias", t_state)
    tensor(p + "mlp_ln.weight", t_state)
    tensor(p + "mlp_ln.bias", t_state)
    tensor(p + "mlp.0.weight", t_state, 4 * t_state)
    tensor(p + "mlp.0.bias", 4 * t_state)
    tensor(p + "mlp.2.weight", 4 * t_state, t_state)
    tensor(p + "mlp.2.bias", t_state)
f.close()
print("ok:", out)
