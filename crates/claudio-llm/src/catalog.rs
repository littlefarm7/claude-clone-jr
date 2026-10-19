//! Presets for API providers and a catalog of recommended local GGUF models.

use crate::{ModelConfig, ProviderKind};

/// An API provider the settings screen can configure.
#[derive(Debug, Clone, Copy)]
pub struct ApiPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub provider: ProviderKind,
    /// Fixed base URL for OpenAI-compatible services (None = provider default).
    pub base_url: Option<&'static str>,
    pub models: &'static [&'static str],
    /// Where to create a key.
    pub key_url: &'static str,
    pub needs_key: bool,
    pub context_window: u32,
}

pub const API_PRESETS: &[ApiPreset] = &[
    ApiPreset {
        id: "anthropic",
        label: "Anthropic (Claude)",
        provider: ProviderKind::Anthropic,
        base_url: None,
        models: &["claude-sonnet-5-5", "claude-opus-5-5", "claude-haiku-5-5"],
        key_url: "https://console.anthropic.com/settings/keys",
        needs_key: true,
        context_window: 200_000,
    },
    ApiPreset {
        id: "openai",
        label: "OpenAI (GPT)",
        provider: ProviderKind::Openai,
        base_url: None,
        models: &["gpt-5", "gpt-5-mini", "gpt-4.1"],
        key_url: "https://platform.openai.com/api-keys",
        needs_key: true,
        context_window: 128_000,
    },
    ApiPreset {
        id: "gemini",
        label: "Google Gemini",
        provider: ProviderKind::Gemini,
        base_url: None,
        models: &["gemini-2.5-flash", "gemini-2.5-pro"],
        key_url: "https://aistudio.google.com/apikey",
        needs_key: true,
        context_window: 1_000_000,
    },
    ApiPreset {
        id: "openrouter",
        label: "OpenRouter (vários modelos)",
        provider: ProviderKind::OpenaiCompat,
        base_url: Some("https://openrouter.ai/api/v1"),
        models: &[
            "anthropic/claude-sonnet-4.5",
            "openai/gpt-5-mini",
            "qwen/qwen3-coder",
        ],
        key_url: "https://openrouter.ai/keys",
        needs_key: true,
        context_window: 128_000,
    },
    ApiPreset {
        id: "groq",
        label: "Groq",
        provider: ProviderKind::OpenaiCompat,
        base_url: Some("https://api.groq.com/openai/v1"),
        models: &["llama-3.3-70b-versatile", "qwen/qwen3-32b"],
        key_url: "https://console.groq.com/keys",
        needs_key: true,
        context_window: 128_000,
    },
    ApiPreset {
        id: "ollama",
        label: "Ollama (local)",
        provider: ProviderKind::OpenaiCompat,
        base_url: Some("http://127.0.0.1:11434/v1"),
        models: &["qwen2.5:7b", "llama3.1:8b"],
        key_url: "https://ollama.com/download",
        needs_key: false,
        context_window: 8192,
    },
    ApiPreset {
        id: "lmstudio",
        label: "LM Studio (local)",
        provider: ProviderKind::OpenaiCompat,
        base_url: Some("http://127.0.0.1:1234/v1"),
        models: &["local-model"],
        key_url: "https://lmstudio.ai",
        needs_key: false,
        context_window: 8192,
    },
    ApiPreset {
        id: "custom",
        label: "Outro compatível com OpenAI",
        provider: ProviderKind::OpenaiCompat,
        base_url: Some("https://"),
        models: &[],
        key_url: "",
        needs_key: true,
        context_window: 32_000,
    },
];

pub fn preset(id: &str) -> Option<&'static ApiPreset> {
    API_PRESETS.iter().find(|p| p.id == id)
}

/// Build the `[[models]]` entry for an API model.
pub fn api_model_config(
    preset: &ApiPreset,
    name: &str,
    model: &str,
    base_url: Option<&str>,
) -> ModelConfig {
    let base = base_url
        .filter(|b| !b.trim().is_empty())
        .map(|b| b.trim().to_string())
        .or_else(|| preset.base_url.map(String::from));
    ModelConfig {
        name: name.to_string(),
        provider: preset.provider,
        model: model.to_string(),
        base_url: base,
        api_key_env: None,
        api_key: None,
        context_window: preset.context_window,
        max_output_tokens: 8192.min(preset.context_window / 4),
        tags: vec!["smart".into(), "code".into(), "long".into()],
        gguf_path: None,
        temperature: None,
    }
}

/// A downloadable GGUF model (Hugging Face).
#[derive(Debug, Clone, Copy)]
pub struct LocalModel {
    pub id: &'static str,
    pub name: &'static str,
    pub repo: &'static str,
    pub file: &'static str,
    pub size_mb: u64,
    /// RAM (or RAM + VRAM) needed to run it comfortably.
    pub min_ram_mb: u64,
    pub context: u32,
    pub description: &'static str,
    pub tags: &'static [&'static str],
}

impl LocalModel {
    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/main/{}?download=true",
            self.repo, self.file
        )
    }

    /// Whether it fits in the given amount of RAM.
    pub fn fits(&self, mem_total_mb: u64) -> bool {
        mem_total_mb == 0 || self.min_ram_mb <= mem_total_mb
    }

    pub fn model_config(&self, gguf_path: &str) -> ModelConfig {
        ModelConfig {
            name: "local".into(),
            provider: ProviderKind::OpenaiCompat,
            model: self.id.into(),
            base_url: Some("http://127.0.0.1:9931/v1".into()),
            api_key_env: None,
            api_key: None,
            context_window: self.context,
            max_output_tokens: 2048,
            tags: self.tags.iter().map(|t| t.to_string()).collect(),
            gguf_path: Some(gguf_path.into()),
            temperature: None,
        }
    }
}

