//! Memory tools (user-controlled, inspectable with `claudio memory list`).

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use claudio_core::permissions::{Risk, RiskAssessment};
use claudio_core::tool::{arg_str, arg_u64, Tool, ToolContext, ToolOutput, ToolRegistry};
use claudio_llm::ToolSpec;
use serde_json::{json, Value};

pub fn register(r: &mut ToolRegistry) {
    r.register(Arc::new(MemorySave));
    r.register(Arc::new(MemorySearch));
    r.register(Arc::new(HistorySearch));
}

pub struct MemorySave;

#[async_trait]
impl Tool for MemorySave {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memory_save".into(),
            description: "Guarda um fato ou preferência do usuário para sessões futuras. Use só quando o usuário pedir ou \
                for claramente uma preferência duradoura. Nunca guarde senhas ou chaves."
                .into(),
            parameters: json!({"type":"object","properties":{
                "content":{"type":"string"},
                "kind":{"type":"string","enum":["fact","preference","note"]},
                "tags":{"type":"string"}
            },"required":["content"]}),
        }
    }
    fn assess(&self, args: &Value, _: &ToolContext) -> RiskAssessment {
        let c = arg_str(args, "content").unwrap_or("");
        let ra = RiskAssessment::new(Risk::Write, format!("memorizar: {c}"));
        if claudio_core::safety::redact(c) != c {
            return ra.deny("o conteúdo parece conter um segredo; memória não guarda credenciais");
        }
        ra
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let id = ctx
            .memory
            .save(
                arg_str(&args, "kind").unwrap_or("note"),
                arg_str(&args, "content").unwrap_or(""),
                arg_str(&args, "tags").unwrap_or(""),
                &format!("session:{}", ctx.session_id),
            )
            .await?;
        Ok(ToolOutput::ok(format!(
            "Memória #{id} salva. O usuário pode removê-la com `claudio memory delete {id}`."
        )))
    }
}

pub struct MemorySearch;

#[async_trait]
impl Tool for MemorySearch {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memory_search".into(),
            description: "Pesquisa nas memórias do usuário (busca híbrida: texto + vetores)."
                .into(),
            parameters: json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer"}},"required":["query"]}),
        }
    }
    fn assess(&self, _: &Value, _: &ToolContext) -> RiskAssessment {
        RiskAssessment::new(Risk::Read, "pesquisar memórias")
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let hits = ctx
            .memory
            .search(
                arg_str(&args, "query").unwrap_or(""),
                arg_u64(&args, "limit").unwrap_or(8) as usize,
            )
            .await?;
        if hits.is_empty() {
            return Ok(ToolOutput::ok("Nenhuma memória relevante."));
        }
        Ok(ToolOutput::ok(
            hits.iter()
                .map(|h| {
                    format!(
                        "#{} [{}] ({:.2}) {} {}",
                        h.record.id,
                        h.record.kind,
                        h.score,
                        h.record.content,
                        if h.record.tags.is_empty() {
                            String::new()
                        } else {
                            format!("[{}]", h.record.tags)
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ))
    }
}

pub struct HistorySearch;

#[async_trait]
impl Tool for HistorySearch {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "history_search".into(),
            description: "Pesquisa no histórico de conversas anteriores.".into(),
            parameters: json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer"}},"required":["query"]}),
        }
    }
    fn assess(&self, _: &Value, _: &ToolContext) -> RiskAssessment {
        RiskAssessment::new(Risk::Read, "pesquisar histórico")
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let rows = ctx.store.search_history(
            arg_str(&args, "query").unwrap_or(""),
            arg_u64(&args, "limit").unwrap_or(10) as usize,
        )?;
        if rows.is_empty() {
            return Ok(ToolOutput::ok("Nada encontrado no histórico."));
        }
        Ok(ToolOutput::ok(
            rows.iter()
                .map(|(s, snip)| format!("[{}] {}", &s[..8.min(s.len())], snip))
                .collect::<Vec<_>>()
                .join("\n"),
        ))
    }
}
