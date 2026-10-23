//! Voice conversation: microphone capture, end-of-speech detection, speech
//! recognition (whisper.cpp built into Claudio, or the user's cloud account)
//! and spoken answers (piper, or the system voice).
//!
//! Everything a non-technical user needs is installed with one call to
//! [`setup_voice`]: the whisper model, piper and a Brazilian Portuguese voice.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use claudio_core::config::{data_dir, expand, Config};
use claudio_llm::ProviderKind;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::downloads::{self, ProgressHandle};
use crate::shell::which;

pub const SAMPLE_RATE: u32 = 16_000;

// ---------------------------------------------------------------------------
// Files and installation
// ---------------------------------------------------------------------------

pub fn voice_dir() -> PathBuf {
    data_dir().join("voice")
}

/// Speech recognition models (whisper.cpp, quantized).
pub struct WhisperModel {
    pub id: &'static str,
    pub file: &'static str,
    pub size_mb: u64,
}

pub const WHISPER_MODELS: &[WhisperModel] = &[
    WhisperModel {
        id: "small",
        file: "ggml-small-q5_1.bin",
        size_mb: 182,
    },
    WhisperModel {
        id: "base",
        file: "ggml-base-q5_1.bin",
        size_mb: 57,
    },
];

fn whisper_url(file: &str) -> String {
    match std::env::var("CLAUDIO_MODELS_MIRROR") {
        Ok(b) if !b.trim().is_empty() => format!("{}/{file}", b.trim().trim_end_matches('/')),
        _ => format!(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}?download=true"
        ),
    }
}

const PIPER_RELEASE: &str = "https://github.com/rhasspy/piper/releases/download/2023.11.14-2";
const PIPER_VOICE: &str = "pt_BR-faber-medium";

fn piper_voice_url(ext: &str) -> String {
    match std::env::var("CLAUDIO_MODELS_MIRROR") {
        Ok(b) if !b.trim().is_empty() => {
            format!("{}/{PIPER_VOICE}.{ext}", b.trim().trim_end_matches('/'))
        }
        _ => format!(
            "https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/pt/pt_BR/faber/medium/{PIPER_VOICE}.{ext}?download=true"
        ),
    }
}

fn piper_asset() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("piper_linux_x86_64.tar.gz"),
        ("linux", "aarch64") => Some("piper_linux_aarch64.tar.gz"),
        ("windows", "x86_64") => Some("piper_windows_amd64.zip"),
        ("macos", "aarch64") => Some("piper_macos_aarch64.tar.gz"),
        ("macos", "x86_64") => Some("piper_macos_x64.tar.gz"),
        _ => None,
    }
}

fn piper_release_base() -> String {
    std::env::var("CLAUDIO_PIPER_RELEASE").unwrap_or_else(|_| PIPER_RELEASE.into())
}

/// The whisper model in use: configured, or the one installed by setup.
pub fn whisper_model_path(cfg: &Config) -> Option<PathBuf> {
    if let Some(p) = cfg.voice.whisper_model.as_deref().map(expand) {
        if p.is_file() && p.to_string_lossy().ends_with(".bin") {
            return Some(p);
        }
    }
    WHISPER_MODELS
        .iter()
        .map(|m| voice_dir().join(m.file))
        .find(|p| p.is_file())
}

/// piper binary and voice, if available.
pub fn piper_paths(cfg: &Config) -> Option<(PathBuf, PathBuf)> {
    let model = cfg
        .voice
        .piper_model
        .as_deref()
        .map(expand)
        .filter(|p| p.is_file())
        .or_else(|| {
            Some(voice_dir().join(format!("{PIPER_VOICE}.onnx"))).filter(|p| p.is_file())
        })?;
    let exe = claudio_llm::platform::exe_name("piper");
    let bin = cfg
        .voice
        .piper_bin
        .as_deref()
        .map(expand)
        .filter(|p| p.is_file())
        .or_else(|| Some(voice_dir().join("piper").join(&exe)).filter(|p| p.is_file()))
        .or_else(|| which("piper"))?;
    Some((bin, model))
}

