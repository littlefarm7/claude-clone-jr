# Changelog

## 0.5.0
- Conversa por voz em tempo real: botão de microfone, modo “Conversa por voz” (ouve → responde falando → ouve
  de novo), detecção automática do fim da fala, whisper.cpp embutido (sem instalar nada), voz Piper em pt_BR,
  opção de usar OpenAI/Groq/Gemini; ativação com um clique; `claudio voice setup|chat|listen`.
- Comandos de administrador com autorização: tela com o comando exato, “assumo a responsabilidade” e senha
  (sudo -S, senha nunca exposta ao modelo/logs, ticket revogado ao fim); polkit e UAC (Windows); ferramenta
  `admin_command`; `shell` com `sudo` usa a mesma tela.
- Navegador interativo `browser_session` (CDP): abrir, ler com elementos numerados, clicar, digitar, escolher,
  teclas, rolar, voltar, captura; janela visível e perfil com logins persistentes; nunca digita senhas.
- `http_request` para APIs e serviços externos, com chaves `{{secret:NOME}}` (`claudio secret set`) que o
  modelo nunca vê.
- Instruções de honestidade: pesquisar quando não souber, citar fontes, corrigir o usuário com educação,
  admitir e corrigir os próprios erros, separar fato de suposição.
- Testes novos: elevação com sudo real, APIs, navegador real, pipeline de voz (espeak → microfone → whisper).

## 0.4.1
- Corrige a instalação automática da IA no computador: o llama.cpp passou a publicar os programas prontos
  (`b####`) como pré-lançamento, e `/releases/latest` apontava para uma versão só com código-fonte
  (“o release v0.7.0 não tem build pronta…”). Agora o Claudio procura nas versões recentes a primeira com o
  pacote certo para o computador.
- O `llama-server` não trava mais ao carregar modelos grandes: as mensagens dele são lidas desde o início
  (antes podiam encher o canal e bloquear o carregamento). Tempo máximo de carregamento: 10 minutos.
- Downloads grandes tentam de novo sozinhos (até 5 vezes, continuando de onde pararam) quando a conexão cai.
- “Pronto — o resultado foi conferido” só aparece quando a conferência realmente aconteceu.
- Nome da placa de vídeo exibido corretamente (AMD Radeon, NVIDIA, Intel).

## 0.4.0
- Assistente de boas-vindas: configuração automática da IA no computador (detecção de memória, GPU e disco,
  escolha do modelo, instalação e download com uma barra) ou na nuvem (Claude, ChatGPT, Gemini, OpenRouter)
  com teste da chave e erros em linguagem simples.
- Interface redesenhada: cantos arredondados, balões, barra lateral, tema claro/escuro/automático.
- Ações, permissões e erros descritos em português simples; detalhes técnicos escondidos por padrão.
- Configurações com aba Básico e opções avançadas ocultas.
- Fallback automático para CPU quando o modelo não inicia na GPU.
- Pasta de trabalho padrão em Documentos/Claudio; espelhos `CLAUDIO_LLAMA_RELEASES_API` e `CLAUDIO_MODELS_MIRROR`.
- A seção `[ui]` (tema, assistente concluído) foi adicionada ao `config.toml`.

## 0.3.0
- Projeto renomeado para Claudio Code (binários `claudio` e `claudio-gui`), com migração das pastas antigas.
- Tela de Configurações: modelos por API (com teste de conexão e chave protegida), download de modelos locais e instalação do llama.cpp, opções gerais.
- Comandos `claudio models …` e `claudio llama-install`.

## 0.2.0
- Suporte a Windows e macOS (ver RELEASE_NOTES.md).
- Release automatizado no GitHub com AppImage, tar.gz, zip e dmg.

## 0.1.0
- Primeira versão: núcleo do agente (plano → execução → verificação), permissões, auditoria, SQLite,
  provedores llama.cpp/Anthropic/Gemini/OpenAI, ferramentas de arquivos, terminal, web, documentos,
  multimídia, memória, workflows, agendamentos, MCP, skills, GUI egui e AppImage.
