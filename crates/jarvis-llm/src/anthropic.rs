//! Anthropic Messages API (`POST /v1/messages`).

use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::{http, split_system, ChatRequest, ChatResponse, Message, ModelConfig, Part, Provider, Role, StopReason, Usage};

pub const API_VERSION: &str = "2023-06-01";

pub struct Anthropic {
    cfg: ModelConfig,
    key: String,
    client: reqwest::Client,
}

impl Anthropic {
    pub fn new(cfg: ModelConfig, key: String) -> Self {
        Self { cfg, key, client: http::client(Duration::from_secs(600)) }
    }
}

fn block(p: &Part) -> Option<Value> {
    Some(match p {
        Part::Text { text } if text.is_empty() => return None,
        Part::Text { text } => json!({"type": "text", "text": text}),
        Part::ToolCall { id, name, arguments, .. } => {
            let input = if arguments.is_object() { arguments.clone() } else { json!({}) };
            json!({"type": "tool_use", "id": id, "name": name, "input": input})
        }
        Part::ToolResult { call_id, content, is_error, .. } => json!({
            "type": "tool_result", "tool_use_id": call_id, "content": content, "is_error": is_error
        }),
        Part::Image { mime, data_b64 } => json!({
            "type": "image", "source": {"type": "base64", "media_type": mime, "data": data_b64}
        }),
    })
}

pub fn to_wire(messages: &[Message]) -> (String, Vec<Value>) {
    let (system, rest) = split_system(messages);
    let mut out: Vec<Value> = Vec::new();
    for m in rest {
        let role = match m.role {
            Role::Assistant => "assistant",
            _ => "user", // tool results travel in user turns
        };
        let blocks: Vec<Value> = m.parts.iter().filter_map(block).collect();
        if blocks.is_empty() {
            continue;
        }
        // Merge consecutive turns of the same role.
        if let Some(last) = out.last_mut() {
            if last["role"] == role {
                last["content"].as_array_mut().unwrap().extend(blocks);
                continue;
            }
        }
        out.push(json!({"role": role, "content": blocks}));
    }
    (system, out)
}

pub fn parse_response(v: &Value) -> Result<ChatResponse> {
    let content = v["content"].as_array().ok_or_else(|| anyhow!("resposta sem 'content': {v}"))?;
    let mut parts = Vec::new();
    for b in content {
        match b["type"].as_str() {
            Some("text") => parts.push(Part::Text { text: b["text"].as_str().unwrap_or("").into() }),
            Some("tool_use") => parts.push(Part::ToolCall {
                id: b["id"].as_str().unwrap_or("").into(),
                name: b["name"].as_str().unwrap_or("").into(),
                arguments: b["input"].clone(),
                provider_meta: None,
            }),
            _ => {}
        }
    }
    let stop = match v["stop_reason"].as_str() {
        Some("end_turn") | Some("stop_sequence") => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        _ => StopReason::Other,
    };
    let usage = Usage {
        input_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
    };
    Ok(ChatResponse { message: Message { role: Role::Assistant, parts }, usage, stop })
}

#[async_trait]
impl Provider for Anthropic {
    fn id(&self) -> String {
        format!("anthropic:{}", self.cfg.model)
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let (system, messages) = to_wire(&req.messages);
        let mut body = json!({
            "model": self.cfg.model,
            "max_tokens": req.max_tokens,
            "messages": messages,
        });
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        if let Some(t) = req.temperature.or(self.cfg.temperature) {
            body["temperature"] = json!(t);
        }
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.parameters}))
                    .collect(),
            );
        }
        let url = format!("{}/v1/messages", self.cfg.base_url().trim_end_matches('/'));
        let headers = [("x-api-key", self.key.clone()), ("anthropic-version", API_VERSION.to_string())];
        let v = http::post_json(&self.client, &url, &headers, &body).await?;
        parse_response(&v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_tool_results_into_user_turn() {
        let msgs = vec![
            Message::system("sys"),
            Message::user("hi"),
            Message {
                role: Role::Assistant,
                parts: vec![Part::ToolCall { id: "t1".into(), name: "x".into(), arguments: json!({}), provider_meta: None }],
            },
            Message {
                role: Role::Tool,
                parts: vec![Part::ToolResult { call_id: "t1".into(), name: "x".into(), content: "ok".into(), is_error: false }],
            },
            Message::user("next"),
        ];
        let (sys, w) = to_wire(&msgs);
        assert_eq!(sys, "sys");
        assert_eq!(w.len(), 3);
        assert_eq!(w[2]["content"][0]["type"], "tool_result");
        assert_eq!(w[2]["content"][1]["type"], "text");
    }
}
