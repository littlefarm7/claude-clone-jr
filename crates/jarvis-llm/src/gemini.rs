//! Google Gemini `models/{model}:generateContent` REST API.

use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::{http, split_system, ChatRequest, ChatResponse, Message, ModelConfig, Part, Provider, Role, StopReason, Usage};

pub struct Gemini {
    cfg: ModelConfig,
    key: String,
    client: reqwest::Client,
}

impl Gemini {
    pub fn new(cfg: ModelConfig, key: String) -> Self {
        Self { cfg, key, client: http::client(Duration::from_secs(600)) }
    }
}

/// Gemini accepts only a subset of JSON Schema; drop unsupported keywords.
pub fn sanitize_schema(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                if matches!(k.as_str(), "additionalProperties" | "$schema" | "default" | "examples") {
                    continue;
                }
                out.insert(k.clone(), sanitize_schema(val));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(sanitize_schema).collect()),
        other => other.clone(),
    }
}

pub fn to_wire(messages: &[Message]) -> (String, Vec<Value>) {
    let (system, rest) = split_system(messages);
    let mut out: Vec<Value> = Vec::new();
    for m in rest {
        let role = if m.role == Role::Assistant { "model" } else { "user" };
        let mut parts = Vec::new();
        for p in &m.parts {
            match p {
                Part::Text { text } if !text.is_empty() => parts.push(json!({"text": text})),
                Part::ToolCall { name, arguments, provider_meta, .. } => {
                    let mut part = json!({"functionCall": {"name": name, "args": arguments}});
                    if let Some(sig) = provider_meta.as_ref().and_then(|m| m.get("thoughtSignature")) {
                        part["thoughtSignature"] = sig.clone();
                    }
                    parts.push(part);
                }
                Part::ToolResult { name, content, is_error, .. } => {
                    let key = if *is_error { "error" } else { "output" };
                    parts.push(json!({"functionResponse": {"name": name, "response": {key: content}}}));
                }
                Part::Image { mime, data_b64 } => {
                    parts.push(json!({"inlineData": {"mimeType": mime, "data": data_b64}}))
                }
                _ => {}
            }
        }
        if parts.is_empty() {
            continue;
        }
        if let Some(last) = out.last_mut() {
            if last["role"] == role {
                last["parts"].as_array_mut().unwrap().extend(parts);
                continue;
            }
        }
        out.push(json!({"role": role, "parts": parts}));
    }
    (system, out)
}

pub fn parse_response(v: &Value) -> Result<ChatResponse> {
    let cand = v["candidates"].get(0).ok_or_else(|| anyhow!("resposta sem 'candidates': {v}"))?;
    let mut parts = Vec::new();
    if let Some(ps) = cand["content"]["parts"].as_array() {
        for p in ps {
            if p["thought"].as_bool() == Some(true) {
                continue;
            }
            if let Some(t) = p["text"].as_str() {
                parts.push(Part::Text { text: t.into() });
            } else if let Some(fc) = p.get("functionCall") {
                let meta = p.get("thoughtSignature").map(|s| json!({"thoughtSignature": s}));
                parts.push(Part::ToolCall {
                    id: fc["id"].as_str().map(String::from).unwrap_or_else(crate::new_call_id),
                    name: fc["name"].as_str().unwrap_or("").into(),
                    arguments: if fc["args"].is_object() { fc["args"].clone() } else { json!({}) },
                    provider_meta: meta,
                });
            }
        }
    }
    let has_calls = parts.iter().any(|p| matches!(p, Part::ToolCall { .. }));
    let stop = match cand["finishReason"].as_str() {
        _ if has_calls => StopReason::ToolUse,
        Some("STOP") => StopReason::EndTurn,
        Some("MAX_TOKENS") => StopReason::MaxTokens,
        _ => StopReason::Other,
    };
    let usage = Usage {
        input_tokens: v["usageMetadata"]["promptTokenCount"].as_u64().unwrap_or(0),
        output_tokens: v["usageMetadata"]["candidatesTokenCount"].as_u64().unwrap_or(0),
    };
    Ok(ChatResponse { message: Message { role: Role::Assistant, parts }, usage, stop })
}

#[async_trait]
impl Provider for Gemini {
    fn id(&self) -> String {
        format!("gemini:{}", self.cfg.model)
    }

    fn is_local(&self) -> bool {
        false
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let (system, contents) = to_wire(&req.messages);
        let mut gen = json!({"maxOutputTokens": req.max_tokens});
        if let Some(t) = req.temperature.or(self.cfg.temperature) {
            gen["temperature"] = json!(t);
        }
        let mut body = json!({"contents": contents, "generationConfig": gen});
        if !system.is_empty() {
            body["systemInstruction"] = json!({"parts": [{"text": system}]});
        }
        if !req.tools.is_empty() {
            let decls: Vec<Value> = req
                .tools
                .iter()
                .map(|t| json!({"name": t.name, "description": t.description, "parameters": sanitize_schema(&t.parameters)}))
                .collect();
            body["tools"] = json!([{"functionDeclarations": decls}]);
        }
        let url = format!(
            "{}/models/{}:generateContent",
            self.cfg.base_url().trim_end_matches('/'),
            self.cfg.model
        );
        let headers = [("x-goog-api-key", self.key.clone())];
        let v = http::post_json(&self.client, &url, &headers, &body).await?;
        parse_response(&v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_roundtrip() {
        let v = json!({"candidates":[{"finishReason":"STOP","content":{"role":"model","parts":[
            {"functionCall":{"name":"shell","args":{"command":"ls"}},"thoughtSignature":"SIG"}]}}]});
        let r = parse_response(&v).unwrap();
        assert_eq!(r.stop, StopReason::ToolUse);
        let (_, w) = to_wire(&[r.message]);
        assert_eq!(w[0]["role"], "model");
        assert_eq!(w[0]["parts"][0]["thoughtSignature"], "SIG");
    }

    #[test]
    fn schema_sanitized() {
        let s = sanitize_schema(&json!({"type":"object","additionalProperties":false,"properties":{"a":{"type":"string","default":"x"}}}));
        assert!(s.get("additionalProperties").is_none());
        assert!(s["properties"]["a"].get("default").is_none());
    }
}
