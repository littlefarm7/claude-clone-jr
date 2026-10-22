# Changelog

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
