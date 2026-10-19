# Modelos locais no LMDE (llama.cpp)

Hardware de referência: Xeon E5-2680 v4 (14 núcleos / 28 threads, AVX2, sem AVX-512), 16 GB de RAM,
Radeon RX 5500 XT (4 ou 8 GB de VRAM).

## 1. Compilar o llama.cpp

### Só CPU (mais simples)

```bash
sudo apt install build-essential cmake git
git clone --depth 1 https://github.com/ggml-org/llama.cpp ~/llama.cpp
cd ~/llama.cpp
cmake -B build -DCMAKE_BUILD_TYPE=Release -DLLAMA_CURL=OFF
cmake --build build -j"$(nproc)" --target llama-server
```

### Com GPU AMD via Vulkan (recomendado para a RX 5500 XT)

```bash
sudo apt install libvulkan-dev glslc mesa-vulkan-drivers vulkan-tools
vulkaninfo --summary          # deve listar a RX 5500 XT (driver RADV)
cd ~/llama.cpp
cmake -B build -DCMAKE_BUILD_TYPE=Release -DLLAMA_CURL=OFF -DGGML_VULKAN=ON
cmake --build build -j"$(nproc)" --target llama-server
```

Depois, aponte o Claudio para o binário em `~/.config/claudio/config.toml`:

```toml
[local]
llama_server = "~/llama.cpp/build/bin/llama-server"
gpu_layers = 99      # 8 GB de VRAM; com 4 GB use ~20 para um modelo 7B Q4
threads = 14         # núcleos físicos
ctx_size = 8192
```

## 2. Escolher um modelo (GGUF)

Modelos com bom suporte a chamadas de ferramenta no llama.cpp (`--jinja`): Qwen 2.5/3, Llama 3.1+, Mistral Nemo,
Hermes. Sugestões para 16 GB de RAM:

| Modelo | Arquivo (aprox.) | Uso |
|---|---|---|
| Qwen2.5-3B-Instruct Q4_K_M | 2 GB | rápido, cabe inteiro em 4 GB de VRAM |
| Qwen2.5-7B-Instruct Q4_K_M | 4,7 GB | equilíbrio (recomendado) |
| Qwen2.5-Coder-7B-Instruct Q4_K_M | 4,7 GB | código |
| Llama-3.1-8B-Instruct Q4_K_M | 4,9 GB | geral |
| Qwen3-14B Q4_K_M | 9 GB | melhor raciocínio; mais lento em CPU |

Baixe de repositórios GGUF no Hugging Face (ex.: `Qwen/Qwen2.5-7B-Instruct-GGUF`) para `~/modelos/`.
Evite quantizar o cache KV agressivamente (`-ctk q4_0`): prejudica as chamadas de ferramenta.

## 3. Configurar no Claudio

```toml
default_model = "local"

[[models]]
name = "local"
provider = "openai_compat"
model = "qwen2.5-7b"
base_url = "http://127.0.0.1:9931/v1"
gguf_path = "~/modelos/qwen2.5-7b-instruct-q4_k_m.gguf"
context_window = 8192
max_output_tokens = 2048
tags = ["fast", "default", "local", "code"]
```

Com `gguf_path` e `local.autostart = true`, o Claudio inicia o `llama-server` em `127.0.0.1` quando precisa e
o encerra ao sair. O roteador recusa o modelo se o arquivo for maior que a RAM livre.

Para usar um servidor já em execução (seu, ou Ollama/LM Studio), remova `gguf_path` e ajuste `base_url`.
Teste com `claudio models`.

## 4. Memória semântica com embeddings locais (opcional)

```bash
llama-server -m ~/modelos/nomic-embed-text-v1.5.Q8_0.gguf --embedding --port 9932
```

```toml
[[models]]
name = "emb"
provider = "openai_compat"
model = "nomic-embed"
base_url = "http://127.0.0.1:9932/v1"

[memory]
embedding_model = "emb"
```

Sem isso, a memória usa busca de texto completo (FTS5) somada a vetores léxicos calculados localmente.
