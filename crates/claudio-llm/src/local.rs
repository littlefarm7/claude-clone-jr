//! Lifecycle of a local llama.cpp `llama-server` (always bound to 127.0.0.1).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tokio::process::{Child, Command};

#[derive(Debug, Clone)]
pub struct LlamaServerOptions {
    pub binary: Option<PathBuf>,
    pub gguf: PathBuf,
    pub port: u16,
    pub ctx_size: u32,
    /// Layers to offload to GPU (Vulkan/ROCm builds). 0 = CPU only.
    pub gpu_layers: u32,
    pub threads: Option<usize>,
    pub extra_args: Vec<String>,
}

pub struct LlamaServer {
    child: Child,
    pub base_url: String,
}

/// Root URL of an OpenAI-compatible base (strip trailing `/v1`).
pub fn root_url(base_url: &str) -> String {
    base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .to_string()
}

pub async fn health(base_url: &str) -> bool {
    let url = format!("{}/health", root_url(base_url));
    let client = crate::http::client(Duration::from_secs(3));
    matches!(client.get(&url).send().await, Ok(r) if r.status().is_success())
}

pub fn find_binary(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return p.exists().then(|| p.to_path_buf());
    }
    if let Some(p) = crate::platform::which("llama-server") {
        return Some(p);
    }
    let exe = crate::platform::exe_name("llama-server");
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = crate::platform::home_dir() {
        candidates.push(home.join(".local/share/claudio/bin").join(&exe));
    }
    // Next to our own executable (AppImage / zip / .app bundle ship it there).
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            candidates.push(dir.join(&exe));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

impl LlamaServer {
    pub async fn start(opts: &LlamaServerOptions) -> Result<Self> {
        let bin = find_binary(opts.binary.as_deref())
            .context("llama-server não encontrado. Instale o llama.cpp (veja README) ou defina local.llama_server")?;
        if !opts.gguf.is_file() {
            bail!("arquivo GGUF não encontrado: {}", opts.gguf.display());
        }
        let mut cmd = Command::new(&bin);
        cmd.arg("-m")
            .arg(&opts.gguf)
            .args(["--host", "127.0.0.1", "--port", &opts.port.to_string()])
            .args(["-c", &opts.ctx_size.to_string()])
            .args(["-ngl", &opts.gpu_layers.to_string()])
            .arg("--jinja")
            .args(&opts.extra_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(t) = opts.threads {
            cmd.args(["-t", &t.to_string()]);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("falha ao iniciar {}", bin.display()))?;
        // Read stderr from the start: llama-server logs a lot while loading a
        // big model, and a full pipe would block it before it becomes ready.
        let log: Arc<Mutex<VecDeque<String>>> = Arc::default();
        if let Some(e) = child.stderr.take() {
            let log = log.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let mut lines = tokio::io::BufReader::new(e).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let mut q = log.lock().unwrap();
                    if q.len() == 20 {
                        q.pop_front();
                    }
                    q.push_back(l);
                }
            });
        }
        let base_url = format!("http://127.0.0.1:{}/v1", opts.port);
        let mut server = Self { child, base_url };
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(600) {
            if let Some(status) = server.child.try_wait()? {
                // Give the reader a moment to collect the last lines.
                tokio::time::sleep(Duration::from_millis(200)).await;
                let tail = log
                    .lock()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");
                bail!("llama-server saiu com {status}:\n{tail}");
            }
            if health(&server.base_url).await {
                return Ok(server);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        bail!("llama-server não ficou pronto em 600 s")
    }

    pub async fn stop(mut self) {
        let _ = self.child.kill().await;
    }
}
