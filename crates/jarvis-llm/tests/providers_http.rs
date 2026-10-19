//! Each provider is exercised against a local HTTP server that checks the
//! request shape of the real API and answers with a real-format response.

use std::sync::{Arc, Mutex};

use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
use jarvis_llm::*;
use serde_json::{json, Value};

type Seen = Arc<Mutex<Vec<(HeaderMap, Value)>>>;

async fn serve(path: &str, reply: Value) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let s2 = seen.clone();
    let app = Router::new()
        .route(
            path,
            post(move |State(seen): State<Seen>, headers: HeaderMap, Json(body): Json<Value>| {
                let reply = reply.clone();
                async move {
                    seen.lock().unwrap().push((headers, body));
                    Json(reply)
                }
            }),
        )
        .with_state(s2);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn cfg(provider: ProviderKind, base: String) -> ModelConfig {
    ModelConfig {
        name: "t".into(),
        provider,
        model: "test-model".into(),
        base_url: Some(base),
        api_key_env: None,
        api_key: Some("sk-test".into()),
        context_window: 8192,
        max_output_tokens: 256,
        tags: vec![],
        gguf_path: None,
        temperature: None,
    }
}

fn req() -> ChatRequest {
    ChatRequest {
        messages: vec![Message::system("be brief"), Message::user("list files")],
        tools: vec![ToolSpec {
            name: "shell".into(),
            description: "run".into(),
            parameters: json!({"type":"object","properties":{"command":{"type":"string"}},"required":["command"],"additionalProperties":false}),
        }],
        max_tokens: 100,
        temperature: None,
    }
}

#[tokio::test]
async fn anthropic_roundtrip() {
    let (base, seen) = serve("/v1/messages", json!({
        "id":"msg_1","type":"message","role":"assistant","model":"test-model",
        "content":[{"type":"text","text":"ok"},{"type":"tool_use","id":"toolu_1","name":"shell","input":{"command":"ls"}}],
        "stop_reason":"tool_use","usage":{"input_tokens":12,"output_tokens":7}})).await;
    let p = build_provider(&cfg(ProviderKind::Anthropic, base)).unwrap();
    let r = p.chat(req()).await.unwrap();
    assert_eq!(r.stop, StopReason::ToolUse);
    assert_eq!(r.message.tool_calls()[0].arguments["command"], "ls");
    let (h, body) = seen.lock().unwrap()[0].clone();
    assert_eq!(h["x-api-key"], "sk-test");
    assert_eq!(h["anthropic-version"], "2023-06-01");
    assert_eq!(body["system"], "be brief");
    assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
    assert_eq!(body["messages"][0]["role"], "user");
}

#[tokio::test]
async fn gemini_roundtrip() {
    let (base, seen) = serve("/models/{model}", json!({
        "candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"shell","args":{"command":"ls"}}}]},"finishReason":"STOP"}],
        "usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":4}})).await;
    let p = build_provider(&cfg(ProviderKind::Gemini, base)).unwrap();
    let r = p.chat(req()).await.unwrap();
    assert_eq!(r.message.tool_calls()[0].name, "shell");
    assert_eq!(r.usage.total(), 7);
    let (h, body) = seen.lock().unwrap()[0].clone();
    assert_eq!(h["x-goog-api-key"], "sk-test");
    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be brief");
    assert!(body["tools"][0]["functionDeclarations"][0]["parameters"].get("additionalProperties").is_none());
}

#[tokio::test]
async fn openai_roundtrip() {
    let (base, seen) = serve("/v1/chat/completions", json!({
        "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"hello"}}],
        "usage":{"prompt_tokens":5,"completion_tokens":1}})).await;
    let p = build_provider(&cfg(ProviderKind::Openai, format!("{base}/v1"))).unwrap();
    let r = p.chat(req()).await.unwrap();
    assert_eq!(r.message.text_content(), "hello");
    let (h, body) = seen.lock().unwrap()[0].clone();
    assert_eq!(h["authorization"], "Bearer sk-test");
    assert_eq!(body["max_completion_tokens"], 100);
    assert_eq!(body["tools"][0]["function"]["name"], "shell");
}

#[tokio::test]
async fn missing_key_is_an_error() {
    let mut c = cfg(ProviderKind::Anthropic, "http://127.0.0.1:1".into());
    c.api_key = None;
    c.api_key_env = Some("JARVIS_NO_SUCH_ENV".into());
    assert!(build_provider(&c).is_err());
}
