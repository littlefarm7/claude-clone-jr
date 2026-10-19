//! User-defined tools from `[[custom_tools]]`: argv templates, executed without a shell.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use jarvis_core::config::{Config, CustomToolConfig};
use jarvis_core::permissions::{Risk, RiskAssessment};
use jarvis_core::tool::{Tool, ToolContext, ToolOutput, ToolRegistry};
use jarvis_llm::ToolSpec;
use serde_json::{json, Value};

pub fn register(reg: &mut ToolRegistry, cfg: &Config, warnings: &mut Vec<String>) {
    for t in &cfg.custom_tools {
        if t.command.is_empty() {
            warnings.push(format!("custom tool '{}' sem comando", t.name));
            continue;
        }
        if reg.get(&t.name).is_some() {
            warnings.push(format!(
                "custom tool '{}' conflita com uma ferramenta existente",
                t.name
            ));
            continue;
        }
        reg.register(Arc::new(CustomTool(t.clone())));
    }
}

pub fn parse_risk(s: &str) -> Risk {
    match s.to_lowercase().as_str() {
        "read" => Risk::Read,
        "network" => Risk::Network,
        "write" => Risk::Write,
        "destructive" => Risk::Destructive,
        "admin" => Risk::Admin,
        _ => Risk::Execute,
    }
}

pub struct CustomTool(pub CustomToolConfig);

impl CustomTool {
    pub fn argv(&self, args: &Value) -> Vec<String> {
        self.0
            .command
            .iter()
            .map(|part| {
                let mut s = part.clone();
                for k in self.0.params.keys() {
                    let v = match args.get(k) {
                        Some(Value::String(x)) => x.clone(),
                        Some(Value::Null) | None => String::new(),
                        Some(other) => other.to_string(),
                    };
                    s = s.replace(&format!("{{{k}}}"), &v);
                }
                s
            })
            .collect()
    }
}

#[async_trait]
impl Tool for CustomTool {
    fn spec(&self) -> ToolSpec {
        let props: serde_json::Map<String, Value> = self
            .0
            .params
            .iter()
            .map(|(k, d)| (k.clone(), json!({"type":"string","description": d})))
            .collect();
        ToolSpec {
            name: self.0.name.clone(),
            description: self.0.description.clone(),
            parameters: json!({"type":"object","properties": props, "required": self.0.params.keys().collect::<Vec<_>>()}),
        }
    }
    fn assess(&self, args: &Value, _: &ToolContext) -> RiskAssessment {
        RiskAssessment::new(
            parse_risk(&self.0.risk),
            format!("$ {}", shell_words::join(self.argv(args))),
        )
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let argv = self.argv(&args);
        let cmd = crate::shell::build_command(&argv[0], &argv[1..], &ctx.guard.cwd(), ctx, true);
        let res =
            crate::shell::run_command(cmd, Duration::from_secs(ctx.timeout.as_secs()), ctx).await?;
        let mut out = ToolOutput::ok(res.render(ctx.max_output));
        out.is_error = !res.ok();
        Ok(out)
    }
}
