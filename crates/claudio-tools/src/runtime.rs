//! Shared wiring used by the CLI and the GUI: config → store, tools, model, agent.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use claudio_core::audit::Audit;
use claudio_core::config::{data_dir, Config};
use claudio_core::consent::ensure_remote_consent;
use claudio_core::events::{Approver, EventSink};
use claudio_core::memory::{Embedder, HashEmbedder, Memory};
use claudio_core::permissions::PathGuard;
use claudio_core::store::Store;
use claudio_core::tool::ToolRegistry;
use claudio_core::{Agent, AgentDeps};
use claudio_llm::local::{self, LlamaServer, LlamaServerOptions};
use claudio_llm::router::{self, Availability, RouterOptions};
use claudio_llm::{build_provider, ModelConfig, Provider};
use tokio::sync::Mutex;

pub struct Runtime {
    pub cfg: Arc<Config>,
    pub store: Arc<Store>,
    pub audit: Arc<Audit>,
    pub memory: Arc<Memory>,
    pub guard: Arc<PathGuard>,
    pub tools: ToolRegistry,
    pub prompt_extra: String,
    pub warnings: Vec<String>,
    local_server: Mutex<Option<LlamaServer>>,
    #[allow(dead_code)]
    mcp: Vec<Arc<crate::mcp::McpClient>>,
}

struct HttpEmbedder(claudio_llm::embeddings::EmbeddingClient);

#[async_trait]
impl Embedder for HttpEmbedder {
    fn id(&self) -> String {
        self.0.id()
    }
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.0.embed(texts).await
    }
}

pub struct SelectedModel {
    pub config: ModelConfig,
    pub provider: Arc<dyn Provider>,
    pub reason: String,
}

impl Runtime {
    /// Open storage and build the tool registry. `db_dir` overrides the data dir (tests).
    pub async fn init(cfg: Config) -> Result<Self> {
        Self::init_at(cfg, data_dir()).await
    }

    pub async fn init_at(cfg: Config, dir: PathBuf) -> Result<Self> {
        let mut warnings = Vec::new();
        if cfg.insecure_file {
            warnings.push(format!(
                "{} pode ser lido por outros usuários; chaves inline foram ignoradas (use chmod 600)",
                cfg.source.clone().unwrap_or_else(Config::path).display()
            ));
        }
        for w in cfg.workspace_roots() {
            std::fs::create_dir_all(&w)
                .with_context(|| format!("criando workspace {}", w.display()))?;
        }
        let store = Arc::new(Store::open(dir.join("claudio.db"))?);
        let stale = store.mark_stale_running()?;
        if stale > 0 {
            warnings.push(format!(
                "{stale} sessão(ões) interrompida(s) podem ser retomadas com `claudio resume <id>`"
            ));
        }
        let audit = Arc::new(Audit::open(dir.join("audit.jsonl"))?);
        let embedder: Arc<dyn Embedder> = match cfg
            .memory
            .embedding_model
            .as_ref()
            .and_then(|n| cfg.model(n))
        {
            Some(m) => Arc::new(HttpEmbedder(claudio_llm::embeddings::EmbeddingClient::new(
                m.clone(),
            ))),
            None => Arc::new(HashEmbedder::default()),
        };
        let memory = Arc::new(Memory::new(store.clone(), embedder));
        let guard = Arc::new(PathGuard::from_config(&cfg.permissions));
        let mut tools = crate::builtin_registry(&cfg);
        let mut prompt_extra = String::new();

        crate::custom::register(&mut tools, &cfg, &mut warnings);
        let skills = crate::skills::discover(&cfg);
        if !skills.is_empty() {
            prompt_extra.push_str(&crate::skills::catalog(&skills));
            tools.register(Arc::new(crate::skills::SkillLoad { skills }));
        }
        let mut mcp = Vec::new();
        for server in &cfg.mcp_servers {
            match crate::mcp::McpClient::spawn(server).await {
                Ok(client) => {
                    let client = Arc::new(client);
                    match crate::mcp::register_tools(&client, &mut tools).await {
                        Ok(n) => tracing::info!("MCP {}: {n} ferramentas", server.name),
                        Err(e) => warnings.push(format!("MCP '{}': {e:#}", server.name)),
                    }
                    mcp.push(client);
                }
                Err(e) => warnings.push(format!("MCP '{}' não iniciou: {e:#}", server.name)),
            }
        }
        Ok(Self {
            cfg: Arc::new(cfg),
            store,
            audit,
            memory,
            guard,
            tools,
            prompt_extra,
            warnings,
            local_server: Mutex::new(None),
            mcp,
        })
    }