/// Whether this processor can run the built-in recognizer (built for AVX2 on x86).
pub fn cpu_supports_local_stt() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        true
    }
}

/// A cloud account that can transcribe audio, if the user has one configured.
pub fn cloud_stt(cfg: &Config) -> Option<(claudio_llm::ModelConfig, &'static str)> {
    for m in &cfg.models {
        if m.resolve_api_key().is_none() {
            continue;
        }
        let base = m.base_url();
        match m.provider {
            ProviderKind::Openai => return Some((m.clone(), "OpenAI")),
            ProviderKind::OpenaiCompat if base.contains("groq.com") => {
                return Some((m.clone(), "Groq"))
            }
            ProviderKind::Gemini => return Some((m.clone(), "Gemini")),
            _ => {}
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct VoiceStatus {
    pub local_stt: bool,
    pub cloud_stt: Option<&'static str>,
    pub piper: bool,
    pub uses_cloud: bool,
}

impl VoiceStatus {
    /// Can listen with the current settings.
    pub fn can_listen(&self) -> bool {
        if self.uses_cloud {
            self.cloud_stt.is_some()
        } else {
            self.local_stt
        }
    }
}

pub fn status(cfg: &Config) -> VoiceStatus {
    VoiceStatus {
        local_stt: whisper_model_path(cfg).is_some() && cpu_supports_local_stt(),
        cloud_stt: cloud_stt(cfg).map(|(_, n)| n),
        piper: piper_paths(cfg).is_some(),
        uses_cloud: cfg.voice.stt == "cloud",
    }
}

/// Download everything needed for local voice conversations.
pub async fn setup_voice(
    cfg: &mut Config,
    progress: &ProgressHandle,
    cancel: &CancellationToken,
    on_phase: impl Fn(&str),
) -> Result<()> {
    let dir = voice_dir();
    std::fs::create_dir_all(&dir)?;
    // 1. Recognition model (smaller one on low-memory computers).
    let ram = claudio_llm::router::detect_resources().mem_total_mb;
    let model = if ram != 0 && ram < 6_000 {
        &WHISPER_MODELS[1]
    } else {
        &WHISPER_MODELS[0]
    };
    let path = dir.join(model.file);
    if !path.is_file() {
        on_phase("Baixando o reconhecimento de voz (etapa 1 de 2)");
        downloads::download(&whisper_url(model.file), &path, progress, cancel).await?;
    }
    cfg.voice.whisper_model = Some(path.to_string_lossy().into_owned());
    if cfg.voice.stt.is_empty() {
        cfg.voice.stt = "local".into();
    }
    // 2. Natural voice (piper). Not fatal: the system voice is the fallback.
    on_phase("Baixando a voz em português (etapa 2 de 2)");
    if let Err(e) = install_piper(&dir, progress, cancel).await {
        if cancel.is_cancelled() {
            return Err(e);
        }
        tracing::warn!("piper não instalado ({e:#}); usando a voz do sistema");
    }
    if let Some((bin, model)) = piper_paths(cfg) {
        cfg.voice.piper_bin = Some(bin.to_string_lossy().into_owned());
        cfg.voice.piper_model = Some(model.to_string_lossy().into_owned());
    }
    Ok(())
}

async fn install_piper(
    dir: &Path,
    progress: &ProgressHandle,
    cancel: &CancellationToken,
) -> Result<()> {
    let exe = claudio_llm::platform::exe_name("piper");
    if !dir.join("piper").join(&exe).is_file() {
        let asset = piper_asset().context("piper não tem versão para este sistema")?;
        let archive = dir.join(asset);
        downloads::download(
            &format!("{}/{asset}", piper_release_base()),
            &archive,
            progress,
            cancel,
        )
        .await?;
        let (a, d) = (archive.clone(), dir.to_path_buf());
        tokio::task::spawn_blocking(move || downloads::extract_archive(&a, &d)).await??;
        let _ = std::fs::remove_file(&archive);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for e in std::fs::read_dir(dir.join("piper"))?.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n == "piper"
                    || n.starts_with("espeak")
                    || n.contains(".so")
                    || n.ends_with(".dylib")
                {
                    let _ =
                        std::fs::set_permissions(e.path(), std::fs::Permissions::from_mode(0o755));
                }
            }
        }
    }
    for ext in ["onnx.json", "onnx"] {
        let p = dir.join(format!("{PIPER_VOICE}.{ext}"));
        if !p.is_file() {
            downloads::download(&piper_voice_url(ext), &p, progress, cancel).await?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Speech recognition
// ---------------------------------------------------------------------------

struct LoadedModel {
    path: PathBuf,
    ctx: whisper_rs::WhisperContext,
}

fn whisper_cache() -> &'static Mutex<Option<LoadedModel>> {
    static C: std::sync::OnceLock<Mutex<Option<LoadedModel>>> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        // Keep whisper.cpp's console output quiet.
        whisper_rs::install_logging_hooks();
        Mutex::new(None)
    })
}

fn lang_code(lang: &str) -> String {
    lang.split(['-', '_']).next().unwrap_or("pt").to_lowercase()
}

/// Transcribe 16 kHz mono samples with the built-in whisper.cpp.
pub async fn transcribe_local(model: PathBuf, samples: Vec<f32>, lang: &str) -> Result<String> {
    if !cpu_supports_local_stt() {
        bail!("este processador não tem AVX2; use o reconhecimento de voz na nuvem");
    }
    let lang = lang_code(lang);
    tokio::task::spawn_blocking(move || -> Result<String> {
        let mut guard = whisper_cache().lock().unwrap();
        if guard.as_ref().map(|m| m.path != model).unwrap_or(true) {
            let ctx = whisper_rs::WhisperContext::new_with_params(
                &model,
                whisper_rs::WhisperContextParameters::default(),
            )
            .map_err(|e| anyhow::anyhow!("não foi possível carregar o modelo de voz: {e}"))?;
            *guard = Some(LoadedModel {
                path: model.clone(),
                ctx,
            });
        }
        let loaded = guard.as_ref().unwrap();
        let mut state = loaded
            .ctx
            .create_state()
            .map_err(|e| anyhow::anyhow!("whisper: {e}"))?;
        let mut p =
            whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        p.set_language(Some(&lang));
        p.set_n_threads(
            std::thread::available_parallelism()
                .map(|n| n.get().min(8) as i32)
                .unwrap_or(4),
        );
        p.set_print_progress(false);
        p.set_print_realtime(false);
        p.set_print_special(false);
        p.set_print_timestamps(false);
        p.set_no_context(true);
        p.set_suppress_blank(true);
        state
            .full(p, &samples)
            .map_err(|e| anyhow::anyhow!("whisper: {e}"))?;
        let mut text = String::new();
        for seg in state.as_iter() {
            if let Ok(t) = seg.to_str_lossy() {
                text.push_str(&t);
            }
        }
        Ok(clean_transcript(&text))
    })
    .await?
}

/// Remove whisper's non-speech markers ("[Música]", "(risos)").
pub fn clean_transcript(text: &str) -> String {
    let re = regex::Regex::new(r"\[[^\]]*\]|\([^)]*\)|♪").unwrap();
    re.replace_all(text, " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// WAV (16-bit PCM) bytes for 16 kHz mono samples.
pub fn wav_bytes(samples: &[f32]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::new(&mut buf, spec).expect("wav");
        for s in samples {
            let _ = w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16);
        }
        let _ = w.finalize();
    }
    buf.into_inner()
}

/// Read a WAV file as 16 kHz mono samples (for tests and the transcribe tool).
pub fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let mut r = hound::WavReader::open(path)
        .with_context(|| format!("WAV inválido: {}", path.display()))?;
    let spec = r.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.unwrap_or(0) as f32 / max)
                .collect()
        }
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap_or(0.0)).collect(),
    };
    let ch = spec.channels.max(1) as usize;
    let mono: Vec<f32> = raw
        .chunks(ch)
        .map(|c| c.iter().sum::<f32>() / ch as f32)
        .collect();
    Ok(resample(&mono, spec.sample_rate, SAMPLE_RATE))
}

