//! OpenAI Chat Completions format. Also used for llama.cpp `llama-server`,
//! Ollama, LM Studio and vLLM, which expose the same API.

use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::{http, ChatRequest, ChatResponse, Message, ModelConfig, Part, Provider, ProviderKind, Role, StopReason, Usage};

pub struct OpenAiCompat {
    cfg: ModelConfig,
    key: Option<String>,
    client: reqwest::Client,
}

impl OpenAiCompat {
    pub fn new(cfg: ModelConfig, key: Option<String>) -> Self {
        // Local CPU inference can be slow; allow long requests.
        let timeout = if cfg.is_local() { 900 } else { 300 };
        Self { cfg, key, client: http::client(Duration::from_secs(timeout)) }
    }
}

pub fn to_wire_messages(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in messages {
        match m.role {
            Role::System => out.push(json!({"role": "system", "content": m.text_content()})),
            Role::User => {
                let has_image = m.parts.iter().any(|p| matches!(p, Part::Image { .. }));
                if has_image {
                    let content: Vec<Value> = m
                        .parts
                        .iter()
                        .filter_map(|p| match p {
                            Part::Text { text } => Some(json!({"type": "text", "text": text})),
                            Part::Image { mime, data_b64 } => Some(json!({
                                "type": "image_url",
                                "image_url": {"url": format!("data:{mime};base64,{data_b64}")}
                            })),
                            _ => None,
                        })
                        .collect();
                    out.push(json!({"role": "user", "content": content}));
                } else {
                    out.push(json!({"role": "user", "content": m.text_content()}));
                }
                // Tool results may also be carried in a user message by some callers.
                push_tool_results(m, &mut out);
            }
            Role::Assistant => {
                let text = m.text_content();
                let calls: Vec<Value> = m
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::ToolCall { id, name, arguments, .. } => Some(json!({
                            "id": id,
                            "type": "function",
                            "function": {"name": name, "arguments": arguments.to_string()}
                        })),
                        _ => None,
                    })
                    .collect();
                let mut msg = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { Value::String(text) }});
                if !calls.is_empty() {
                    msg["tool_calls"] = Value::Array(calls);
                }
                out.push(msg);
            }
            Role::Tool => push_tool_results(m, &mut out),
        }
    }
    out
}

fn push_tool_results(m: &Message, out: &mut Vec<Value>) {
    for p in &m.parts {
        if let Part::ToolResult { call_id, content, .. } = p {
            out.push(json!({"role": "tool", "tool_call_id": call_id, "content": content}));
        }
    }
}

pub fn parse_response(v: &Value) -> Result<ChatResponse> {
    let choice = v["choices"].get(0).ok_or_else(|| anyhow!("resposta sem 'choices': {v}"))?;
    let msg = &choice["message"];
    let mut parts = Vec::new();
    let mut text = msg["content"].as_str().unwrap_or("").to_string();
    if let Some(calls) = msg["tool_calls"].as_array() {
        for c in calls {
            let name = c["function"]["name"].as_str().unwrap_or("").to_string();
            let args_raw = &c["function"]["arguments"];
            let arguments = match args_raw {
                Value::String(s) if s.trim().is_empty() => json!({}),
                Value::String(s) => serde_json::from_str(s).unwrap_or(Value::String(s.clone())),
                Value::Object(_) => args_raw.clone(),
                _ => json!({}),
            };
            let id = c["id"].as_str().map(String::from).unwrap_or_else(crate::new_call_id);
            parts.push(Part::ToolCall { id, name, arguments, provider_meta: None });
        }
    }
    if parts.is_empty() {
        // Fallback for templates that leave Hermes/Qwen-style tags in the text.
        let (rest, calls) = extract_tagged_tool_calls(&text);
        if !calls.is_empty() {
            text = rest;
            parts.extend(calls);
        }
    }
    if !text.trim().is_empty() {
        parts.insert(0, Part::Text { text });
    }
    let stop = match choice["finish_reason"].as_str() {
        Some("tool_calls") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        Some("stop") => {
            if parts.iter().any(|p| matches!(p, Part::ToolCall { .. })) {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            }
        }
        _ => StopReason::Other,
    };
    let usage = Usage {
        input_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
        output_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0),
    };
    Ok(ChatResponse { message: Message { role: Role::Assistant, parts }, usage, stop })
}