    /// Availability of every configured model (probes local servers).
    pub async fn availability(&self) -> HashMap<String, Availability> {
        let res = router::detect_resources();
        let mut map = HashMap::new();
        for m in &self.cfg.models {
            let mut a = router::static_availability(m, &res);
            if m.is_local() && a == Availability::Ready && m.gguf_path.is_none() {
                if !local::health(&m.base_url()).await {
                    a = Availability::Unavailable(format!(
                        "servidor local não responde em {}",
                        m.base_url()
                    ));
                }
            } else if a == Availability::Startable && local::health(&m.base_url()).await {
                a = Availability::Ready;
            } else if a == Availability::Startable && !self.cfg.local.autostart {
                a = Availability::Unavailable("autostart desligado e servidor parado".into());
            }
            map.insert(m.name.clone(), a);
        }
        map
    }

    /// Choose a model for `task`, start llama-server if needed and get consent for remote use.
    pub async fn select_model(
        &self,
        explicit: Option<&str>,
        task: &str,
        approver: &dyn Approver,
    ) -> Result<SelectedModel> {
        let profile = router::classify_task(task);
        let avail = self.availability().await;
        let opts = RouterOptions {
            explicit: explicit.map(String::from).or_else(|| {
                // The configured default wins when it is usable.
                self.cfg.default_model.clone().filter(|d| {
                    matches!(
                        avail.get(d),
                        Some(Availability::Ready | Availability::Startable)
                    )
                })
            }),
            local_only: self.cfg.privacy.local_only,
            prefer_local: self.cfg.privacy.prefer_local,
        };
        let (m, reason) = router::select(&self.cfg.models, profile, &opts, &avail)?;
        let m = m.clone();
        if avail.get(&m.name) == Some(&Availability::Startable) {
            self.start_local(&m).await?;
        }
        ensure_remote_consent(&self.store, approver, &self.cfg.privacy, &m).await?;
        let provider = build_provider(&m)?;
        Ok(SelectedModel {
            config: m,
            provider,
            reason,
        })
    }

    pub async fn start_local(&self, m: &ModelConfig) -> Result<()> {
        let mut guard = self.local_server.lock().await;
        if guard.is_some() && local::health(&m.base_url()).await {
            return Ok(());
        }
        let gguf = m
            .gguf_path
            .as_deref()
            .context("modelo local sem gguf_path")?;
        let port = m
            .base_url()
            .rsplit(':')
            .next()
            .and_then(|p| p.trim_end_matches("/v1").trim_end_matches('/').parse().ok())
            .unwrap_or(self.cfg.local.port);
        let server = LlamaServer::start(&LlamaServerOptions {
            binary: self
                .cfg
                .local
                .llama_server
                .as_deref()
                .map(claudio_core::config::expand),
            gguf: claudio_core::config::expand(gguf),
            port,
            ctx_size: m
                .context_window
                .min(self.cfg.local.ctx_size.max(m.context_window)),
            gpu_layers: self.cfg.local.gpu_layers,
            threads: self.cfg.local.threads,
            extra_args: vec![],
        })
        .await?;
        *guard = Some(server);
        Ok(())
    }

    pub fn agent(
        &self,
        sel: &SelectedModel,
        approver: Arc<dyn Approver>,
        events: Arc<dyn EventSink>,
    ) -> Agent {
        Agent::new(AgentDeps {
            provider: sel.provider.clone(),
            model: sel.config.clone(),
            tools: self.tools.clone(),
            cfg: self.cfg.clone(),
            store: self.store.clone(),
            memory: self.memory.clone(),
            audit: self.audit.clone(),
            approver,
            events,
            guard: self.guard.clone(),
            prompt_extra: self.prompt_extra.clone(),
        })
    }

    pub async fn shutdown(&self) {
        if let Some(s) = self.local_server.lock().await.take() {
            s.stop().await;
        }
    }
}
