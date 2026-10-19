//! Model selection by task profile, availability and machine resources.

use std::collections::HashMap;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::ModelConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskProfile {
    Fast,
    Smart,
    Code,
    Long,
    Vision,
    Default,
}

impl TaskProfile {
    pub fn tag(&self) -> &'static str {
        match self {
            TaskProfile::Fast => "fast",
            TaskProfile::Smart => "smart",
            TaskProfile::Code => "code",
            TaskProfile::Long => "long",
            TaskProfile::Vision => "vision",
            TaskProfile::Default => "default",
        }
    }
}

/// Cheap keyword heuristic (Portuguese and English).
pub fn classify_task(task: &str) -> TaskProfile {
    let t = task.to_lowercase();
    let has = |ws: &[&str]| ws.iter().any(|w| t.contains(w));
    if has(&[
        "imagem",
        "image",
        "foto",
        "photo",
        "screenshot",
        "captura de tela",
    ]) {
        TaskProfile::Vision
    } else if task.len() > 4000
        || has(&[
            "resuma o documento",
            "summarize the document",
            "livro",
            "relatório longo",
        ])
    {
        TaskProfile::Long
    } else if has(&[
        "código", "codigo", "code", "rust", "python", "compile", "compilar", "bug", "teste",
        "refator", "função", "function",
    ]) {
        TaskProfile::Code
    } else if has(&[
        "planeje",
        "analise",
        "analyze",
        "pesquise",
        "research",
        "compare",
        "estratégia",
    ]) {
        TaskProfile::Smart
    } else if task.len() < 80 {
        TaskProfile::Fast
    } else {
        TaskProfile::Default
    }
}

#[derive(Debug, Clone, Default)]
pub struct Resources {
    pub mem_available_mb: u64,
    pub mem_total_mb: u64,
    pub cpu_threads: usize,
}

