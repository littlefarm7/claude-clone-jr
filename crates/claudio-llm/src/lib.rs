//! Provider-neutral LLM interface for Claudio.
//!
//! The agent talks only to [`Provider`]. Adapters translate the neutral
//! [`Message`]/[`Part`] types to each vendor's wire format.

pub mod anthropic;
pub mod catalog;
pub mod embeddings;
pub mod gemini;
pub mod http;
pub mod local;
pub mod openai;
pub mod platform;
pub mod router;
pub mod scripted;
pub mod types;

use std::sync::Arc;

use anyhow::{bail, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use types::*;

#[async_trait]
pub trait Provider: Send + Sync {
    /// Short identifier, e.g. `anthropic:claude-sonnet-5-5`.
    fn id(&self) -> String;
    /// True when no data leaves this machine.
    fn is_local(&self) -> bool;
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// llama.cpp `llama-server`, Ollama, LM Studio, vLLM, or OpenAI itself.
    OpenaiCompat,
    Openai,
    Anthropic,
    Gemini,
}

/// Configuration of one selectable model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub name: String,
    pub provider: ProviderKind,
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
    /// Environment variable holding the API key (preferred over inline keys).
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Inline key. Only honoured when the config file is private (0600).
    #[serde(default, skip_serializing)]
    pub api_key: Option<String>,
    #[serde(default = "default_ctx")]
    pub context_window: u32,
    #[serde(default = "default_max_out")]
    pub max_output_tokens: u32,
    /// Free-form capability tags: fast, smart, code, long, vision, local.
    #[serde(default)]
    pub tags: Vec<String>,
    /// For local models managed by Claudio: path to the GGUF file.
    #[serde(default)]
    pub gguf_path: Option<String>,
    #[serde(default)]
    pub temperature: Option<f32>,
}

fn default_ctx() -> u32 {
    8192
}
fn default_max_out() -> u32 {
    2048
}

impl ModelConfig {
    pub fn default_base_url(&self) -> String {
        match self.provider {
            ProviderKind::OpenaiCompat => "http://127.0.0.1:9931/v1".into(),
            ProviderKind::Openai => "https://api.openai.com/v1".into(),
            ProviderKind::Anthropic => "https://api.anthropic.com".into(),
            ProviderKind::Gemini => "https://generativelanguage.googleapis.com/v1beta".into(),
        }
    }

    pub fn base_url(&self) -> String {
        self.base_url
            .clone()
            .unwrap_or_else(|| self.default_base_url())
    }

    pub fn default_key_env(&self) -> Option<&'static str> {
        match self.provider {
            ProviderKind::OpenaiCompat => None,
            ProviderKind::Openai => Some("OPENAI_API_KEY"),
            ProviderKind::Anthropic => Some("ANTHROPIC_API_KEY"),
            ProviderKind::Gemini => Some("GEMINI_API_KEY"),
        }
    }

    pub fn resolve_api_key(&self) -> Option<String> {
        let env = self.api_key_env.as_deref().or(self.default_key_env());
        if let Some(env) = env {
            if let Ok(v) = std::env::var(env) {
                if !v.trim().is_empty() {
                    return Some(v.trim().to_string());
                }
            }
        }
        self.api_key.clone().filter(|k| !k.is_empty())
    }

    /// Whether requests stay on this machine (loopback base URL).
    pub fn is_local(&self) -> bool {
        let url = self.base_url();
        let host = url
            .split("://")
            .nth(1)
            .unwrap_or("")
            .split(['/', ':'])
            .next()
            .unwrap_or("");
        matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1")
    }
}

/// Build a provider from config.
pub fn build_provider(cfg: &ModelConfig) -> Result<Arc<dyn Provider>> {
    let key = cfg.resolve_api_key();
    Ok(match cfg.provider {
        ProviderKind::OpenaiCompat => Arc::new(openai::OpenAiCompat::new(cfg.clone(), key)),
        ProviderKind::Openai => {
            if key.is_none() {
                bail!(
                    "modelo '{}': defina OPENAI_API_KEY (ou api_key_env)",
                    cfg.name
                );
            }
            Arc::new(openai::OpenAiCompat::new(cfg.clone(), key))
        }
        ProviderKind::Anthropic => match key {
            Some(k) => Arc::new(anthropic::Anthropic::new(cfg.clone(), k)),
            None => bail!(
                "modelo '{}': defina ANTHROPIC_API_KEY (ou api_key_env)",
                cfg.name
            ),
        },
        ProviderKind::Gemini => match key {
            Some(k) => Arc::new(gemini::Gemini::new(cfg.clone(), k)),
            None => bail!(
                "modelo '{}': defina GEMINI_API_KEY (ou api_key_env)",
                cfg.name
            ),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_detection() {
        let mut c = ModelConfig {
            name: "x".into(),
            provider: ProviderKind::OpenaiCompat,
            model: "m".into(),
            base_url: None,
            api_key_env: None,
            api_key: None,
            context_window: 1,
            max_output_tokens: 1,
            tags: vec![],
            gguf_path: None,
            temperature: None,
        };
        assert!(c.is_local());
        c.base_url = Some("http://localhost:8080/v1".into());
        assert!(c.is_local());
        c.base_url = Some("https://api.openai.com/v1".into());
        assert!(!c.is_local());
        c.base_url = Some("http://127.0.0.1.evil.com/v1".into());
        assert!(!c.is_local());
    }
}
