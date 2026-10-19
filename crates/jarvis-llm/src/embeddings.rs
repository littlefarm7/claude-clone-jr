//! OpenAI-compatible `/embeddings` client (llama-server `--embedding`, OpenAI, Ollama).

use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::{http, ModelConfig};

pub struct EmbeddingClient {
    cfg: ModelConfig,
    key: Option<String>,
    client: reqwest::Client,
}

impl EmbeddingClient {
    pub fn new(cfg: ModelConfig) -> Self {
        let key = cfg.resolve_api_key();
        Self { cfg, key, client: http::client(Duration::from_secs(120)) }
    }

    pub fn id(&self) -> String {
        format!("emb:{}", self.cfg.model)
    }

    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/embeddings", self.cfg.base_url().trim_end_matches('/'));
        let mut headers = Vec::new();
        if let Some(k) = &self.key {
            headers.push(("authorization", format!("Bearer {k}")));
        }
        let v = http::post_json(&self.client, &url, &headers, &json!({"model": self.cfg.model, "input": texts})).await?;
        let data = v["data"].as_array().ok_or_else(|| anyhow!("resposta de embeddings inválida"))?;
        Ok(data
            .iter()
            .map(|d| d["embedding"].as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect()).unwrap_or_default())
            .collect())
    }
}
