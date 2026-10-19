//! Integration with a real llama.cpp `llama-server`.
//!
//! Runs only when JARVIS_TEST_GGUF points to a GGUF file and `llama-server`
//! is on PATH (or JARVIS_TEST_LLAMA_SERVER is set). In CI we use a tiny
//! random-weight model built by scripts/make_tiny_gguf.py: it validates the
//! plumbing (autostart, health, chat template with tools, HTTP), not quality.

use std::sync::Arc;

use jarvis_core::config::Config;
use jarvis_core::events::{AutoApprove, VecSink};
use jarvis_core::store::SessionStatus;
use jarvis_llm::{ModelConfig, ProviderKind};
use jarvis_tools::runtime::Runtime;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn autostart_llama_server_and_run_agent() {
    let Ok(gguf) = std::env::var("JARVIS_TEST_GGUF") else {
        eprintln!("ignorado: defina JARVIS_TEST_GGUF");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::default();
    cfg.permissions.workspace_roots = vec![dir.path().join("ws").to_string_lossy().into()];
    cfg.local.llama_server = std::env::var("JARVIS_TEST_LLAMA_SERVER").ok();
    cfg.agent.max_iterations = 3;
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    cfg.models = vec![ModelConfig {
        name: "tiny".into(),
        provider: ProviderKind::OpenaiCompat,
        model: "tiny".into(),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        api_key_env: None,
        api_key: None,
        context_window: 2048,
        max_output_tokens: 16,
        tags: vec![],
        gguf_path: Some(gguf),
        temperature: Some(0.0),
    }];
    cfg.default_model = Some("tiny".into());
    let rt = Runtime::init_at(cfg, dir.path().join("data")).await.unwrap();
    let avail = rt.availability().await;
    assert_eq!(avail["tiny"], jarvis_llm::router::Availability::Startable);
    let sel = rt.select_model(None, "oi", &AutoApprove).await.unwrap();
    assert!(sel.provider.is_local());
    assert!(jarvis_llm::local::health(&format!("http://127.0.0.1:{port}/v1")).await, "servidor deveria estar de pé");
    let events = Arc::new(VecSink::default());
    let agent = rt.agent(&sel, Arc::new(AutoApprove), events.clone());
    let out = agent.start("diga oi", CancellationToken::new()).await.unwrap();
    // Random weights: we only require a well-formed round trip through the real server.
    assert!(
        matches!(out.status, SessionStatus::Completed | SessionStatus::LimitReached | SessionStatus::CompletedUnverified),
        "{:?}: {}",
        out.status,
        out.summary
    );
    let msgs = rt.store.messages(&out.session_id).unwrap();
    assert!(msgs.len() >= 3, "deveria haver resposta do modelo");
    rt.shutdown().await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!jarvis_llm::local::health(&format!("http://127.0.0.1:{port}/v1")).await, "servidor deveria ter parado");
}
