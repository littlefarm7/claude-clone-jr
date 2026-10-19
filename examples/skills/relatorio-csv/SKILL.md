---
name: relatorio-csv
description: Padrão da casa para relatórios a partir de planilhas CSV/XLSX
---
1. Leia cada planilha com `doc_read` (use `max_rows` 50 para uma amostra).
2. Calcule totais com `shell` usando ferramentas padrão (`awk`, `python3`) quando precisar de números exatos.
3. Crie o relatório com `doc_create` em PDF, com título, data e uma seção por arquivo.
4. Verifique relendo o PDF com `doc_read` antes de concluir.
