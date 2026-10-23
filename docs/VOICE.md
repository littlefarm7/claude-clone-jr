# Conversa por voz

O Claudio entende o que você fala e responde falando. Na interface:

1. Clique no **🎤** ao lado do campo de mensagem e fale. Quando você faz uma pausa, ele entende e envia.
2. Ou ligue **Conversa por voz** (no alto, à direita): ele responde em voz alta e volta a ouvir sozinho.
   Se ninguém falar por alguns segundos, a conversa pausa.
3. Para interromper a fala do Claudio, clique no 🎤.

Na primeira vez, aparece a tela **Conversar por voz** com duas opções:

| Opção | O que faz | Privacidade |
|---|---|---|
| **Ativar voz no computador** | baixa o reconhecimento de voz (Whisper, ~180 MB; ~60 MB em computadores com pouca memória) e uma voz em português (Piper, ~70 MB) | nada sai do computador |
| **Usar minha conta na nuvem** | usa a OpenAI, Groq ou Gemini já configurada | o áudio da fala é enviado ao serviço escolhido |

Também em **Configurações → Voz**: ativar/trocar e “Ler as respostas em voz alta”.

## Como funciona

| Parte | Linux | macOS | Windows |
|---|---|---|---|
| Microfone | `parecord`, `arecord` ou `pw-record` (já vêm no sistema) | nativo (CoreAudio) | nativo (WASAPI) |
| Fim da fala | detector de voz por energia com ruído adaptativo (pausa de ~0,9 s) | igual | igual |
| Fala → texto | **whisper.cpp embutido no Claudio** (sem instalar nada) ou nuvem | igual | igual |
| Texto → fala | Piper (pt_BR-faber-medium) ou espeak-ng | Piper ou `say` | Piper ou voz do Windows (SAPI) |
| Tocar o áudio | `paplay`, `pw-play` ou `aplay` | `afplay` | Windows (SoundPlayer) |

O reconhecimento embutido é compilado para processadores com AVX2 (Intel/AMD a partir de ~2013 e todos os
Apple Silicon). Em processadores mais antigos, use a opção na nuvem.

## Terminal

```bash
claudio voice setup            # instala reconhecimento e voz (uma vez)
claudio voice setup --cloud    # usa sua conta OpenAI/Groq/Gemini para entender a voz
claudio voice chat             # conversa contínua (Ctrl+C sai)
claudio voice listen           # ouve uma frase e executa como tarefa
claudio voice transcribe reuniao.mp3 --lang pt
claudio voice say "Bom dia"
```

O agente também pode usar as ferramentas `transcribe` (arquivos de áudio e vídeo) e `speak`.

## Configuração (opcional)

```toml
[voice]
stt = "local"              # ou "cloud"
speak_replies = true
whisper_model = "~/.local/share/claudio/voice/ggml-small-q5_1.bin"   # preenchido pelo assistente
piper_bin = "~/.local/share/claudio/voice/piper/piper"
piper_model = "~/.local/share/claudio/voice/pt_BR-faber-medium.onnx"
espeak_voice = "pt-br"
```

Redes que bloqueiam o Hugging Face: `CLAUDIO_MODELS_MIRROR` (pasta com os arquivos `.bin`/`.onnx` pelo nome)
e `CLAUDIO_PIPER_RELEASE` (pasta com os pacotes do Piper).

## O que foi testado

- Detecção de fala com áudio real (frase gerada pelo espeak-ng entrando pelo “microfone”), silêncio sem
  resposta, conversa completa na interface e no terminal (ouvir → transcrever → responder → ouvir de novo).
- whisper.cpp embutido carregando e transcrevendo com um modelo de teste de pesos aleatórios
  (`scripts/make_tiny_whisper.py`): o texto sai sem sentido, mas o caminho completo é exercitado.
  A qualidade com o modelo real (`ggml-small-q5_1`) não pôde ser verificada no ambiente de desenvolvimento
  (Hugging Face bloqueado).
- Instalação com um clique a partir de um espelho local.