/// Linear resampling (good enough for speech).
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let n = (input.len() as f64 / ratio) as usize;
    (0..n)
        .map(|i| {
            let pos = i as f64 * ratio;
            let j = pos as usize;
            let frac = (pos - j as f64) as f32;
            let a = input[j.min(input.len() - 1)];
            let b = input[(j + 1).min(input.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}

/// Transcribe with the user's cloud account (OpenAI, Groq or Gemini).
pub async fn transcribe_cloud(cfg: &Config, samples: &[f32]) -> Result<String> {
    let (m, name) = cloud_stt(cfg).context(
        "nenhuma conta na nuvem com reconhecimento de voz (OpenAI, Groq ou Gemini) está configurada",
    )?;
    let key = m.resolve_api_key().context("sem chave")?;
    let wav = wav_bytes(samples);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let lang = lang_code(&cfg.agent.language);
    let text = match m.provider {
        ProviderKind::Gemini => {
            use base64::Engine;
            let url = format!(
                "{}/models/{}:generateContent",
                m.base_url().trim_end_matches('/'),
                m.model
            );
            let body = serde_json::json!({"contents":[{"role":"user","parts":[
                {"inline_data":{"mime_type":"audio/wav","data": base64::engine::general_purpose::STANDARD.encode(&wav)}},
                {"text":"Transcreva exatamente o que foi falado neste áudio. Responda somente com a transcrição, sem comentários."}
            ]}]});
            let v: serde_json::Value = client
                .post(url)
                .header("x-goog-api-key", key)
                .json(&body)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            v["candidates"][0]["content"]["parts"]
                .as_array()
                .map(|p| {
                    p.iter()
                        .filter_map(|x| x["text"].as_str())
                        .collect::<String>()
                })
                .unwrap_or_default()
        }
        _ => {
            let model = if name == "Groq" {
                "whisper-large-v3-turbo"
            } else {
                "whisper-1"
            };
            let form = reqwest::multipart::Form::new()
                .text("model", model)
                .text("language", lang)
                .part(
                    "file",
                    reqwest::multipart::Part::bytes(wav)
                        .file_name("fala.wav")
                        .mime_str("audio/wav")?,
                );
            let v: serde_json::Value = client
                .post(format!(
                    "{}/audio/transcriptions",
                    m.base_url().trim_end_matches('/')
                ))
                .bearer_auth(key)
                .multipart(form)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            v["text"].as_str().unwrap_or("").to_string()
        }
    };
    Ok(clean_transcript(&text))
}

/// Transcribe with the engine chosen in the settings.
pub async fn transcribe(cfg: &Config, samples: Vec<f32>) -> Result<String> {
    if cfg.voice.stt == "cloud" {
        return transcribe_cloud(cfg, &samples).await;
    }
    let model = whisper_model_path(cfg)
        .context("o reconhecimento de voz ainda não foi instalado (Configurações → Voz)")?;
    transcribe_local(model, samples, &cfg.agent.language).await
}

// ---------------------------------------------------------------------------
// Microphone
// ---------------------------------------------------------------------------

/// Live microphone stream (16 kHz mono f32 chunks).
pub struct Microphone {
    rx: tokio::sync::mpsc::UnboundedReceiver<Vec<f32>>,
    stop: Arc<AtomicBool>,
    _child: Option<tokio::process::Child>,
}

impl Drop for Microphone {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Linux recorders that print raw 16-bit 16 kHz mono audio to stdout.
fn linux_recorders() -> Vec<(String, Vec<String>)> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut v = Vec::new();
    if let Ok(cmd) = std::env::var("CLAUDIO_MIC_CMD") {
        // Tests and unusual setups: any command printing s16le 16 kHz mono.
        if let Ok(parts) = shell_words::split(&cmd) {
            if let Some((p, a)) = parts.split_first() {
                v.push((p.clone(), a.to_vec()));
            }
        }
        return v;
    }
    if which("parecord").is_some() {
        v.push((
            "parecord".into(),
            s(&["--raw", "--rate=16000", "--channels=1", "--format=s16le"]),
        ));
    }
    if which("arecord").is_some() {
        v.push((
            "arecord".into(),
            s(&["-q", "-f", "S16_LE", "-r", "16000", "-c", "1", "-t", "raw"]),
        ));
    }
    if which("pw-record").is_some() {
        v.push((
            "pw-record".into(),
            s(&["--rate", "16000", "--channels", "1", "--format", "s16", "-"]),
        ));
    }
    if which("ffmpeg").is_some() {
        v.push((
            "ffmpeg".into(),
            s(&[
                "-loglevel",
                "quiet",
                "-f",
                "pulse",
                "-i",
                "default",
                "-ac",
                "1",
                "-ar",
                "16000",
                "-f",
                "s16le",
                "-",
            ]),
        ));
    }
    v
}

impl Microphone {
    pub fn open() -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        #[cfg(any(windows, target_os = "macos"))]
        if std::env::var("CLAUDIO_MIC_CMD").is_err() {
            // cpal streams must stay on the thread that created them: keep the
            // stream on its own thread until the microphone is dropped.
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let stop2 = stop.clone();
            std::thread::spawn(move || match cpal_input(tx, stop2.clone()) {
                Ok(stream) => {
                    let _ = ready_tx.send(Ok(()));
                    while !stop2.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            });
            ready_rx
                .recv_timeout(Duration::from_secs(10))
                .context("o microfone não respondeu")??;
            return Ok(Self {
                rx,
                stop,
                _child: None,
            });
        }
        let recorders = linux_recorders();
        let mut last_err = None;
        for (prog, args) in recorders {
            match tokio::process::Command::new(&prog)
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
            {
                Ok(mut child) => {
                    let mut out = child.stdout.take().context("stdout")?;
                    let stop2 = stop.clone();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 3200];
                        let mut carry: Option<u8> = None;
                        while !stop2.load(Ordering::SeqCst) {
                            let n = match out.read(&mut buf).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => n,
                            };
                            let mut bytes: Vec<u8> = carry.take().into_iter().collect();
                            bytes.extend_from_slice(&buf[..n]);
                            if bytes.len() % 2 == 1 {
                                carry = bytes.pop();
                            }
                            let chunk: Vec<f32> = bytes
                                .chunks_exact(2)
                                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                                .collect();
                            if tx.send(chunk).is_err() {
                                break;
                            }
                        }
                    });
                    return Ok(Self {
                        rx,
                        stop,
                        _child: Some(child),
                    });
                }
                Err(e) => last_err = Some(format!("{prog}: {e}")),
            }
        }
        bail!(
            "não encontrei como gravar do microfone ({}). Instale o pacote pulseaudio-utils ou alsa-utils.",
            last_err.unwrap_or_else(|| "nenhum gravador disponível".into())
        )
    }

    pub async fn next_chunk(&mut self) -> Option<Vec<f32>> {
        self.rx.recv().await
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn cpal_input(
    tx: tokio::sync::mpsc::UnboundedSender<Vec<f32>>,
    stop: Arc<AtomicBool>,
) -> Result<cpal::Stream> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = cpal::default_host();
    let dev = host
        .default_input_device()
        .context("nenhum microfone encontrado")?;
    let conf = dev
        .default_input_config()
        .context("microfone sem configuração")?;
    let rate = conf.sample_rate().0;
    let ch = conf.channels() as usize;
    let err = |e| tracing::warn!("microfone: {e}");
    let send = move |mono: Vec<f32>| {
        if !stop.load(Ordering::SeqCst) {
            let _ = tx.send(resample(&mono, rate, SAMPLE_RATE));
        }
    };
    let stream = match conf.sample_format() {
        cpal::SampleFormat::F32 => dev.build_input_stream(
            &conf.into(),
            move |d: &[f32], _: &_| {
                send(
                    d.chunks(ch)
                        .map(|c| c.iter().sum::<f32>() / ch as f32)
                        .collect(),
                )
            },
            err,
            None,
        )?,
        cpal::SampleFormat::I16 => dev.build_input_stream(
            &conf.into(),
            move |d: &[i16], _: &_| {
                send(
                    d.chunks(ch)
                        .map(|c| c.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / ch as f32)
                        .collect(),
                )
            },
            err,
            None,
        )?,
        cpal::SampleFormat::U16 => dev.build_input_stream(
            &conf.into(),
            move |d: &[u16], _: &_| {
                send(
                    d.chunks(ch)
                        .map(|c| {
                            c.iter()
                                .map(|&s| (s as f32 - 32768.0) / 32768.0)
                                .sum::<f32>()
                                / ch as f32
                        })
                        .collect(),
                )
            },
            err,
            None,
        )?,
        other => bail!("formato de microfone não suportado: {other:?}"),
    };
    stream.play()?;
    Ok(stream)
}

