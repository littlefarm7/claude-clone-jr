# Roadmap — Jarvis Core

Legenda: ✅ feito e testado · 🟡 parcial · ⛔ bloqueado (ver motivo) · ⏳ pendente

| Etapa | Escopo | Critério de aceitação | Status |
|---|---|---|---|
| 0 | Inspeção do ambiente, documentação | ARCHITECTURE/ROADMAP/SECURITY/README | ✅ |
| 1 | Fundação Rust + agente com arquivos e terminal | loop plan/exec/verify, permissões, auditoria, persistência, retomada; testes com provedor scripted | ⏳ |
| 2 | Provedores local e API | llama.cpp real; Anthropic/Gemini/OpenAI contra mocks HTTP; roteador | ⏳ |
| 3 | Web | busca (SearXNG/DuckDuckGo), fetch com bloqueio SSRF, navegador headless | ⏳ |
| 4 | Memória | SQLite FTS5 + vetores; CLI de controle | ⏳ |
| 5 | Documentos | ler/criar PDF, DOCX, XLSX, CSV, PPTX, LaTeX | ⏳ |
| 6 | Multimídia | ffprobe/ffmpeg, imagens, whisper.cpp/piper (voz) | ⏳ |
| 7 | Automações | workflows, agendamentos, segundo plano, subagentes, MCP, skills | ⏳ |
| 8 | Interface + empacotamento | GUI egui, AppImage testado | ⏳ |

## Depois da v0.1

- Streaming de tokens na GUI.
- Automação de navegador interativa via CDP (cliques, formulários).
- Transporte MCP HTTP (streamable HTTP).
- Embeddings locais dedicados (llama-server `--embedding`) por padrão.
- Entrada de voz contínua (wake word).
