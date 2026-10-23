## Claudio Code v0.4.1 — correções

- **Corrigido:** “Algo deu errado… o release v0.7.0 não tem build pronta de llama.cpp” ao clicar em
  **Instalar e começar**. O Claudio agora encontra o pacote oficial certo (testado com o llama.cpp b11829 real).
- **Corrigido:** o motor de IA podia travar ao carregar o modelo de 4,6 GB.
- Downloads grandes continuam sozinhos se a internet cair.
- “Resultado conferido” só aparece quando houve conferência.

Quem viu o erro: atualize para esta versão e clique de novo em **Configurar IA** → **No meu computador**.

---

## Claudio Code v0.4 — fácil para qualquer pessoa

Agente de IA autônomo, local-first e open source (Apache-2.0), para Linux, Windows e macOS.

### Novidades

- **Assistente de boas-vindas.** Na primeira vez, o Claudio pergunta só uma coisa: a IA vai funcionar
  **no meu computador** (grátis e privado) ou **na nuvem**?
  - **No meu computador:** detecta memória, placa de vídeo e espaço livre, escolhe o modelo certo,
    instala o motor de IA e baixa o modelo com uma única barra de progresso. Sem termos técnicos.
    Se a placa de vídeo falhar, volta sozinho para o processador.
  - **Na nuvem:** botões para Claude, ChatGPT, Gemini e OpenRouter, passo a passo para criar a chave,
    botão “Abrir o site”, teste automático da chave e mensagens de erro em português simples.
- **Visual novo, arredondado e moderno:** balões de conversa, barra lateral com “Nova conversa”,
  campo de mensagem arredondado, cartões de sugestão, **tema claro e escuro** (ou automático).
- **Linguagem simples em tudo:** cada ação aparece como “Lendo um arquivo”, “Criando um arquivo”,
  “Pesquisando na internet”; os pedidos de permissão explicam o que vai acontecer; detalhes técnicos
  ficam escondidos (“Detalhes técnicos” e “Registro técnico”).
- **Configurações simplificadas:** IA, pasta, quando pedir permissão, privacidade e aparência.
  Opções manuais em “Mostrar opções avançadas”.
- **Enter envia** a mensagem; tarefas longas têm o botão **Continuar de onde parou**.
- Pasta de trabalho padrão em **Documentos/Claudio**.
- Espelhos de download para redes restritas: `CLAUDIO_LLAMA_RELEASES_API` e `CLAUDIO_MODELS_MIRROR`.

### Downloads

| Sistema | Arquivo |
|---|---|
| Linux x86_64 (glibc ≥ 2.31) | `Claudio_Code-0.4.1-x86_64.AppImage` ou `…-linux-x86_64.tar.gz` |
| Windows 10/11 x64 | `Claudio_Code-0.4.1-windows-x86_64.zip` |
| macOS 11+ (Apple Silicon e Intel) | `Claudio_Code-0.4.1-macos-universal.dmg` ou `…-macos-universal.tar.gz` |

Confira os arquivos com `SHA256SUMS.txt`.

### Avisos

- Windows: executável sem assinatura comercial (o SmartScreen pode avisar).
- macOS: assinatura ad hoc, sem notarização. Na primeira vez, clique com o botão direito → Abrir.
