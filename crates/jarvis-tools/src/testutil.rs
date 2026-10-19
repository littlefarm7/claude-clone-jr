//! Helpers to build a `ToolContext` in tests and examples.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use jarvis_core::config::Config;
use jarvis_core::events::NullSink;
use jarvis_core::memory::{HashEmbedder, Memory};
use jarvis_core::permissions::PathGuard;
use jarvis_core::store::Store;
use jarvis_core::tool::ToolContext;
use tokio_util::sync::CancellationToken;

pub fn context(workspace: &Path) -> ToolContext {
    let mut cfg = Config::default();
    cfg.permissions.workspace_roots = vec![workspace.to_string_lossy().into()];
    context_with(cfg)
}

pub fn context_with(cfg: Config) -> ToolContext {
    let store = Arc::new(Store::in_memory().unwrap());
    ToolContext {
        session_id: "test".into(),
        cancel: CancellationToken::new(),
        guard: Arc::new(PathGuard::from_config(&cfg.permissions)),
        memory: Arc::new(Memory::new(store.clone(), Arc::new(HashEmbedder::default()))),
        store,
        timeout: Duration::from_secs(30),
        max_output: 20_000,
        events: Arc::new(NullSink),
        subagents: None,
        cfg: Arc::new(cfg),
    }
}
