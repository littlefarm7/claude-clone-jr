//! Downloads used by the settings screen: GGUF models (resumable, verified)
//! and prebuilt llama.cpp binaries from the official GitHub releases.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use claudio_core::config::{data_dir, models_dir, Config};
use claudio_llm::catalog::LocalModel;
use claudio_llm::ModelConfig;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub stage: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub done: bool,
    pub error: Option<String>,
    pub result: Option<PathBuf>,
}

impl Progress {
    pub fn fraction(&self) -> Option<f32> {
        self.total
            .filter(|t| *t > 0)
            .map(|t| (self.downloaded as f64 / t as f64) as f32)
    }
}

pub type ProgressHandle = Arc<Mutex<Progress>>;

pub fn new_progress() -> ProgressHandle {
    Arc::new(Mutex::new(Progress::default()))
}

fn set_stage(p: &ProgressHandle, stage: &str) {
    p.lock().unwrap().stage = stage.to_string();
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(120))
        .user_agent(concat!("claudio-code/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

/// Download `url` to `dest`. Resumes from `dest.part` when the server
/// supports ranges, and checks the SHA-256 published by Hugging Face
/// (`X-Linked-Etag`) when present.
pub async fn download(
    url: &str,
    dest: &Path,
    progress: &ProgressHandle,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    if !(url.starts_with("https://")
        || url.starts_with("http://127.0.0.1")
        || url.starts_with("http://localhost"))
    {
        bail!("apenas downloads https são permitidos");
    }
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = dest.with_extension(format!(
        "{}.part",
        dest.extension().and_then(|e| e.to_str()).unwrap_or("bin")
    ));
    let mut have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let mut req = client().get(url);
    if have > 0 {
        req = req.header("Range", format!("bytes={have}-"));
    }
    let resp = tokio::select! {
        _ = cancel.cancelled() => bail!("download cancelado"),
        r = req.send() => r.with_context(|| format!("falha ao conectar em {url}"))?,
    };
    let status = resp.status();
    if !(status.is_success()) {
        bail!("o servidor respondeu HTTP {status} para {url}");
    }
    let expected_sha = resp
        .headers()
        .get("x-linked-etag")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim_matches('"').to_lowercase())
        .filter(|v| v.len() == 64 && v.chars().all(|c| c.is_ascii_hexdigit()));
    let resumed = status.as_u16() == 206;
    if !resumed {
        have = 0; // server ignored the range: start over
    }
    let total = resp.content_length().map(|l| l + have);
    {
        let mut p = progress.lock().unwrap();
        p.stage = format!(
            "Baixando {}",
            dest.file_name().unwrap_or_default().to_string_lossy()
        );
        p.downloaded = have;
        p.total = total;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(&part)?;
    let mut resp = resp;
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => {
                file.flush()?;
                bail!("download cancelado (o arquivo parcial será retomado depois)");
            }
            c = resp.chunk() => c.context("conexão interrompida; tente de novo para continuar")?,
        };
        let Some(chunk) = chunk else { break };
        file.write_all(&chunk)?;
        progress.lock().unwrap().downloaded += chunk.len() as u64;
    }
    file.flush()?;
    drop(file);
    let size = std::fs::metadata(&part)?.len();
    if let Some(t) = total {
        if size != t {
            bail!("download incompleto ({size} de {t} bytes); tente de novo para continuar");
        }
    }
    if let Some(sha) = expected_sha {
        set_stage(progress, "Verificando integridade (SHA-256)");
        let p2 = part.clone();
        let got = tokio::task::spawn_blocking(move || sha256_file(&p2)).await??;
        if got != sha {
            let _ = std::fs::remove_file(&part);
            bail!("arquivo corrompido: SHA-256 não confere (esperado {sha}, obtido {got})");
        }
    }
    std::fs::rename(&part, dest)?;
    Ok(dest.to_path_buf())
}

pub fn sha256_file(p: &Path) -> Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h)?;
    Ok(hex::encode(h.finalize()))
}

// ---------------------------------------------------------------------------
// Local models
// ---------------------------------------------------------------------------

pub fn model_path(m: &LocalModel) -> PathBuf {
    models_dir().join(m.file)
}

pub fn is_downloaded(m: &LocalModel) -> bool {
    model_path(m).is_file()
}

/// File name for a GGUF downloaded from an arbitrary URL.
pub fn file_name_from_url(url: &str) -> Result<String> {
    let u = url::Url::parse(url).context("URL inválida")?;
    let name = u
        .path_segments()
        .and_then(|mut s| s.next_back())
        .filter(|n| !n.is_empty())
        .context("a URL não termina num nome de arquivo")?
        .to_string();
    if !name.to_lowercase().ends_with(".gguf") {
        bail!("a URL deve apontar para um arquivo .gguf");
    }
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        bail!("nome de arquivo inválido");
    }
    Ok(name)
}