/// Extract `<tool_call>{"name":..,"arguments":..}</tool_call>` blocks.
pub fn extract_tagged_tool_calls(text: &str) -> (String, Vec<Part>) {
    let mut rest = String::new();
    let mut calls = Vec::new();
    let mut cursor = text;
    while let Some(start) = cursor.find("<tool_call>") {
        rest.push_str(&cursor[..start]);
        let after = &cursor[start + "<tool_call>".len()..];
        let Some(end) = after.find("</tool_call>") else {
            rest.push_str(&cursor[start..]);
            cursor = "";
            break;
        };
        let body = after[..end].trim();
        if let Ok(v) = serde_json::from_str::<Value>(body) {
            if let Some(name) = v["name"].as_str() {
                let arguments = match &v["arguments"] {
                    Value::String(s) => serde_json::from_str(s).unwrap_or(json!({})),
                    Value::Null => json!({}),
                    a => a.clone(),
                };
                calls.push(Part::ToolCall {
                    id: crate::new_call_id(),
                    name: name.to_string(),
                    arguments,
                    provider_meta: None,
                });
            }
        }
        cursor = &after[end + "</tool_call>".len()..];
    }
    rest.push_str(cursor);
    (rest.trim().to_string(), calls)
}

#[async_trait]
impl Provider for OpenAiCompat {
    fn id(&self) -> String {
        format!("{}:{}", if self.cfg.is_local() { "local" } else { "openai" }, self.cfg.model)
    }

    fn is_local(&self) -> bool {
        self.cfg.is_local()
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let mut body = json!({
            "model": self.cfg.model,
            "messages": to_wire_messages(&req.messages),
        });
        if self.cfg.provider == ProviderKind::Openai {
            body["max_completion_tokens"] = json!(req.max_tokens);
        } else {
            body["max_tokens"] = json!(req.max_tokens);
        }
        if let Some(t) = req.temperature.or(self.cfg.temperature) {
            body["temperature"] = json!(t);
        }
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.parameters}}))
                    .collect(),
            );
        }
        let mut headers = Vec::new();
        if let Some(k) = &self.key {
            headers.push(("authorization", format!("Bearer {k}")));
        }
        let url = format!("{}/chat/completions", self.cfg.base_url().trim_end_matches('/'));
        let v = http::post_json(&self.client, &url, &headers, &body).await?;
        parse_response(&v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagged_calls() {
        let (rest, calls) = extract_tagged_tool_calls(
            "ok <tool_call>{\"name\":\"read_file\",\"arguments\":{\"path\":\"a\"}}</tool_call>",
        );
        assert_eq!(rest, "ok");
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn parse_tool_call_response() {
        let v = json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"shell","arguments":"{\"command\":\"ls\"}"}}]}}],
            "usage":{"prompt_tokens":10,"completion_tokens":5}});
        let r = parse_response(&v).unwrap();
        assert_eq!(r.stop, StopReason::ToolUse);
        let calls = r.message.tool_calls();
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "ls");
        assert_eq!(r.usage.total(), 15);
    }

    #[test]
    fn wire_roundtrip() {
        let msgs = vec![
            Message::system("s"),
            Message::user("u"),
            Message {
                role: Role::Assistant,
                parts: vec![Part::ToolCall { id: "c1".into(), name: "t".into(), arguments: json!({"a":1}), provider_meta: None }],
            },
            Message {
                role: Role::Tool,
                parts: vec![Part::ToolResult { call_id: "c1".into(), name: "t".into(), content: "r".into(), is_error: false }],
            },
        ];
        let w = to_wire_messages(&msgs);
        assert_eq!(w.len(), 4);
        assert_eq!(w[2]["tool_calls"][0]["function"]["arguments"], "{\"a\":1}");
        assert_eq!(w[3]["role"], "tool");
    }
}
