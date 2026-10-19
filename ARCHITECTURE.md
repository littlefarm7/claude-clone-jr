# Arquitetura — Jarvis Core

Jarvis Core é um agente autônomo de uso geral escrito em Rust. A CLI (`jarvis`)
e a interface gráfica (`jarvis-gui`) usam o mesmo núcleo. O núcleo não sabe
qual modelo está rodando nem quais ferramentas existem: tudo entra por traits.

## Visão geral

```
            +-------------------+      +--------------------+
            |   jarvis (CLI)    |      |  jarvis-gui (egui) |
            +---------+---------+      +----------+---------+
                      |   Approver / EventSink    |
                      +-------------+-------------+
                                    |
                     +--------------v---------------+
                     |          jarvis-core         |
                     |  Agent (plan/exec/verify)    |
                     |  Permissions + Taint + Audit |
                     |  Store (SQLite) + Memory     |
                     |  Config, Limits, Scheduler   |
                     +------+----------------+------+
                            |                |
               +------------v----+    +------v---------------+
               |   jarvis-llm    |    |    jarvis-tools      |
               | Provider trait  |    | fs, shell, web, docs |
               | llama.cpp/OpenAI|    | media, memory, mcp,  |
               | Anthropic,Gemini|    | skills, custom, voice|
               | Router          |    | subagents            |
               +-----------------+    +----------------------+
```

## Crates

| Crate | Papel | Depende de |
|---|---|---|
| `jarvis-llm` | Tipos de mensagem neutros, trait `Provider`, provedores (OpenAI‑compatível/llama.cpp, Anthropic, Gemini, Scripted para testes), roteador de modelos, gerenciador do `llama-server`. | reqwest, serde |
| `jarvis-core` | Ciclo do agente, limites, plano, verificação, permissões, taint (injeção de prompt), auditoria, persistência SQLite, memória, configuração, workflows, agendador, trait `Tool`. | jarvis-llm, rusqlite |
| `jarvis-tools` | Ferramentas reais: arquivos, terminal, web, navegador, documentos, multimídia, memória, MCP, skills, ferramentas customizadas, voz, subagentes. Também o módulo `runtime`, que monta tudo a partir da configuração (usado pela CLI e pela GUI). | jarvis-core |
| `jarvis-cli` | Binário `jarvis`: chat, `run`, `resume`, histórico, memória, modelos, workflows, agendamentos, daemon. | todos |
| `jarvis-gui` | Binário `jarvis-gui` (egui/eframe): chat, histórico, seleção de modelo, plano, progresso, logs, aprovações, cancelamento. | todos |

## Ciclo do agente

1. **Planejar** – o modelo recebe a tarefa e é instruído a chamar `update_plan`
   com os passos. O plano é persistido e mostrado na interface.
2. **Executar** – a cada iteração o modelo devolve texto e/ou chamadas de
   ferramenta. Cada chamada passa por: validação de argumentos → política de
   permissão (com taint) → aprovação humana se necessário → execução com
   timeout e cancelamento → resultado truncado, com segredos removidos e
   conteúdo externo marcado como não confiável → auditoria.
3. **Verificar** – quando o modelo diz que terminou, o núcleo:
   - recusa a conclusão se houver passos do plano pendentes;
   - abre uma fase de verificação em que o modelo precisa usar ferramentas
     (ler o arquivo, rodar o teste, listar a saída) e emitir `VERDICT: PASS`
     ou `VERDICT: FAIL: ...`;
   - um `PASS` sem nenhuma chamada de ferramenta de verificação, depois de
     ações que mudam estado, é rejeitado uma vez;
   - `FAIL` volta para a execução (correção de erros).
4. **Persistir e retomar** – mensagens, plano, contadores e status são
   gravados no SQLite a cada passo. Sessões interrompidas ficam `running` ou
   `interrupted` e podem ser retomadas com `jarvis resume <id>`; chamadas de
   ferramenta sem resultado recebem um resultado sintético “interrompido —
   verifique o estado real”.

Limites (`[agent]`): iterações, chamadas ao modelo, chamadas de ferramenta,
tempo total, tokens totais, bytes de saída por ferramenta, timeout por
ferramenta, rodadas de verificação.

Janela de contexto: as definições de ferramentas são reduzidas às essenciais
quando ocupariam mais de 35% do contexto (`fit_tools`), e o histórico é
compactado (saídas antigas encurtadas, turnos antigos removidos sem separar
chamada e resultado) para caber no orçamento restante (`compact`).

## Modelos

`Provider` tem um único método `chat(ChatRequest) -> ChatResponse`. Tipos
neutros: `Message { role, parts }`, `Part::{Text, ToolCall, ToolResult, Image}`.

| Provedor | Uso | Observação |
|---|---|---|
| `openai_compat` | llama.cpp `llama-server` (local), Ollama, LM Studio, OpenAI | llama-server precisa de `--jinja` para tools. Padrão local: `http://127.0.0.1:9931/v1`. |
| `anthropic` | Claude Messages API | `x-api-key`, `anthropic-version: 2023-06-01`. |
| `gemini` | Gemini `generateContent` | `x-goog-api-key`; preserva `thoughtSignature`. |
| `scripted` | Testes determinísticos | Respostas pré-gravadas. |