pub fn detect_resources() -> Resources {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    Resources {
        cpu_threads: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
        mem_available_mb: sys.available_memory() / (1024 * 1024),
        mem_total_mb: sys.total_memory() / (1024 * 1024),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Ready,
    /// Local GGUF that can be started on demand.
    Startable,
    Unavailable(String),
}

#[derive(Debug, Clone, Default)]
pub struct RouterOptions {
    pub explicit: Option<String>,
    pub local_only: bool,
    pub prefer_local: bool,
}

/// Static checks that need no network: keys and GGUF size vs RAM.
pub fn static_availability(cfg: &ModelConfig, res: &Resources) -> Availability {
    if !cfg.is_local() {
        return if cfg.resolve_api_key().is_some() {
            Availability::Ready
        } else {
            Availability::Unavailable("sem chave de API".into())
        };
    }
    if let Some(path) = &cfg.gguf_path {
        match std::fs::metadata(shellexpand(path)) {
            Ok(meta) => {
                let need_mb = meta.len() / (1024 * 1024) + 512;
                if res.mem_available_mb > 0 && need_mb > res.mem_available_mb {
                    return Availability::Unavailable(format!(
                        "GGUF precisa ~{need_mb} MB, livres {} MB",
                        res.mem_available_mb
                    ));
                }
                return Availability::Startable;
            }
            Err(_) => return Availability::Unavailable(format!("GGUF não encontrado: {path}")),
        }
    }
    // Local server without a managed GGUF: assume it may be running; caller probes /health.
    Availability::Ready
}

pub fn shellexpand(p: &str) -> String {
    crate::platform::expand_home(p)
}

/// Pick the best model. Returns the chosen config and a human-readable reason.
pub fn select<'a>(
    models: &'a [ModelConfig],
    profile: TaskProfile,
    opts: &RouterOptions,
    availability: &HashMap<String, Availability>,
) -> Result<(&'a ModelConfig, String)> {
    if let Some(name) = &opts.explicit {
        let Some(m) = models.iter().find(|m| &m.name == name) else {
            bail!("modelo '{name}' não existe na configuração");
        };
        if opts.local_only && !m.is_local() {
            bail!("modelo '{name}' é remoto e privacy.local_only está ativo");
        }
        return Ok((m, "escolhido pelo usuário".into()));
    }
    let mut best: Option<(&ModelConfig, i32, String)> = None;
    let mut skipped = Vec::new();
    for m in models {
        let avail = availability
            .get(&m.name)
            .cloned()
            .unwrap_or(Availability::Ready);
        if let Availability::Unavailable(why) = &avail {
            skipped.push(format!("{}: {why}", m.name));
            continue;
        }
        if opts.local_only && !m.is_local() {
            skipped.push(format!("{}: remoto (local_only)", m.name));
            continue;
        }
        let mut score = 0;
        let mut why = Vec::new();
        if m.tags.iter().any(|t| t == profile.tag()) {
            score += 30;
            why.push(format!("tag '{}'", profile.tag()));
        }
        if m.is_local() && opts.prefer_local {
            score += 20;
            why.push("local".to_string());
        }
        if avail == Availability::Ready {
            score += 5;
        }
        if profile == TaskProfile::Long {
            score += (m.context_window / 16384) as i32;
        }
        if best.as_ref().map(|b| score > b.1).unwrap_or(true) {
            best = Some((m, score, why.join(", ")));
        }
    }
    match best {
        Some((m, _, why)) => Ok((
            m,
            if why.is_empty() {
                "primeiro disponível".into()
            } else {
                why
            },
        )),
        None => bail!("nenhum modelo disponível ({})", skipped.join("; ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderKind;

    fn m(name: &str, provider: ProviderKind, tags: &[&str]) -> ModelConfig {
        ModelConfig {
            name: name.into(),
            provider,
            model: name.into(),
            base_url: None,
            api_key_env: Some("CLAUDIO_TEST_NO_SUCH_KEY".into()),
            api_key: None,
            context_window: 8192,
            max_output_tokens: 1024,
            tags: tags.iter().map(|s| s.to_string()).collect(),
            gguf_path: None,
            temperature: None,
        }
    }

    #[test]
    fn classify() {
        assert_eq!(
            classify_task("corrija o bug no código rust"),
            TaskProfile::Code
        );
        assert_eq!(classify_task("oi"), TaskProfile::Fast);
        assert_eq!(classify_task("descreva esta imagem"), TaskProfile::Vision);
    }

    #[test]
    fn select_prefers_tag_and_local_and_skips_keyless() {
        let models = vec![
            m("local", ProviderKind::OpenaiCompat, &["fast"]),
            m("claude", ProviderKind::Anthropic, &["code", "smart"]),
        ];
        let res = Resources::default();
        let avail: HashMap<_, _> = models
            .iter()
            .map(|c| (c.name.clone(), static_availability(c, &res)))
            .collect();
        let opts = RouterOptions {
            prefer_local: true,
            ..Default::default()
        };
        let (c, _) = select(&models, TaskProfile::Code, &opts, &avail).unwrap();
        assert_eq!(c.name, "local"); // claude has no key -> unavailable
        let opts = RouterOptions {
            explicit: Some("claude".into()),
            local_only: true,
            ..Default::default()
        };
        assert!(select(&models, TaskProfile::Code, &opts, &avail).is_err());
    }

    #[test]
    fn gguf_too_big_is_unavailable() {
        let dir = std::env::temp_dir().join(format!("claudio-router-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("m.gguf");
        std::fs::write(&f, vec![0u8; 2 * 1024 * 1024]).unwrap();
        let mut c = m("l", ProviderKind::OpenaiCompat, &[]);
        c.gguf_path = Some(f.to_string_lossy().into());
        let small = Resources {
            mem_available_mb: 100,
            ..Default::default()
        };
        assert!(matches!(
            static_availability(&c, &small),
            Availability::Unavailable(_)
        ));
        let big = Resources {
            mem_available_mb: 100_000,
            ..Default::default()
        };
        assert_eq!(static_availability(&c, &big), Availability::Startable);
    }
}
