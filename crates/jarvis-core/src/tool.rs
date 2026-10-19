//! Tool trait, execution context and registry.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use jarvis_llm::ToolSpec;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::events::EventSink;
use crate::memory::Memory;
use crate::permissions::{PathGuard, RiskAssessment};
use crate::store::Store;

#[derive(Debug, Clone, Default)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    /// Set when the content came from outside (web, documents, MCP...).
    pub untrusted_source: Option<String>,
    /// Files created or modified, for the UI.
    pub artifacts: Vec<PathBuf>,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            ..Default::default()
        }
    }
    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            ..Default::default()
        }
    }
    pub fn untrusted(mut self, source: impl Into<String>) -> Self {
        self.untrusted_source = Some(source.into());
        self
    }
    pub fn with_artifact(mut self, p: PathBuf) -> Self {
        self.artifacts.push(p);
        self
    }
}

/// Runs a child agent (implemented by the agent itself).
#[async_trait]
pub trait SubagentRunner: Send + Sync {
    async fn run_subagent(
        &self,
        parent_session: &str,
        task: &str,
        tools: Vec<String>,
        cancel: CancellationToken,
    ) -> Result<String>;
}

#[derive(Clone)]
pub struct ToolContext {
    pub session_id: String,
    pub cancel: CancellationToken,
    pub guard: Arc<PathGuard>,
    pub cfg: Arc<Config>,
    pub store: Arc<Store>,
    pub memory: Arc<Memory>,
    pub timeout: Duration,
    pub max_output: usize,
    pub events: Arc<dyn EventSink>,
    pub subagents: Option<Arc<dyn SubagentRunner>>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    /// Classify this concrete call. Must not have side effects.
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment;
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput>;
}

#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.spec().name, tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    /// Restrict to a subset (sub-agents, scheduled jobs).
    pub fn subset(&self, names: &[String]) -> Self {
        Self {
            tools: self
                .tools
                .iter()
                .filter(|(k, _)| names.iter().any(|n| n == *k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// Minimal JSON-Schema check: object shape, required keys and primitive types.
pub fn validate_args(schema: &Value, args: &Value) -> Result<(), String> {
    if !args.is_object() {
        return Err("os argumentos devem ser um objeto JSON".into());
    }
    if let Some(req) = schema["required"].as_array() {
        for r in req.iter().filter_map(|r| r.as_str()) {
            if args.get(r).map(|v| v.is_null()).unwrap_or(true) {
                return Err(format!("argumento obrigatório ausente: '{r}'"));
            }
        }
    }
    if let Some(props) = schema["properties"].as_object() {
        for (k, v) in args.as_object().unwrap() {
            let Some(p) = props.get(k) else { continue };
            let ok = match p["type"].as_str() {
                Some("string") => v.is_string(),
                Some("integer") => v.is_i64() || v.is_u64(),
                Some("number") => v.is_number(),
                Some("boolean") => v.is_boolean(),
                Some("array") => v.is_array(),
                Some("object") => v.is_object(),
                _ => true,
            };
            if !ok && !v.is_null() {
                return Err(format!("argumento '{k}' deveria ser do tipo {}", p["type"]));
            }
            if let (Some(en), Some(s)) = (p["enum"].as_array(), v.as_str()) {
                if !en.iter().any(|e| e.as_str() == Some(s)) {
                    return Err(format!("argumento '{k}' deve ser um de {}", p["enum"]));
                }
            }
        }
    }
    Ok(())
}

/// Helpers for reading arguments.
pub fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

pub fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

pub fn arg_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validation() {
        let s = json!({"type":"object","properties":{"a":{"type":"string"},"n":{"type":"integer"},"m":{"type":"string","enum":["x","y"]}},"required":["a"]});
        assert!(validate_args(&s, &json!({"a":"ok","n":3})).is_ok());
        assert!(validate_args(&s, &json!({"n":3})).is_err());
        assert!(validate_args(&s, &json!({"a":1})).is_err());
        assert!(validate_args(&s, &json!({"a":"x","m":"z"})).is_err());
        assert!(validate_args(&s, &json!("str")).is_err());
    }
}