O roteador escolhe o modelo por: escolha explícita → perfil da tarefa
(`fast`, `smart`, `code`, `long`) → disponibilidade (servidor local responde?
chave presente?) → recursos (RAM livre vs. tamanho do GGUF) → preferência por
local. Provedores remotos só são usados após consentimento registrado.

## Ferramentas

Cada ferramenta implementa `Tool` com `spec()` (nome, descrição, JSON
Schema), `risk(args)` e `call(args, ctx)`. Classes de risco: `Read`, `Write`,
`Execute`, `Network`, `Destructive`, `Admin`. A classe pode depender dos
argumentos (ex.: `shell` com `ls` é `Read`; com `rm -rf` é `Destructive`;
com `sudo` é `Admin`).

| Grupo | Ferramentas |
|---|---|
| Plano | `update_plan` |
| Arquivos | `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`, `delete_path`, `move_path` |
| Terminal | `shell`, `job_status`, `system_info`, `process_list` |
| Web | `web_search`, `web_fetch`, `browser` (Chromium headless) |
| Documentos | `doc_read` (PDF, DOCX, XLSX/ODS, CSV, PPTX, ODT, TeX, HTML), `doc_create` (DOCX, PDF, XLSX, CSV, PPTX, TeX, MD), `latex_compile`, `doc_convert` |
| Multimídia | `media_info`, `media_convert`, `image_edit`, `ocr`, `transcribe`, `speak` |
| Memória | `memory_save`, `memory_search`, `history_search` |
| Extensões | `mcp__<servidor>__<ferramenta>`, `skill_load`, ferramentas customizadas TOML |
| Subagentes | `delegate_task` |

## Workflows e agendamentos

- `jarvis_core::workflow`: workflows TOML com passos `tool` (passam pela mesma
  política de permissão via `Agent::call_tool`) e `task` (sessões filhas).
- Agendamentos cron ficam na tabela `schedules`; `jarvis daemon` verifica a
  cada 20 s e executa com aprovador não interativo (só ferramentas
  pré‑aprovadas; nunca destrutivas ou administrativas).

## Plataformas

`jarvis_llm::platform` concentra as diferenças entre sistemas: busca de executáveis (PATHEXT no
Windows, pastas de apps no macOS), `~`, caminhos canônicos sem `\\?\`, nome do SO, PIDs e memória
(`sysinfo`). O `shell` usa bash (Linux/macOS) ou PowerShell (Windows); o isolamento usa bubblewrap
(Linux) ou `sandbox-exec` (macOS).

## Empacotamento

- Linux: `scripts/build-appimage.sh` compila com `cargo zigbuild` para glibc 2.31,
monta o AppDir (`packaging/appimage`) e gera o AppImage. O `AppRun` abre a GUI
sem argumentos e a CLI com argumentos ou sem display.
- macOS: `scripts/build-macos.sh` gera binários universais (arm64 + x86_64), `Jarvis Core.app` e `.dmg`.
- Windows: `scripts/build-windows.ps1` gera um `.zip` com `jarvis.exe` e `jarvis-gui.exe` (ícone embutido).
- `.github/workflows/release.yml` roda os testes nos três sistemas, gera todos os pacotes e publica o
  release com `SHA256SUMS.txt`.

## Persistência

`~/.local/share/jarvis/jarvis.db` (SQLite, WAL):
`sessions`, `messages`, `memories` + `memories_fts` (FTS5), `schedules`,
`consents`. Auditoria em `~/.local/share/jarvis/audit.jsonl` com encadeamento
de hash. Configuração em `~/.config/jarvis/config.toml`.

## Interfaces

- `EventSink` – o núcleo emite `AgentEvent` (plano, chamada ao modelo, texto,
  ferramenta, resultado, verificação, limites, fim). CLI imprime; GUI desenha.
- `Approver` – decide pedidos de aprovação (`AllowOnce`, `AllowSession`,
  `Deny`). CLI pergunta no terminal; GUI abre diálogo; modo não interativo
  nega.
- `CancellationToken` – cancela modelo e ferramentas (processos filhos são
  mortos pelo grupo de processos).

## Critérios de aceitação

1. `cargo build --release` e `cargo test --workspace` passam.
2. `jarvis run "..."` com modelo local (llama-server) cria/edita arquivos e
   roda comandos reais, com aprovação.
3. Provedores Anthropic, Gemini e OpenAI testados contra servidores HTTP
   simulados com o formato real da API (sem chaves no ambiente de CI).
4. Ações de escrita, execução e rede pedem aprovação conforme a política;
   `sudo` nunca é aprovado automaticamente; caminhos fora do workspace são
   bloqueados.
5. Sessão interrompida pode ser retomada.
6. AppImage gerado e executado (`--appimage-extract-and-run`).
