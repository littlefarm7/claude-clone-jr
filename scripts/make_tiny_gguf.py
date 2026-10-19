#!/usr/bin/env python3
"""Build a tiny qwen2-architecture GGUF with RANDOM weights.

Purpose: integration tests of the local runtime (llama-server start/health,
OpenAI-compatible requests, Jinja chat template with tool support) on
machines without internet access to real models. The model's output is
meaningless text. Usage:

    python3 make_tiny_gguf.py <llama.cpp dir> <output.gguf>
"""
import sys

import numpy as np

llama_dir, out = sys.argv[1], sys.argv[2]
sys.path.insert(0, f"{llama_dir}/gguf-py")
import gguf  # noqa: E402

vocab = gguf.GGUFReader(f"{llama_dir}/models/ggml-vocab-qwen2.gguf")
template = open(f"{llama_dir}/models/templates/Qwen-Qwen2.5-7B-Instruct.jinja").read()


def field(name):
    f = vocab.fields[name]
    if f.types[0] == gguf.GGUFValueType.ARRAY:
        if f.types[1] == gguf.GGUFValueType.STRING:
            return [bytes(f.parts[i]).decode("utf-8", "replace") for i in f.data]
        return [int(f.parts[i][0]) for i in f.data]
    if f.types[0] == gguf.GGUFValueType.STRING:
        return bytes(f.parts[f.data[0]]).decode()
    return f.parts[f.data[0]][0]


tokens = field("tokenizer.ggml.tokens")
n_vocab, n_embd, n_ff, n_layer, n_head, n_kv = len(tokens), 64, 128, 2, 4, 2
head_dim = n_embd // n_head
rng = np.random.default_rng(0)

w = gguf.GGUFWriter(out, "qwen2")
w.add_name("jarvis-tiny-random")
w.add_block_count(n_layer)
w.add_context_length(4096)
w.add_embedding_length(n_embd)
w.add_feed_forward_length(n_ff)
w.add_head_count(n_head)
w.add_head_count_kv(n_kv)
w.add_rope_freq_base(1000000.0)
w.add_layer_norm_rms_eps(1e-6)
w.add_file_type(gguf.LlamaFileType.MOSTLY_F16)
w.add_tokenizer_model(field("tokenizer.ggml.model"))
w.add_tokenizer_pre(field("tokenizer.ggml.pre"))
w.add_token_list(tokens)
w.add_token_types(field("tokenizer.ggml.token_type"))
w.add_token_merges(field("tokenizer.ggml.merges"))
w.add_eos_token_id(int(field("tokenizer.ggml.eos_token_id")))
w.add_bos_token_id(int(field("tokenizer.ggml.bos_token_id")))
w.add_pad_token_id(int(field("tokenizer.ggml.padding_token_id")))
w.add_chat_template(template)


def t(name, *shape, f16=True):
    a = (rng.standard_normal(shape) * 0.02).astype(np.float16 if f16 else np.float32)
    w.add_tensor(name, a)


t("token_embd.weight", n_vocab, n_embd)
t("output_norm.weight", n_embd, f16=False)
t("output.weight", n_vocab, n_embd)
for i in range(n_layer):
    p = f"blk.{i}."
    t(p + "attn_norm.weight", n_embd, f16=False)
    t(p + "attn_q.weight", n_embd, n_embd)
    t(p + "attn_q.bias", n_embd, f16=False)
    t(p + "attn_k.weight", n_kv * head_dim, n_embd)
    t(p + "attn_k.bias", n_kv * head_dim, f16=False)
    t(p + "attn_v.weight", n_kv * head_dim, n_embd)
    t(p + "attn_v.bias", n_kv * head_dim, f16=False)
    t(p + "attn_output.weight", n_embd, n_embd)
    t(p + "ffn_norm.weight", n_embd, f16=False)
    t(p + "ffn_gate.weight", n_ff, n_embd)
    t(p + "ffn_up.weight", n_ff, n_embd)
    t(p + "ffn_down.weight", n_embd, n_ff)

w.write_header_to_file()
w.write_kv_data_to_file()
w.write_tensors_to_file()
w.close()
print(f"ok: {out}")
