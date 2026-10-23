## Claudio Code v0.5.0 — assistente pessoal completo

### Novidades

- **Conversa por voz em tempo real.** Clique no 🎤 ou ligue **Conversa por voz**: você fala, o Claudio percebe
  quando você termina, responde em voz alta e volta a ouvir. O reconhecimento de voz (Whisper) já vem dentro do
  aplicativo e roda no seu computador; na primeira vez ele baixa o modelo e uma voz em português com um clique.
  Quem preferir pode usar a própria conta OpenAI, Groq ou Gemini.
- **Comandos de administrador com a sua senha.** Quando uma tarefa precisa de `sudo` (ou do administrador no
  Windows/macOS), aparece uma tela com o comando exato, um aviso e o campo de senha. Você decide e assume a
  responsabilidade. A senha vai direto para o sistema: a IA nunca a vê.
- **Navegador de verdade.** O Claudio abre sites numa janela que você acompanha, lê, clica, preenche
  formulários e tira fotos da tela. Para logins, você mesmo digita a senha na janela.
- **APIs e serviços externos** com chaves guardadas com segurança (`claudio secret set NOME`).
- **Mais honesto:** pesquisa quando não sabe, cita fontes, corrige você quando algo está errado e corrige a si
  mesmo.

### Downloads

| Sistema | Arquivo |
|---|---|
| Linux x86_64 (glibc ≥ 2.31) | `Claudio_Code-0.5.0-x86_64.AppImage` ou `…-linux-x86_64.tar.gz` |
| Windows 10/11 x64 | `Claudio_Code-0.5.0-windows-x86_64.zip` |
| macOS 11+ (Apple Silicon e Intel) | `Claudio_Code-0.5.0-macos-universal.dmg` ou `…-macos-universal.tar.gz` |

Confira os arquivos com `SHA256SUMS.txt`.

### Avisos

- Reconhecimento de voz no computador: processadores com AVX2 (Intel/AMD de ~2013 em diante, Apple Silicon).
- Windows: executável sem assinatura comercial (o SmartScreen pode avisar).
- macOS: assinatura ad hoc, sem notarização. Na primeira vez, clique com o botão direito → Abrir.

---

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