// ---------------------------------------------------------------------------
// End-of-speech detection
// ---------------------------------------------------------------------------

/// Energy-based voice activity detector with an adaptive noise floor.
pub struct Vad {
    frame: Vec<f32>,
    noise: f32,
    calibrated: usize,
    voiced_run: usize,
    silence_run: usize,
    pub speaking: bool,
    pre_roll: std::collections::VecDeque<Vec<f32>>,
    pub utterance: Vec<f32>,
    /// Silence (ms) that ends an utterance.
    pub end_silence_ms: usize,
}

pub enum VadEvent {
    None,
    Started,
    Ended,
}

const FRAME: usize = 480; // 30 ms

impl Default for Vad {
    fn default() -> Self {
        Self {
            frame: Vec::with_capacity(FRAME),
            noise: 0.004,
            calibrated: 0,
            voiced_run: 0,
            silence_run: 0,
            speaking: false,
            pre_roll: Default::default(),
            utterance: Vec::new(),
            end_silence_ms: 900,
        }
    }
}

impl Vad {
    /// Current loudness (0..1) for a level meter.
    pub fn level(&self) -> f32 {
        (self.noise * 10.0).min(1.0)
    }

    pub fn push(&mut self, samples: &[f32]) -> VadEvent {
        let mut ev = VadEvent::None;
        for &s in samples {
            self.frame.push(s);
            if self.frame.len() < FRAME {
                continue;
            }
            let frame = std::mem::take(&mut self.frame);
            let rms = (frame.iter().map(|x| x * x).sum::<f32>() / FRAME as f32).sqrt();
            // The first ~300 ms set the noise floor; later quiet frames adapt it.
            if self.calibrated < 10 {
                self.noise = if self.calibrated == 0 {
                    rms
                } else {
                    self.noise.max(rms)
                };
                self.calibrated += 1;
            }
            let threshold = (self.noise * 3.0).max(0.012);
            let voiced = rms > threshold;
            if !voiced && !self.speaking {
                self.noise = self.noise * 0.95 + rms * 0.05;
            }
            if self.speaking {
                self.utterance.extend_from_slice(&frame);
                if voiced {
                    self.silence_run = 0;
                } else {
                    self.silence_run += 1;
                    if self.silence_run * 30 >= self.end_silence_ms {
                        self.speaking = false;
                        ev = VadEvent::Ended;
                    }
                }
            } else {
                self.pre_roll.push_back(frame.clone());
                if self.pre_roll.len() > 10 {
                    self.pre_roll.pop_front();
                }
                self.voiced_run = if voiced { self.voiced_run + 1 } else { 0 };
                if self.voiced_run >= 3 {
                    self.speaking = true;
                    self.silence_run = 0;
                    self.utterance = self.pre_roll.drain(..).flatten().collect();
                    ev = VadEvent::Started;
                }
            }
            if matches!(ev, VadEvent::Ended) {
                return ev;
            }
        }
        ev
    }
}

