//! Sub-agents: delegate a self-contained sub-task to a child agent.

use std::sync::Arc;

use anyhow::{bail, Result};
use async_trait::async_trait;
use jarvis_core::permissions::{Risk, RiskAssessment};
use jarvis_core::tool::{arg_str, Tool, ToolContext, ToolOutput, ToolRegistry};
use jarvis_llm::ToolSpec;
use serde_json::{json, Value};

pub fn register(r: &mut ToolRegistry) {
    r.register(Arc::new(DelegateTask));
}

pub struct DelegateTask;

#[async_trait]
impl Tool for DelegateTask {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "delegate_task".into(),
            description: "Delega uma subtarefa independente (ex.: pesquisar um tema, analisar uma pasta) a um subagente \
                com contexto próprio. Por padrão o subagente só tem ferramentas de leitura e pesquisa."
                .into(),
            parameters: json!({"type":"object","properties":{
                "task":{"type":"string"},
                "tools":{"type":"array","items":{"type":"string"},"description":"ferramentas permitidas (opcional)"}
            },"required":["task"]}),
        }
    }
    fn assess(&self, args: &Value, _: &ToolContext) -> RiskAssessment {
        // The child's own tool calls go through the normal permission checks.
        RiskAssessment::new(Risk::Read, format!("subagente: {}", arg_str(args, "task").unwrap_or("")))
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let Some(runner) = &ctx.subagents else { bail!("subagentes indisponíveis neste contexto") };
        let tools: Vec<String> = args["tools"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
        let out = runner.run_subagent(&ctx.session_id, arg_str(&args, "task").unwrap_or(""), tools, ctx.cancel.child_token()).await?;
        Ok(ToolOutput::ok(out))
    }
}