/// Make `gguf` the local model ("local" entry) and the default model.
pub fn use_local_model(cfg: &mut Config, entry: ModelConfig) {
    match cfg.models.iter_mut().find(|m| m.name == entry.name) {
        Some(existing) => *existing = entry.clone(),
        None => cfg.models.insert(0, entry.clone()),
    }
    cfg.default_model = Some(entry.name);
}

/// Model entry for any GGUF file (from URL or picked by the user).
pub fn custom_gguf_config(path: &Path) -> ModelConfig {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "local".into());
    ModelConfig {
        name: "local".into(),
        provider: claudio_llm::ProviderKind::OpenaiCompat,
        model: stem,
        base_url: Some("http://127.0.0.1:9931/v1".into()),
        api_key_env: None,
        api_key: None,
        context_window: 8192,
        max_output_tokens: 2048,
        tags: vec!["local".into(), "default".into()],
        gguf_path: Some(path.to_string_lossy().into()),
        temperature: None,
    }
}

// ---------------------------------------------------------------------------
// llama.cpp
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlamaVariant {
    Cpu,
    Vulkan,
}

impl LlamaVariant {
    pub fn label(&self) -> &'static str {
        match self {
            LlamaVariant::Cpu => "CPU",
            LlamaVariant::Vulkan => "GPU (Vulkan: AMD, Intel, NVIDIA)",
        }
    }
}

/// Choose the release asset for this OS/arch. Asset names follow the
/// llama.cpp convention, e.g. `llama-b6000-bin-ubuntu-vulkan-x64.zip`,
/// `llama-b6000-bin-win-cpu-x64.zip`, `llama-b6000-bin-macos-arm64.zip`.
pub fn pick_llama_asset<'a>(
    names: &'a [String],
    os: &str,
    arch: &str,
    variant: LlamaVariant,
) -> Option<&'a String> {
    let arch_tag = match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    let archive = |n: &&String| n.ends_with(".zip") || n.ends_with(".tar.gz");
    let candidates: Vec<&String> = names
        .iter()
        .filter(archive)
        .filter(|n| n.starts_with("llama-") && n.contains("-bin-"))
        .filter(|n| n.contains(&format!("-{arch_tag}.")) || n.contains(&format!("-{arch_tag}-")))
        .collect();
    let has = |n: &str, s: &str| n.contains(s);
    match os {
        "macos" => candidates.into_iter().find(|n| has(n, "-macos-")),
        "windows" => {
            let want = if variant == LlamaVariant::Vulkan {
                "-win-vulkan-"
            } else {
                "-win-cpu-"
            };
            candidates.into_iter().find(|n| has(n, want))
        }
        _ => {
            let linux: Vec<&String> = candidates
                .into_iter()
                .filter(|n| has(n, "-ubuntu-") || has(n, "-linux-"))
                .collect();
            let gpu_words = [
                "vulkan", "cuda", "rocm", "hip", "sycl", "musa", "openvino", "s390x",
            ];
            if variant == LlamaVariant::Vulkan {
                linux.into_iter().find(|n| has(n, "vulkan"))
            } else {
                linux
                    .into_iter()
                    .find(|n| !gpu_words.iter().any(|w| has(n, w)))
            }
        }
    }
}

/// Extract a `.zip` or `.tar.gz` archive into `dest`.
pub fn extract_archive(archive: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let name = archive.to_string_lossy().to_lowercase();
    if name.ends_with(".zip") {
        let f = std::fs::File::open(archive)?;
        let mut z = zip::ZipArchive::new(f).context("zip inválido")?;
        z.extract(dest).context("falha ao extrair o zip")?;
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let f = std::fs::File::open(archive)?;
        let mut t = tar::Archive::new(flate2::read::GzDecoder::new(f));
        t.set_preserve_permissions(true);
        // `unpack` rejects absolute paths and `..` components.
        t.unpack(dest).context("falha ao extrair o tar.gz")?;
    } else {
        bail!("formato de arquivo desconhecido: {}", archive.display());
    }
    Ok(())
}

/// Find `llama-server` inside an extracted folder and make the binaries executable.
pub fn find_llama_server(dir: &Path) -> Option<PathBuf> {
    let exe = claudio_llm::platform::exe_name("llama-server");
    let mut found = None;
    for e in ignore::WalkBuilder::new(dir)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .build()
        .flatten()
    {
        let p = e.path();
        #[cfg(unix)]
        if p.is_file() {
            use std::os::unix::fs::PermissionsExt;
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if name.starts_with("llama-")
                || name.ends_with(".so")
                || name.contains(".so.")
                || name.ends_with(".dylib")
            {
                let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
            }
        }
        if found.is_none() && p.file_name().map(|n| n == exe.as_str()).unwrap_or(false) {
            found = Some(p.to_path_buf());
        }
    }
    found
}

pub const LLAMA_RELEASES_API: &str =
    "https://api.github.com/repos/ggml-org/llama.cpp/releases/latest";

