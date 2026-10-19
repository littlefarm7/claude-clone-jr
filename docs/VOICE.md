# Voz (local)

O Claudio já tem a camada de voz pronta. Ela usa motores locais e gratuitos:

| Função | Motor | Estado |
|---|---|---|
| Fala → texto | whisper.cpp (`whisper-cli`) | implementado; não testado neste ambiente (sem modelo) |
| Texto → fala | piper | implementado; não testado neste ambiente (sem voz `.onnx`) |
| Texto → fala | espeak-ng | implementado e testado |
| Microfone | `arecord` (alsa-utils) | implementado (`claudio voice listen`) |

## Instalação no LMDE

1. Síntese simples: `sudo apt install espeak-ng alsa-utils`.
2. whisper.cpp:
   ```bash
   git clone --depth 1 https://github.com/ggml-org/whisper.cpp ~/whisper.cpp
   cd ~/whisper.cpp && cmake -B build -DGGML_VULKAN=ON && cmake --build build -j --target whisper-cli
   sh ./models/download-ggml-model.sh small
   ```
3. piper (voz natural): baixe o binário em <https://github.com/rhasspy/piper/releases> e uma voz pt_BR
   (por exemplo `pt_BR-faber-medium.onnx` + `.onnx.json`).
4. Configure:
   ```toml
   [voice]
   whisper_bin = "~/whisper.cpp/build/bin/whisper-cli"
   whisper_model = "~/whisper.cpp/models/ggml-small.bin"
   piper_bin = "~/piper/piper"
   piper_model = "~/modelos/pt_BR-faber-medium.onnx"
   espeak_voice = "pt-br"
   ```

## Uso

- `claudio voice say "Bom dia"` — gera `fala.wav` e toca com `paplay`/`aplay`.
- `claudio voice transcribe reuniao.mp3 --lang pt` — transcreve qualquer áudio/vídeo (converte com ffmpeg).
- `claudio voice listen --seconds 6` — grava, transcreve e executa como tarefa.
- O agente também pode usar as ferramentas `transcribe` e `speak`.

Próximo passo previsto: modo de conversa contínua com detecção de fala (ver ROADMAP).
