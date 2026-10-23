//! Automatic setup for non-technical users: detect the hardware, pick a
//! suitable local model, install the engine and download the model — all
//! behind one progress bar. Also connects cloud accounts with a single key.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use claudio_core::config::{models_dir, Config};
use claudio_llm::catalog::{api_model_config, local_model, preset, LocalModel};
use claudio_llm::{ChatRequest, Message, ModelConfig};
use tokio_util::sync::CancellationToken;

use crate::downloads::{self, LlamaVariant, ProgressHandle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    Amd,
    Nvidia,
    Intel,
    Apple,
    Other,
}

#[derive(Debug, Clone)]
pub struct Gpu {
    pub vendor: GpuVendor,
    pub name: String,
    /// Dedicated video memory, when the OS tells us.
    pub vram_mb: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Hardware {
    pub ram_mb: u64,
    pub gpu: Option<Gpu>,
    /// Vulkan runtime present (needed by the GPU build on Linux/Windows).
    pub vulkan: bool,
    pub free_disk_mb: Option<u64>,
}

#[cfg_attr(not(windows), allow(dead_code))]
fn vendor_from_name(name: &str) -> GpuVendor {
    let n = name.to_lowercase();
    if n.contains("amd") || n.contains("radeon") || n.contains("ati ") {
        GpuVendor::Amd
    } else if n.contains("nvidia")
        || n.contains("geforce")
        || n.contains("rtx")
        || n.contains("quadro")
    {
        GpuVendor::Nvidia
    } else if n.contains("intel") {
        GpuVendor::Intel
    } else if n.contains("apple") {
        GpuVendor::Apple
    } else {
        GpuVendor::Other
    }
}

#[cfg(target_os = "linux")]
fn detect_gpu() -> Option<Gpu> {
    // /sys/class/drm/cardN/device/{vendor,mem_info_vram_total}
    let mut best: Option<Gpu> = None;
    for e in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let dev = e.path().join("device");
        let vendor_id = std::fs::read_to_string(dev.join("vendor")).unwrap_or_default();
        let vendor = match vendor_id.trim() {
            "0x1002" => GpuVendor::Amd,
            "0x10de" => GpuVendor::Nvidia,
            "0x8086" => GpuVendor::Intel,
            _ => continue,
        };
        let vram_mb = std::fs::read_to_string(dev.join("mem_info_vram_total"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(|b| b / (1024 * 1024));
        let gpu = Gpu {
            vendor,
            name: match vendor {
                GpuVendor::Amd => "AMD Radeon",
                GpuVendor::Nvidia => "NVIDIA",
                _ => "Intel",
            }
            .to_string(),
            vram_mb,
        };
        // Prefer a dedicated card over an integrated one.
        let better = match &best {
            None => true,
            Some(b) => b.vendor == GpuVendor::Intel && vendor != GpuVendor::Intel,
        };
        if better {
            best = Some(gpu);
        }
    }
    best
}

#[cfg(target_os = "windows")]
fn detect_gpu() -> Option<Gpu> {
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_VideoController | ForEach-Object { \"$($_.Name)|$($_.AdapterRAM)\" }",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut best: Option<Gpu> = None;
    for line in text.lines() {
        let (name, ram) = line.split_once('|').unwrap_or((line, ""));
        let vendor = vendor_from_name(name);
        if vendor == GpuVendor::Other {
            continue;
        }
        // AdapterRAM is a 32-bit value: treat it as a lower bound.
        let vram_mb = ram.trim().parse::<u64>().ok().map(|b| b / (1024 * 1024));
        let gpu = Gpu {
            vendor,
            name: name.trim().to_string(),
            vram_mb,
        };
        let better = match &best {
            None => true,
            Some(b) => b.vendor == GpuVendor::Intel && vendor != GpuVendor::Intel,
        };
        if better {
            best = Some(gpu);
        }
    }
    best
}

#[cfg(target_os = "macos")]
fn detect_gpu() -> Option<Gpu> {
    // Apple Silicon shares memory with the CPU and runs models on Metal.
    if std::env::consts::ARCH == "aarch64" {
        return Some(Gpu {
            vendor: GpuVendor::Apple,
            name: "Apple Silicon".into(),
            vram_mb: None,
        });
    }
    None
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn detect_gpu() -> Option<Gpu> {
    None
}

fn vulkan_available() -> bool {
    if cfg!(windows) {
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        return Path::new(&sys)
            .join("System32")
            .join("vulkan-1.dll")
            .exists();
    }
    if cfg!(target_os = "linux") {
        return [
            "/usr/lib/x86_64-linux-gnu/libvulkan.so.1",
            "/usr/lib64/libvulkan.so.1",
            "/usr/lib/libvulkan.so.1",
            "/usr/lib/aarch64-linux-gnu/libvulkan.so.1",
        ]
        .iter()
        .any(|p| Path::new(p).exists());
    }
    false
}

fn free_disk_mb(path: &Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let target = std::iter::successors(Some(path), |p| p.parent()).find(|p| p.exists())?;
    let target = claudio_llm::platform::canonicalize(target).ok()?;
    disks
        .list()
        .iter()
        .filter(|d| target.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space() / (1024 * 1024))
}

pub fn detect_hardware() -> Hardware {
    let res = claudio_llm::router::detect_resources();
    Hardware {
        ram_mb: res.mem_total_mb,
        gpu: detect_gpu(),
        vulkan: vulkan_available(),
        free_disk_mb: free_disk_mb(&models_dir()),
    }
}

/// What the automatic setup will do on this computer.
#[derive(Debug, Clone)]
pub struct LocalPlan {
    pub model: &'static LocalModel,
    pub variant: LlamaVariant,
    pub gpu_layers: u32,
    /// One-line explanation for the user.
    pub explanation: String,
}

/// Pick the best model and engine for this hardware.
pub fn recommend_local(hw: &Hardware) -> LocalPlan {
    let id = if hw.ram_mb == 0 || hw.ram_mb >= 8_500 {
        "qwen2.5-7b"
    } else if hw.ram_mb >= 5_000 {
        "qwen2.5-3b"
    } else {
        "qwen2.5-1.5b"
    };
    let model = local_model(id).expect("catalog model");
    let gpu = hw
        .gpu
        .as_ref()
        .filter(|g| g.vendor != GpuVendor::Intel && g.vendor != GpuVendor::Other);
    let (variant, gpu_layers) = match gpu {
        Some(g) if g.vendor == GpuVendor::Apple => (LlamaVariant::Cpu, 99), // macOS build uses Metal
        Some(g) if hw.vulkan => {
            let layers = match g.vram_mb {
                Some(v) if v >= model.size_mb * 12 / 10 => 99,
                Some(v) if v >= 1_500 => ((v as f64 * 0.8 / model.size_mb as f64) * 28.0) as u32,
                Some(_) => 0,
                None => 20,
            };
            if layers == 0 {
                (LlamaVariant::Cpu, 0)
            } else {
                (LlamaVariant::Vulkan, layers.min(99))
            }
        }
        _ => (LlamaVariant::Cpu, 0),
    };
    let where_ = if gpu_layers > 0 {
        "usando sua placa de vídeo"
    } else {
        "usando o processador"
    };
    let explanation = format!(
        "Vamos instalar o {} ({:.1} GB), escolhido para os {:.0} GB de memória deste computador, {where_}.",
        model.name.replace(" (recomendado)", "").replace(" Instruct", ""),
        model.size_mb as f64 / 1024.0,
        hw.ram_mb as f64 / 1024.0,
    );
    LocalPlan {
        model,
        variant,
        gpu_layers,
        explanation,
    }
}

/// Install everything needed to run `plan` and update `cfg` (not saved).
pub async fn setup_local(
    cfg: &mut Config,
    plan: &LocalPlan,
    progress: &ProgressHandle,
    cancel: &CancellationToken,
    on_phase: impl Fn(&str),
) -> Result<()> {
    let hw_disk = free_disk_mb(&models_dir());
    let model_path = downloads::model_path(plan.model);
    if !model_path.exists() {
        if let Some(free) = hw_disk {
            let need = plan.model.size_mb + 300;
            if free < need {
                bail!(
                    "Falta espaço no disco: são necessários {:.1} GB livres e há {:.1} GB.",
                    need as f64 / 1024.0,
                    free as f64 / 1024.0
                );
            }
        }
    }

    // 1. Engine
    let existing = claudio_llm::local::find_binary(
        cfg.local
            .llama_server
            .as_deref()
            .map(claudio_core::config::expand)
            .as_deref(),
    );
    let server = match existing {
        Some(p) => p,
        None => {
            on_phase("Preparando o motor de IA (etapa 1 de 2)");
            let r = downloads::install_llama_cpp(
                &downloads::llama_releases_api(),
                plan.variant,
                progress,
                cancel,
            )
            .await;
            match r {
                Ok(p) => p,
                // No GPU build for this system: fall back to the CPU build.
                Err(e) if plan.variant == LlamaVariant::Vulkan && !cancel.is_cancelled() => {
                    tracing::warn!("build Vulkan indisponível ({e:#}); usando CPU");
                    downloads::install_llama_cpp(
                        &downloads::llama_releases_api(),
                        LlamaVariant::Cpu,
                        progress,
                        cancel,
                    )
                    .await?
                }
                Err(e) => return Err(e),
            }
        }
    };
    cfg.local.llama_server = Some(server.to_string_lossy().into_owned());
    cfg.local.gpu_layers = plan.gpu_layers;

    // 2. Model
    if !model_path.exists() {
        progress.lock().unwrap().total = None;
        on_phase(&format!(
            "Baixando a IA {} (etapa 2 de 2)",
            plan.model
                .name
                .replace(" (recomendado)", "")
                .replace(" Instruct", "")
        ));
        downloads::download(
            &downloads::model_url(plan.model),
            &model_path,
            progress,
            cancel,
        )
        .await?;
    }
    downloads::use_local_model(cfg, plan.model.model_config(&model_path.to_string_lossy()));
    cfg.local.autostart = true;
    Ok(())
}

/// A cloud service the onboarding offers, in plain language.
#[derive(Debug, Clone, Copy)]
pub struct CloudChoice {
    pub preset: &'static str,
    pub title: &'static str,
    pub subtitle: &'static str,
    /// Simple steps to get a key.
    pub steps: &'static [&'static str],
}

pub const CLOUD_CHOICES: &[CloudChoice] = &[
    CloudChoice {
        preset: "anthropic",
        title: "Claude",
        subtitle: "da Anthropic — ótimo para tarefas longas e textos",
        steps: &[
            "Clique em “Abrir o site” e entre com sua conta (ou crie uma).",
            "Adicione créditos em Billing, se o site pedir.",
            "Clique em “Create Key”, copie a chave e cole abaixo.",
        ],
    },
    CloudChoice {
        preset: "openai",
        title: "ChatGPT",
        subtitle: "da OpenAI — modelos GPT",
        steps: &[
            "Clique em “Abrir o site” e entre com sua conta da OpenAI.",
            "Adicione créditos em Billing, se o site pedir.",
            "Clique em “Create new secret key”, copie a chave e cole abaixo.",
        ],
    },
    CloudChoice {
        preset: "gemini",
        title: "Gemini",
        subtitle: "do Google — tem uso gratuito com limites",
        steps: &[
            "Clique em “Abrir o site” e entre com sua conta Google.",
            "Clique em “Create API key” (Criar chave de API).",
            "Copie a chave que aparecer e cole abaixo.",
        ],
    },
    CloudChoice {
        preset: "openrouter",
        title: "OpenRouter",
        subtitle: "acesso a vários modelos com uma só conta",
        steps: &[
            "Clique em “Abrir o site” e entre ou crie sua conta.",
            "Clique em “Create Key”, copie a chave e cole abaixo.",
        ],
    },
];

/// Name used for cloud entries created by the onboarding.
pub fn cloud_model_name(preset_id: &str) -> String {
    match preset_id {
        "anthropic" => "claude".into(),
        "openai" => "chatgpt".into(),
        other => other.into(),
    }
}

/// Validate a key by sending a tiny message, then return the model entry.
pub async fn connect_cloud(preset_id: &str, key: &str) -> Result<ModelConfig> {
    let p = preset(preset_id).ok_or_else(|| anyhow::anyhow!("serviço desconhecido"))?;
    let key = key.trim();
    if key.is_empty() {
        bail!("Cole a chave de acesso no campo.");
    }
    let model = p.models.first().copied().unwrap_or_default();
    let mut entry = api_model_config(p, &cloud_model_name(preset_id), model, None);
    entry.api_key = Some(key.to_string());
    let provider = claudio_llm::build_provider(&entry)?;
    provider
        .chat(ChatRequest {
            messages: vec![Message::user("Responda apenas: OK")],
            tools: vec![],
            max_tokens: 16,
            temperature: Some(0.0),
        })
        .await
        .map_err(|e| anyhow::anyhow!(friendly_error(&format!("{e:#}"))))?;
    Ok(entry)
}

/// Turn technical errors into sentences a non-technical user understands.
pub fn friendly_error(raw: &str) -> String {
    let r = raw.to_lowercase();
    let msg = if r.contains("401")
        || r.contains("unauthorized")
        || r.contains("invalid x-api-key")
        || r.contains("invalid api key")
        || r.contains("api key not valid")
        || r.contains("incorrect api key")
    {
        "A chave de acesso não foi aceita. Confira se copiou a chave inteira, sem espaços."
    } else if r.contains("402")
        || r.contains("insufficient")
        || r.contains("credit")
        || r.contains("billing")
        || r.contains("quota")
    {
        "A conta não tem créditos ou atingiu o limite de uso. Verifique o pagamento no site do serviço."
    } else if r.contains("429") || r.contains("rate limit") {
        "O serviço está recebendo muitos pedidos agora. Espere um minuto e tente de novo."
    } else if r.contains("403") || r.contains("forbidden") {
        "O site recusou o download agora. Verifique sua internet (ou o bloqueio da rede) e tente mais tarde."
    } else if r.contains("dns")
        || r.contains("resolve")
        || r.contains("connect")
        || r.contains("network")
        || r.contains("timed out")
        || r.contains("tempo esgotado")
        || r.contains("error sending request")
    {
        "Não consegui me conectar. Verifique sua internet e tente de novo."
    } else if r.contains("falta espaço") {
        return raw.to_string();
    } else if r.contains("no space left") || r.contains("espaço") {
        "Não há espaço suficiente no disco. Libere espaço e tente de novo."
    } else if r.contains("cancelad") {
        "Cancelado. Você pode continuar depois de onde parou."
    } else if r.contains("llama-server não encontrado")
        || r.contains("gguf")
        || r.contains("servidor local não responde")
    {
        "A IA do computador ainda não está pronta. Abra Configurações e clique em “Configurar IA”."
    } else if r.contains("nenhum modelo disponível") || r.contains("sem chave") {
        "Nenhuma IA está configurada ainda. Abra Configurações e clique em “Configurar IA”."
    } else if r.contains("404") {
        "O arquivo ou serviço não foi encontrado. Tente outra opção ou mais tarde."
    } else {
        return format!(
            "Algo deu errado. Detalhes técnicos: {}",
            raw.chars().take(220).collect::<String>()
        );
    };
    msg.to_string()
}

/// Whether the configured default AI can be used right now (no network calls).
pub fn ai_ready(cfg: &Config) -> bool {
    let Some(name) = cfg.default_model.as_deref() else {
        return false;
    };
    let Some(m) = cfg.model(name) else {
        return false;
    };
    if m.is_local() {
        match &m.gguf_path {
            Some(p) => {
                claudio_core::config::expand(p).is_file()
                    && claudio_llm::local::find_binary(
                        cfg.local
                            .llama_server
                            .as_deref()
                            .map(claudio_core::config::expand)
                            .as_deref(),
                    )
                    .is_some()
            }
            // A server the user runs (Ollama, LM Studio...): trust the config.
            None => m
                .base_url
                .as_deref()
                .map(|u| !u.contains(":9931"))
                .unwrap_or(false),
        }
    } else {
        m.resolve_api_key().is_some() || !preset_needs_key(m)
    }
}

fn preset_needs_key(m: &ModelConfig) -> bool {
    !matches!(m.base_url.as_deref(), Some(u) if u.contains("127.0.0.1") || u.contains("localhost"))
}

/// Friendly label of the current AI ("No computador · Qwen2.5 7B", "Claude").
pub fn ai_label(cfg: &Config, name: &str) -> String {
    let Some(m) = cfg.model(name) else {
        return name.to_string();
    };
    if m.is_local()
        && m.gguf_path
            .as_deref()
            .is_some_and(|g| !claudio_core::config::expand(g).is_file())
    {
        return "IA ainda não configurada".into();
    }
    if m.is_local()
        && m.gguf_path.is_none()
        && m.base_url.as_deref().is_none_or(|u| u.contains(":9931"))
    {
        return "IA ainda não configurada".into();
    }
    if m.is_local() {
        let pretty = claudio_llm::catalog::LOCAL_MODELS
            .iter()
            .find(|c| {
                c.id == m.model
                    || m.gguf_path
                        .as_deref()
                        .map(|g| g.ends_with(c.file))
                        .unwrap_or(false)
            })
            .map(|c| {
                c.name
                    .replace(" (recomendado)", "")
                    .replace(" Instruct", "")
            })
            .unwrap_or_else(|| m.model.clone());
        format!("No computador · {pretty}")
    } else {
        let service = match m.provider {
            claudio_llm::ProviderKind::Anthropic => "Claude",
            claudio_llm::ProviderKind::Openai => "ChatGPT",
            claudio_llm::ProviderKind::Gemini => "Gemini",
            claudio_llm::ProviderKind::OpenaiCompat => {
                if m.base_url.as_deref().unwrap_or("").contains("openrouter") {
                    "OpenRouter"
                } else {
                    "Nuvem"
                }
            }
        };
        format!("Na nuvem · {service}")
    }
}

/// Default work folder: a visible "Claudio" folder in Documents.
pub fn default_workspace() -> PathBuf {
    dirs::document_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Claudio")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw(ram_mb: u64, gpu: Option<Gpu>, vulkan: bool) -> Hardware {
        Hardware {
            ram_mb,
            gpu,
            vulkan,
            free_disk_mb: Some(100_000),
        }
    }

    #[test]
    fn recommendations() {
        let p = recommend_local(&hw(16_000, None, false));
        assert_eq!(p.model.id, "qwen2.5-7b");
        assert_eq!((p.variant, p.gpu_layers), (LlamaVariant::Cpu, 0));
        assert!(p.explanation.contains("processador"));
        assert_eq!(
            recommend_local(&hw(6_000, None, false)).model.id,
            "qwen2.5-3b"
        );
        assert_eq!(
            recommend_local(&hw(3_500, None, false)).model.id,
            "qwen2.5-1.5b"
        );

        // RX 5500 XT 8 GB with Vulkan: whole model on the GPU.
        let amd8 = Gpu {
            vendor: GpuVendor::Amd,
            name: "RX 5500 XT".into(),
            vram_mb: Some(8_176),
        };
        let p = recommend_local(&hw(16_000, Some(amd8), true));
        assert_eq!((p.variant, p.gpu_layers), (LlamaVariant::Vulkan, 99));
        assert!(p.explanation.contains("placa de vídeo"));
        // 4 GB card: partial offload.
        let amd4 = Gpu {
            vendor: GpuVendor::Amd,
            name: "RX 5500 XT".into(),
            vram_mb: Some(4_080),
        };
        let p = recommend_local(&hw(16_000, Some(amd4.clone()), true));
        assert_eq!(p.variant, LlamaVariant::Vulkan);
        assert!(p.gpu_layers > 10 && p.gpu_layers < 99, "{}", p.gpu_layers);
        // No Vulkan runtime: stay on CPU.
        assert_eq!(
            recommend_local(&hw(16_000, Some(amd4), false)).variant,
            LlamaVariant::Cpu
        );
        // Integrated Intel graphics: CPU.
        let intel = Gpu {
            vendor: GpuVendor::Intel,
            name: "UHD".into(),
            vram_mb: None,
        };
        assert_eq!(
            recommend_local(&hw(16_000, Some(intel), true)).gpu_layers,
            0
        );
        // Apple Silicon: Metal build, all layers.
        let apple = Gpu {
            vendor: GpuVendor::Apple,
            name: "M2".into(),
            vram_mb: None,
        };
        assert_eq!(
            recommend_local(&hw(16_000, Some(apple), false)).gpu_layers,
            99
        );
    }

    #[test]
    fn friendly_errors() {
        assert!(friendly_error("HTTP 401 Unauthorized: invalid x-api-key").contains("chave"));
        assert!(friendly_error("erro de rede: error sending request for url").contains("internet"));
        assert!(friendly_error("HTTP 429 Too Many Requests").contains("minuto"));
        assert!(
            friendly_error("HTTP status client error (403 Forbidden) for url").contains("recusou")
        );
        assert!(friendly_error("xyz").starts_with("Algo deu errado"));
        assert!(friendly_error("nenhum modelo disponível (local: ...)").contains("Configurar IA"));
    }

    #[test]
    fn readiness_and_labels() {
        let mut cfg = Config::default();
        assert!(
            !ai_ready(&cfg),
            "default local model without GGUF is not ready"
        );
        cfg.models.push(ModelConfig {
            api_key: Some("k".into()),
            ..api_model_config(
                preset("anthropic").unwrap(),
                "claude2",
                "claude-sonnet-5-5",
                None,
            )
        });
        cfg.default_model = Some("claude2".into());
        assert!(ai_ready(&cfg));
        assert_eq!(ai_label(&cfg, "claude2"), "Na nuvem · Claude");
        let m = local_model("qwen2.5-7b").unwrap();
        crate::downloads::use_local_model(
            &mut cfg,
            m.model_config("/x/Qwen2.5-7B-Instruct-Q4_K_M.gguf"),
        );
        assert_eq!(ai_label(&cfg, "local"), "IA ainda não configurada");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Qwen2.5-7B-Instruct-Q4_K_M.gguf");
        std::fs::write(&file, b"x").unwrap();
        crate::downloads::use_local_model(&mut cfg, m.model_config(&file.to_string_lossy()));
        assert_eq!(ai_label(&cfg, "local"), "No computador · Qwen2.5 7B");
    }
}