/// Download the latest official llama.cpp build for this machine.
/// Returns the path to `llama-server`.
pub async fn install_llama_cpp(
    api_url: &str,
    variant: LlamaVariant,
    progress: &ProgressHandle,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    set_stage(progress, "Consultando a versão mais recente do llama.cpp");
    let v: serde_json::Value = client()
        .get(api_url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("não foi possível consultar o GitHub")?
        .error_for_status()?
        .json()
        .await?;
    let tag = v["tag_name"].as_str().unwrap_or("latest").to_string();
    let assets = v["assets"].as_array().cloned().unwrap_or_default();
    let names: Vec<String> = assets
        .iter()
        .filter_map(|a| a["name"].as_str().map(String::from))
        .collect();
    let os = std::env::consts::OS;
    let Some(name) = pick_llama_asset(&names, os, std::env::consts::ARCH, variant) else {
        bail!(
            "o release {tag} não tem build pronta de llama.cpp para {os}/{} ({}). Compile a partir do código (docs/MODELOS-LOCAIS.md).",
            std::env::consts::ARCH,
            variant.label()
        );
    };
    let url = assets
        .iter()
        .find(|a| a["name"].as_str() == Some(name.as_str()))
        .and_then(|a| a["browser_download_url"].as_str())
        .context("asset sem URL")?
        .to_string();
    let root = data_dir().join("llama.cpp");
    let archive = root.join(name);
    download(&url, &archive, progress, cancel).await?;
    set_stage(progress, "Extraindo");
    let dest = root.join(&tag);
    let (a2, d2) = (archive.clone(), dest.clone());
    tokio::task::spawn_blocking(move || extract_archive(&a2, &d2)).await??;
    let _ = std::fs::remove_file(&archive);
    let server =
        find_llama_server(&dest).context("llama-server não encontrado no pacote baixado")?;
    Ok(server)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn picks_llama_assets() {
        let n = names(&[
            "cudart-llama-bin-win-cuda-12.4-x64.zip",
            "llama-b6100-bin-macos-arm64.zip",
            "llama-b6100-bin-macos-x64.zip",
            "llama-b6100-bin-ubuntu-x64.zip",
            "llama-b6100-bin-ubuntu-vulkan-x64.zip",
            "llama-b6100-bin-ubuntu-s390x.zip",
            "llama-b6100-bin-win-cpu-x64.zip",
            "llama-b6100-bin-win-cpu-arm64.zip",
            "llama-b6100-bin-win-vulkan-x64.zip",
            "llama-b6100-bin-win-cuda-12.4-x64.zip",
            "llama-b6100-xcframework.zip",
        ]);
        let p = |os, arch, v| pick_llama_asset(&n, os, arch, v).map(|s| s.as_str());
        assert_eq!(
            p("linux", "x86_64", LlamaVariant::Cpu),
            Some("llama-b6100-bin-ubuntu-x64.zip")
        );
        assert_eq!(
            p("linux", "x86_64", LlamaVariant::Vulkan),
            Some("llama-b6100-bin-ubuntu-vulkan-x64.zip")
        );
        assert_eq!(
            p("windows", "x86_64", LlamaVariant::Cpu),
            Some("llama-b6100-bin-win-cpu-x64.zip")
        );
        assert_eq!(
            p("windows", "x86_64", LlamaVariant::Vulkan),
            Some("llama-b6100-bin-win-vulkan-x64.zip")
        );
        assert_eq!(
            p("windows", "aarch64", LlamaVariant::Cpu),
            Some("llama-b6100-bin-win-cpu-arm64.zip")
        );
        assert_eq!(
            p("macos", "aarch64", LlamaVariant::Cpu),
            Some("llama-b6100-bin-macos-arm64.zip")
        );
        assert_eq!(
            p("macos", "x86_64", LlamaVariant::Vulkan),
            Some("llama-b6100-bin-macos-x64.zip")
        );
        assert_eq!(p("linux", "aarch64", LlamaVariant::Cpu), None);
        // newer tar.gz naming
        let t = names(&[
            "llama-b7000-bin-ubuntu-x64.tar.gz",
            "llama-b7000-bin-ubuntu-vulkan-x64.tar.gz",
        ]);
        assert_eq!(
            pick_llama_asset(&t, "linux", "x86_64", LlamaVariant::Cpu).map(|s| s.as_str()),
            Some("llama-b7000-bin-ubuntu-x64.tar.gz")
        );
    }

    #[test]
    fn url_file_names() {
        assert_eq!(
            file_name_from_url(
                "https://huggingface.co/a/b/resolve/main/M-Q4_K_M.gguf?download=true"
            )
            .unwrap(),
            "M-Q4_K_M.gguf"
        );
        assert!(file_name_from_url("https://x.com/model.bin").is_err());
        assert!(file_name_from_url("nota url").is_err());
    }
}
