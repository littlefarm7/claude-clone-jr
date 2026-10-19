//! User configuration (`~/.config/jarvis/config.toml`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use jarvis_llm::{ModelConfig, ProviderKind};
use serde::{Deserialize, Serialize};

use crate::permissions::{Mode, ToolPolicy};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub default_model: Option<String>,
    pub agent: AgentSettings,
    pub permissions: PermissionsConfig,
    pub privacy: PrivacyConfig,
    pub sandbox: SandboxConfig,
    pub web: WebConfig,
    pub local: LocalConfig,
    pub memory: MemoryConfig,
    pub models: Vec<ModelConfig>,
    pub mcp_servers: Vec<McpServerConfig>,
    pub custom_tools: Vec<CustomToolConfig>,
    pub skills_dirs: Vec<String>,
    /// Set by `load` when the config file is readable by other users.
    #[serde(skip)]
    pub insecure_file: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSettings {
    pub max_iterations: u32,
    pub max_model_calls: u32,
    pub max_tool_calls: u32,
    pub max_wall_secs: u64,
    pub max_total_tokens: u64,
    pub max_tool_output_bytes: usize,
    pub tool_timeout_secs: u64,
    pub verify: bool,
    pub max_verify_rounds: u32,
    pub language: String,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            max_iterations: 40,
            max_model_calls: 60,
            max_tool_calls: 120,
            max_wall_secs: 3600,
            max_total_tokens: 2_000_000,
            max_tool_output_bytes: 24_000,
            tool_timeout_secs: 120,
            verify: true,
            max_verify_rounds: 2,
            language: "pt-BR".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionsConfig {
    pub mode: Mode,
    pub workspace_roots: Vec<String>,
    pub read_roots: Vec<String>,
    pub tools: BTreeMap<String, ToolPolicy>,
    /// Extra path prefixes that are never readable or writable.
    pub blocked_paths: Vec<String>,
}

impl Default for PermissionsConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Ask,
            workspace_roots: vec!["~/jarvis-work".into()],
            read_roots: vec![],
            tools: BTreeMap::new(),
            blocked_paths: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyConfig {
    pub local_only: bool,
    pub prefer_local: bool,
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        Self { local_only: false, prefer_local: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxConfig {
    /// Use bubblewrap for `shell` when available.
    pub enabled: bool,
    /// Allow network inside the sandbox.
    pub network: bool,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self { enabled: true, network: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// SearXNG instance (e.g. `http://127.0.0.1:8888`). Preferred when set.
    pub searxng_url: Option<String>,
    /// Allow fetching localhost / private network addresses.
    pub allow_private_hosts: bool,
    pub max_fetch_bytes: usize,
    pub chromium: Option<String>,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self { searxng_url: None, allow_private_hosts: false, max_fetch_bytes: 3_000_000, chromium: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalConfig {
    pub llama_server: Option<String>,
    pub port: u16,
    pub gpu_layers: u32,
    pub threads: Option<usize>,
    pub ctx_size: u32,
    /// Start llama-server automatically when a local model with `gguf_path` is chosen.
    pub autostart: bool,
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self { llama_server: None, port: 9931, gpu_layers: 0, threads: None, ctx_size: 8192, autostart: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MemoryConfig {
    /// Model name (from `models`) whose OpenAI-compatible `/embeddings` is used. None = lexical vectors.
    pub embedding_model: Option<String>,
    /// Inject relevant memories into the system prompt.
    pub auto_recall: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomToolConfig {
    pub name: String,
    pub description: String,
    /// argv template; `{param}` placeholders are replaced per argument (no shell).
    pub command: Vec<String>,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    #[serde(default = "default_custom_risk")]
    pub risk: String,
}

fn default_custom_risk() -> String {
    "execute".into()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_model: Some("local".into()),
            agent: AgentSettings::default(),
            permissions: PermissionsConfig::default(),
            privacy: PrivacyConfig::default(),
            sandbox: SandboxConfig::default(),
            web: WebConfig::default(),
            local: LocalConfig::default(),
            memory: MemoryConfig { embedding_model: None, auto_recall: true },
            models: default_models(),
            mcp_servers: vec![],
            custom_tools: vec![],
            skills_dirs: vec!["~/.config/jarvis/skills".into()],
            insecure_file: false,
        }
    }
}

pub fn default_models() -> Vec<ModelConfig> {
    let m = |name: &str, provider, model: &str, ctx, tags: &[&str]| ModelConfig {
        name: name.into(),
        provider,
        model: model.into(),
        base_url: None,
        api_key_env: None,
        api_key: None,
        context_window: ctx,
        max_output_tokens: 4096,
        tags: tags.iter().map(|s| s.to_string()).collect(),
        gguf_path: None,
        temperature: None,
    };
    vec![
        m("local", ProviderKind::OpenaiCompat, "local", 8192, &["fast", "default", "local"]),
        m("claude", ProviderKind::Anthropic, "claude-sonnet-5-5", 200_000, &["smart", "code", "long", "vision"]),
        m("gemini", ProviderKind::Gemini, "gemini-2.5-flash", 1_000_000, &["long", "fast", "vision"]),
        m("openai", ProviderKind::Openai, "gpt-5-mini", 128_000, &["smart", "code"]),
    ]
}

pub fn expand(p: &str) -> PathBuf {
    PathBuf::from(jarvis_llm::router::shellexpand(p))
}

pub fn config_dir() -> PathBuf {
    if let Ok(d) = std::env::var("JARVIS_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("jarvis")
}

pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("JARVIS_DATA_DIR") {
        return PathBuf::from(d);
    }
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("jarvis")
}

impl Config {
    pub fn path() -> PathBuf {
        config_dir().join("config.toml")
    }

    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("lendo {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&text).with_context(|| format!("config inválida em {}", path.display()))?;
        cfg.insecure_file = file_is_shared(path);
        if cfg.insecure_file {
            // Never trust inline secrets from a world/group-readable file.
            for m in &mut cfg.models {
                m.api_key = None;
            }
        }
        Ok(cfg)
    }

    pub fn save_default_if_missing() -> Result<PathBuf> {
        let path = Self::path();
        if !path.exists() {
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, DEFAULT_CONFIG_TOML)?;
            set_private(&path);
        }
        Ok(path)
    }

    pub fn model(&self, name: &str) -> Option<&ModelConfig> {
        self.models.iter().find(|m| m.name == name)
    }

    pub fn workspace_roots(&self) -> Vec<PathBuf> {
        self.permissions.workspace_roots.iter().map(|r| expand(r)).collect()
    }
}

#[cfg(unix)]
fn file_is_shared(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).map(|m| m.permissions().mode() & 0o077 != 0).unwrap_or(false)
}
#[cfg(not(unix))]
fn file_is_shared(_: &Path) -> bool {
    false
}

#[cfg(unix)]
pub fn set_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
pub fn set_private(_: &Path) {}

pub const DEFAULT_CONFIG_TOML: &str = r#"# Configuração do Jarvis Core. Chaves de API: prefira variáveis de ambiente
# (ANTHROPIC_API_KEY, GEMINI_API_KEY, OPENAI_API_KEY).

default_model = "local"
skills_dirs = ["~/.config/jarvis/skills"]

[agent]
max_iterations = 40
max_model_calls = 60
max_tool_calls = 120
max_wall_secs = 3600
max_tool_output_bytes = 24000
tool_timeout_secs = 120
verify = true
max_verify_rounds = 2
language = "pt-BR"

[permissions]
mode = "ask"                     # ask | auto-safe | strict
workspace_roots = ["~/jarvis-work"]
read_roots = []
# [permissions.tools]
# shell = "ask"                  # allow | ask | deny

[privacy]
local_only = false
prefer_local = true

[sandbox]
enabled = true                   # usa bubblewrap (bwrap) se instalado
network = true

[web]
# searxng_url = "http://127.0.0.1:8888"
allow_private_hosts = false

[local]
# llama_server = "/caminho/para/llama-server"
port = 9931
gpu_layers = 0                   # RX 5500 XT com build Vulkan: tente 20-33
ctx_size = 8192
autostart = true

[memory]
auto_recall = true

[[models]]
name = "local"
provider = "openai_compat"
model = "local"
base_url = "http://127.0.0.1:9931/v1"
# gguf_path = "~/modelos/Qwen2.5-7B-Instruct-Q4_K_M.gguf"
context_window = 8192
max_output_tokens = 2048
tags = ["fast", "default", "local", "code"]

[[models]]
name = "claude"
provider = "anthropic"
model = "claude-sonnet-5-5"
context_window = 200000
max_output_tokens = 8192
tags = ["smart", "code", "long", "vision"]

[[models]]
name = "gemini"
provider = "gemini"
model = "gemini-2.5-flash"
context_window = 1000000
max_output_tokens = 8192
tags = ["long", "fast", "vision"]

[[models]]
name = "openai"
provider = "openai"
model = "gpt-5-mini"
context_window = 128000
max_output_tokens = 8192
tags = ["smart", "code"]
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_toml_parses() {
        let c: Config = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
        assert_eq!(c.models.len(), 4);
        assert_eq!(c.local.port, 9931);
    }

    #[cfg(unix)]
    #[test]
    fn inline_keys_dropped_when_file_shared() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "[[models]]\nname='x'\nprovider='anthropic'\nmodel='m'\napi_key='secret'\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        let c = Config::load_from(&p).unwrap();
        assert!(c.insecure_file);
        assert!(c.models[0].api_key.is_none());
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        let c = Config::load_from(&p).unwrap();
        assert_eq!(c.models[0].api_key.as_deref(), Some("secret"));
    }
}