/// Models with good tool-calling support in llama.cpp (`--jinja`), Q4_K_M.
pub const LOCAL_MODELS: &[LocalModel] = &[
    LocalModel {
        id: "qwen2.5-1.5b",
        name: "Qwen2.5 1.5B Instruct",
        repo: "bartowski/Qwen2.5-1.5B-Instruct-GGUF",
        file: "Qwen2.5-1.5B-Instruct-Q4_K_M.gguf",
        size_mb: 986,
        min_ram_mb: 3_000,
        context: 8192,
        description: "Muito leve; tarefas simples, máquinas com pouca memória.",
        tags: &["fast", "local"],
    },
    LocalModel {
        id: "llama3.2-3b",
        name: "Llama 3.2 3B Instruct",
        repo: "bartowski/Llama-3.2-3B-Instruct-GGUF",
        file: "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
        size_mb: 2_020,
        min_ram_mb: 5_000,
        context: 8192,
        description: "Leve e rápido; bom para conversa e tarefas curtas.",
        tags: &["fast", "local"],
    },
    LocalModel {
        id: "qwen2.5-3b",
        name: "Qwen2.5 3B Instruct",
        repo: "bartowski/Qwen2.5-3B-Instruct-GGUF",
        file: "Qwen2.5-3B-Instruct-Q4_K_M.gguf",
        size_mb: 1_930,
        min_ram_mb: 5_000,
        context: 8192,
        description: "Leve; cabe inteiro em placas de vídeo com 4 GB.",
        tags: &["fast", "local", "default"],
    },
    LocalModel {
        id: "qwen2.5-7b",
        name: "Qwen2.5 7B Instruct (recomendado)",
        repo: "bartowski/Qwen2.5-7B-Instruct-GGUF",
        file: "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
        size_mb: 4_680,
        min_ram_mb: 8_000,
        context: 16384,
        description: "Melhor equilíbrio entre qualidade e velocidade para o agente.",
        tags: &["default", "local", "code", "smart"],
    },
    LocalModel {
        id: "qwen2.5-coder-7b",
        name: "Qwen2.5 Coder 7B Instruct",
        repo: "bartowski/Qwen2.5-Coder-7B-Instruct-GGUF",
        file: "Qwen2.5-Coder-7B-Instruct-Q4_K_M.gguf",
        size_mb: 4_680,
        min_ram_mb: 8_000,
        context: 16384,
        description: "Focado em programação.",
        tags: &["local", "code"],
    },
    LocalModel {
        id: "qwen3-8b",
        name: "Qwen3 8B",
        repo: "Qwen/Qwen3-8B-GGUF",
        file: "Qwen3-8B-Q4_K_M.gguf",
        size_mb: 5_030,
        min_ram_mb: 9_000,
        context: 16384,
        description: "Raciocínio melhor; um pouco mais lento.",
        tags: &["local", "smart", "code"],
    },
    LocalModel {
        id: "llama3.1-8b",
        name: "Llama 3.1 8B Instruct",
        repo: "bartowski/Meta-Llama-3.1-8B-Instruct-GGUF",
        file: "Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf",
        size_mb: 4_920,
        min_ram_mb: 9_000,
        context: 16384,
        description: "Generalista, bom em português.",
        tags: &["local", "smart"],
    },
    LocalModel {
        id: "qwen2.5-14b",
        name: "Qwen2.5 14B Instruct",
        repo: "bartowski/Qwen2.5-14B-Instruct-GGUF",
        file: "Qwen2.5-14B-Instruct-Q4_K_M.gguf",
        size_mb: 8_990,
        min_ram_mb: 14_000,
        context: 16384,
        description: "Mais capaz; exige 16 GB de RAM e é lento só em CPU.",
        tags: &["local", "smart", "code", "long"],
    },
];

pub fn local_model(id: &str) -> Option<&'static LocalModel> {
    LOCAL_MODELS.iter().find(|m| m.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_consistent() {
        for m in LOCAL_MODELS {
            assert!(m.file.ends_with(".gguf"), "{}", m.id);
            assert!(m.url().starts_with("https://huggingface.co/"));
            assert!(m.min_ram_mb > m.size_mb);
            let c = m.model_config("/x.gguf");
            assert!(c.is_local());
        }
        let ids: std::collections::HashSet<_> = LOCAL_MODELS.iter().map(|m| m.id).collect();
        assert_eq!(ids.len(), LOCAL_MODELS.len());
        assert!(local_model("qwen2.5-7b").unwrap().fits(16_000));
        assert!(!local_model("qwen2.5-14b").unwrap().fits(8_000));
    }

    #[test]
    fn api_presets() {
        let p = preset("openrouter").unwrap();
        let c = api_model_config(p, "router", "x/y", None);
        assert_eq!(c.base_url.as_deref(), Some("https://openrouter.ai/api/v1"));
        assert!(!c.is_local());
        let o = api_model_config(preset("ollama").unwrap(), "ol", "qwen2.5:7b", None);
        assert!(o.is_local());
        let a = api_model_config(
            preset("anthropic").unwrap(),
            "claude",
            "claude-sonnet-5-5",
            Some(" "),
        );
        assert!(a.base_url.is_none());
    }
}
