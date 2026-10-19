## Claudio Code v0.3.0

Agente de IA autônomo, local-first e open source (Apache-2.0), para Linux, Windows e macOS.

### Novidades

- **Novo nome: Claudio Code** (antes “Jarvis Core”). Os comandos agora são `claudio` e `claudio-gui`.
  As pastas de configuração e dados antigas são migradas automaticamente.
- **Tela de Configurações** (botão ⚙ na barra superior):
  - **Modelos por API:** Anthropic, OpenAI, Gemini, OpenRouter, Groq, Ollama, LM Studio ou qualquer
    serviço compatível com OpenAI. Campo de chave oculto, botão **Testar conexão**, escolha do modelo padrão.
  - **Modelos locais:** instalação do llama.cpp oficial com um clique (CPU ou GPU Vulkan), catálogo de
    modelos GGUF recomendados com download que pode ser retomado e verificado (SHA‑256), aviso de memória,
    download por URL ou uso de um `.gguf` já existente, ajuste de camadas na GPU e contexto.
  - **Geral:** modo de permissões, pasta de trabalho, “usar só modelos locais”, verificação e idioma.
- Chaves de API guardadas em arquivo separado com permissão 0600, fora do `config.toml` e bloqueado para
  as ferramentas do agente.
- Novos comandos: `claudio models providers | add-api | test | catalog | download | use-file | use | remove`
  e `claudio llama-install`.

### Downloads

| Sistema | Arquivo |
|---|---|
| Linux x86_64 (glibc ≥ 2.31) | `Claudio_Code-0.3.0-x86_64.AppImage` ou `…-linux-x86_64.tar.gz` |
| Windows 10/11 x64 | `Claudio_Code-0.3.0-windows-x86_64.zip` |
| macOS 11+ (Apple Silicon e Intel) | `Claudio_Code-0.3.0-macos-universal.dmg` ou `…-macos-universal.tar.gz` |

Confira os arquivos com `SHA256SUMS.txt`.

### Avisos

- Windows: executável sem assinatura comercial (o SmartScreen pode avisar).
- macOS: assinatura ad hoc, sem notarização. Na primeira vez, clique com o botão direito → Abrir.
