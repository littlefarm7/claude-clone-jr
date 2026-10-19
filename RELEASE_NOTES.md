## Jarvis Core v0.2.0 — Linux, Windows e macOS

Agente de IA autônomo, local-first e open source (Apache-2.0). Planeja, executa e verifica tarefas com
ferramentas reais: arquivos, terminal, web, documentos, multimídia, memória, workflows, MCP e skills.

### Downloads

| Sistema | Arquivo |
|---|---|
| Linux x86_64 (glibc ≥ 2.31) | `Jarvis_Core-0.2.0-x86_64.AppImage` ou `…-linux-x86_64.tar.gz` |
| Windows 10/11 x64 | `Jarvis_Core-0.2.0-windows-x86_64.zip` |
| macOS 11+ (Apple Silicon e Intel) | `Jarvis_Core-0.2.0-macos-universal.dmg` ou `…-macos-universal.tar.gz` |

Confira os arquivos com `SHA256SUMS.txt`.

### Novidades

- Porte para **Windows** (PowerShell, Lixeira, SAPI, Edge/Chrome headless, ícone no executável) e
  **macOS** (binário universal, `.app` em `.dmg`, sandbox `sandbox-exec`, `say`, Lixeira do Finder).
- Diagnóstico de sistema e lista de processos multiplataforma.
- Leitor de PDF interno quando o poppler não está instalado.
- `jarvis doctor` mostra o comando de instalação certo para cada sistema (apt, brew, winget).
- O isolamento com bubblewrap é testado antes do uso; se o sistema proibir, o terminal segue sem ele e o
  `doctor` avisa.
- `--yes` nunca aprova ações destrutivas nem administrativas.
- CI com testes em Linux, macOS e Windows; release automatizado.

### Avisos

- Windows: executável sem assinatura comercial (SmartScreen pode avisar). Sem isolamento de processos.
- macOS: assinatura ad hoc, sem notarização. Abra com botão direito → Abrir na primeira vez.
- Modelos locais: instale o llama.cpp (`llama-server`) e um GGUF. Veja `docs/MODELOS-LOCAIS.md`.
