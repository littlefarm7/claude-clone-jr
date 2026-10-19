# Extensões: MCP, skills e ferramentas customizadas

## Servidores MCP (stdio)

```toml
[[mcp_servers]]
name = "fs-extra"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/eu/projetos"]
# env = { CHAVE = "valor" }
```

- As ferramentas aparecem como `mcp__<servidor>__<ferramenta>`.
- Risco padrão: execução, então pedem aprovação.
- A saída é tratada como conteúdo externo não confiável.
- O processo recebe o ambiente sem variáveis de segredo (`*_API_KEY`, `*_TOKEN`...), mais o `env` configurado.

## Skills

Uma skill é uma pasta com `SKILL.md` em `~/.config/claudio/skills/` (ou nas pastas de `skills_dirs`):

```markdown
---
name: relatorio-mensal
description: Como montar o relatório mensal de vendas
---
1. Leia os CSV de ~/claudio-work/vendas.
2. ...
```

O catálogo (nome + descrição) entra no prompt do sistema. O agente carrega o conteúdo com `skill_load` quando
precisa. Exemplo em [examples/skills](../examples/skills).

## Ferramentas customizadas

Transforme qualquer programa instalado em ferramenta, sem shell (os argumentos são substituídos um a um, sem
interpretação):

```toml
[[custom_tools]]
name = "otimizar_pdf"
description = "Reduz o tamanho de um PDF com ghostscript"
command = ["gs", "-sDEVICE=pdfwrite", "-dPDFSETTINGS=/ebook", "-o", "{saida}", "{entrada}"]
params = { entrada = "PDF de entrada", saida = "PDF de saída" }
risk = "write"     # read | network | write | execute | destructive
```
