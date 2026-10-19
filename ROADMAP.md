# Roadmap — Jarvis Core

Legenda: ✅ feito e testado · 🟡 feito, teste parcial · ⛔ bloqueado (motivo) · ⏳ pendente

Ambiente de desenvolvimento: Ubuntu 24.04 x86_64 em container, 4 threads, 16 GB, sem GPU, rede restrita
(sem Hugging Face). O LMDE é o alvo; os binários exigem glibc ≥ 2.31.

## v0.1 — estado por etapa

| Etapa | Escopo | Verificação executada | Status |
|---|---|---|---|
| 0 | Inspeção do ambiente; ARCHITECTURE/ROADMAP/SECURITY/README | — | ✅ |
| 1 | Núcleo: plano → execução → verificação, limites, persistência, retomada, permissões, taint, auditoria | 9 testes de ciclo do agente (provedor scripted), 20 unitários; CLI ponta a ponta com servidor scriptado | ✅ |
| 1 | Arquivos e terminal | testes com processos reais: timeout, cancelamento, grupo de processos, segundo plano, lixeira, sandbox bubblewrap | ✅ |
| 2 | Modelo local (llama.cpp) | `llama-server` real compilado do código-fonte; autostart, health, template Jinja com tools, parada | 🟡 pesos aleatórios (download de modelos reais bloqueado) |
| 2 | Anthropic, Gemini, OpenAI | servidores HTTP simulados validando cabeçalhos e corpo no formato oficial | 🟡 sem chamadas reais (sem chaves) |
| 2 | Roteador de modelos | testes de perfil, disponibilidade, RAM vs. GGUF, local_only | ✅ |
| 3 | Web | fetch com redirecionamentos e bloqueio SSRF; SearXNG; parser DuckDuckGo; Chromium headless (JS, captura) | 🟡 busca na internet real bloqueada pela rede do ambiente |
| 4 | Memória | FTS5 + vetores léxicos; embeddings OpenAI‑compatíveis testados com llama-server real; CLI de controle | ✅ |
| 5 | Documentos | PDF/DOCX nativos, XLSX, CSV, PPTX (pandoc), conversões LibreOffice, LaTeX (pdflatex real) | ✅ |
| 6 | Multimídia | ffprobe/ffmpeg reais, edição de imagem, OCR Tesseract, espeak-ng | ✅ |
| 6 | whisper.cpp, piper | implementados; mensagens de erro testadas | ⛔ sem modelos de voz no ambiente |
| 7 | Workflows, agendamentos, daemon, segundo plano, subagentes, MCP, skills, ferramentas customizadas | CLI ponta a ponta (workflow + schedule + daemon), servidor MCP stdio real | ✅ |
| 8 | GUI egui | executada sob Xvfb: tarefa real, diálogo de aprovação, verificação PASS | ✅ |
| 8 | AppImage | gerado com appimagetool; CLI e GUI executados a partir do AppImage | ✅ |

Total: 73 testes automatizados passando (`cargo test --workspace`), `clippy -D warnings` e `rustfmt` limpos.

## Próximos passos

1. Validar com modelos reais no LMDE (Qwen2.5‑7B Q4_K_M; build Vulkan na RX 5500 XT) e ajustar prompts.
2. Streaming de tokens na GUI e na CLI.
3. Navegador interativo via CDP (cliques, formulários, login assistido).
4. Visão: anexar imagens às mensagens para modelos multimodais.
5. Transporte MCP HTTP (streamable HTTP) e recursos/prompts MCP.
6. Voz contínua (detecção de atividade de voz) e atalho global.
7. Painel de permissões na GUI (editar política sem abrir o TOML).
8. Pacote .deb e repositório APT opcional.