/// What the listener is doing (for the interface).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ListenState {
    Waiting,
    Hearing,
}

/// Record one utterance: waits for speech, stops after a pause.
/// Returns None if nobody spoke within `wait` or on cancel.
pub async fn listen_utterance(
    cancel: &CancellationToken,
    wait: Duration,
    on_state: impl Fn(ListenState),
) -> Result<Option<Vec<f32>>> {
    let mut mic = Microphone::open()?;
    let mut vad = Vad::default();
    // Time is counted in audio received, not wall-clock time.
    let mut heard = 0usize;
    let mut started_at: Option<usize> = None;
    let wait = wait.as_secs_f32() * SAMPLE_RATE as f32;
    let max = 30 * SAMPLE_RATE as usize;
    on_state(ListenState::Waiting);
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Ok(None),
            c = tokio::time::timeout(Duration::from_secs(5), mic.next_chunk()) => match c {
                Ok(Some(c)) => c,
                Ok(None) => bail!("o microfone parou de enviar áudio"),
                Err(_) => bail!("o microfone não está enviando áudio (verifique se ele está conectado e liberado)"),
            },
        };
        heard += chunk.len();
        match vad.push(&chunk) {
            VadEvent::Started => {
                started_at = Some(heard);
                on_state(ListenState::Hearing);
            }
            VadEvent::Ended => return Ok(Some(std::mem::take(&mut vad.utterance))),
            VadEvent::None => {}
        }
        match started_at {
            None if heard as f32 > wait => return Ok(None),
            Some(s) if heard - s > max => return Ok(Some(std::mem::take(&mut vad.utterance))),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Speaking
// ---------------------------------------------------------------------------

/// Turn a chat answer into something pleasant to hear.
pub fn clean_for_speech(text: &str) -> String {
    let code = regex::Regex::new(r"(?s)```.*?```").unwrap();
    let url = regex::Regex::new(r"https?://\S+").unwrap();
    let md = regex::Regex::new(r"[*_#`>|]+").unwrap();
    let mut s = code
        .replace_all(text, " (o código está na tela) ")
        .into_owned();
    s = url.replace_all(&s, " (link na tela) ").into_owned();
    s = md.replace_all(&s, " ").into_owned();
    let s = s
        .lines()
        .map(|l| l.trim().trim_start_matches(['-', '•']).trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(". ")
        .replace("..", ".");
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 700 {
        let cut: String = s.chars().take(700).collect();
        let end = cut.rfind(['.', '!', '?']).unwrap_or(cut.len());
        format!(
            "{} O restante está na tela.",
            &cut[..=end.min(cut.len() - 1)]
        )
    } else {
        s
    }
}

async fn run_quiet(
    prog: &Path,
    args: &[String],
    stdin_text: Option<&str>,
    cancel: &CancellationToken,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new(prog)
        .args(args)
        .stdin(if stdin_text.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("falha ao iniciar {}", prog.display()))?;
    if let Some(t) = stdin_text {
        if let Some(mut i) = child.stdin.take() {
            i.write_all(t.as_bytes()).await?;
        }
    }
    let status = tokio::select! {
        _ = cancel.cancelled() => { let _ = child.kill().await; return Ok(()) }
        s = child.wait() => s?,
    };
    if !status.success() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err).await;
        }
        bail!("{} falhou: {}", prog.display(), err.trim());
    }
    Ok(())
}

/// Play a WAV file with the system player (stops on cancel).
pub async fn play_wav(path: &Path, cancel: &CancellationToken) -> Result<()> {
    let p = path.to_string_lossy().to_string();
    if cfg!(windows) {
        let script = format!(
            "(New-Object Media.SoundPlayer '{}').PlaySync()",
            p.replace('\'', "''")
        );
        let ps = which("powershell").unwrap_or_else(|| PathBuf::from("powershell"));
        return run_quiet(
            &ps,
            &["-NoProfile".into(), "-Command".into(), script],
            None,
            cancel,
        )
        .await;
    }
    if cfg!(target_os = "macos") {
        return run_quiet(Path::new("afplay"), &[p], None, cancel).await;
    }
    for player in ["paplay", "pw-play", "aplay"] {
        if let Some(bin) = which(player) {
            let mut args = vec![p.clone()];
            if player == "aplay" {
                args.insert(0, "-q".into());
            }
            return run_quiet(&bin, &args, None, cancel).await;
        }
    }
    bail!("nenhum reprodutor de áudio (paplay, pw-play ou aplay)")
}

/// Speak `text` aloud (blocks until done or cancelled).
pub async fn speak(cfg: &Config, text: &str, cancel: &CancellationToken) -> Result<()> {
    let text = clean_for_speech(text);
    if text.is_empty() {
        return Ok(());
    }
    if let Some((bin, model)) = piper_paths(cfg) {
        let out = std::env::temp_dir().join(format!("claudio-fala-{}.wav", std::process::id()));
        run_quiet(
            &bin,
            &[
                "--model".into(),
                model.to_string_lossy().into(),
                "--output_file".into(),
                out.to_string_lossy().into(),
            ],
            Some(&text),
            cancel,
        )
        .await?;
        let r = play_wav(&out, cancel).await;
        let _ = std::fs::remove_file(&out);
        return r;
    }
    if cfg!(target_os = "macos") {
        return run_quiet(Path::new("say"), &["--".into(), text], None, cancel).await;
    }
    if cfg!(windows) {
        let script = "Add-Type -AssemblyName System.Speech; $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; $s.Speak([Console]::In.ReadToEnd())";
        let ps = which("powershell").unwrap_or_else(|| PathBuf::from("powershell"));
        return run_quiet(
            &ps,
            &["-NoProfile".into(), "-Command".into(), script.into()],
            Some(&text),
            cancel,
        )
        .await;
    }
    if let Some(es) = which("espeak-ng").or_else(|| which("espeak")) {
        let voice = cfg
            .voice
            .espeak_voice
            .clone()
            .unwrap_or_else(|| "pt-br".into());
        return run_quiet(&es, &["-v".into(), voice, "--".into(), text], None, cancel).await;
    }
    bail!("nenhuma voz instalada: ative a voz em Configurações → Voz")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(ms: usize, amp: f32) -> Vec<f32> {
        (0..ms * 16)
            .map(|i| (i as f32 * 0.12).sin() * amp)
            .collect()
    }

    #[test]
    fn vad_detects_utterance() {
        let mut v = Vad::default();
        let mut events = vec![];
        for chunk in [tone(600, 0.001), tone(1200, 0.3), tone(1200, 0.001)] {
            for c in chunk.chunks(1600) {
                match v.push(c) {
                    VadEvent::Started => events.push("start"),
                    VadEvent::Ended => events.push("end"),
                    VadEvent::None => {}
                }
            }
        }
        assert_eq!(events, vec!["start", "end"]);
        let secs = v.utterance.len() as f32 / 16000.0;
        assert!((1.2..2.6).contains(&secs), "{secs}");
    }

    #[test]
    fn speech_text_is_cleaned() {
        let s = clean_for_speech(
            "# Título\n- **um** item\n```rust\nfn x(){}\n```\nVeja https://exemplo.com/a.",
        );
        assert!(!s.contains('#') && !s.contains('*') && !s.contains("fn x"));
        assert!(
            s.contains("código está na tela") && s.contains("link na tela"),
            "{s}"
        );
        assert_eq!(
            clean_transcript(" [Música] Olá,  mundo (risos) "),
            "Olá, mundo"
        );
    }

    #[test]
    fn resampling_and_wav() {
        let x: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.01).sin()).collect();
        let y = resample(&x, 48_000, 16_000);
        assert_eq!(y.len(), 16_000);
        let wav = wav_bytes(&y);
        assert_eq!(&wav[..4], b"RIFF");
    }
}
