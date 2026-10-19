//! Voice I/O prepared for local engines: whisper.cpp (STT), piper / espeak-ng (TTS).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use jarvis_core::config::{expand, Config};
use jarvis_core::permissions::{Risk, RiskAssessment};
use jarvis_core::tool::{arg_str, Tool, ToolContext, ToolOutput, ToolRegistry};
use jarvis_llm::ToolSpec;
use serde_json::{json, Value};

use crate::shell::{build_command, run_command, which};

pub fn register(r: &mut ToolRegistry) {
    r.register(Arc::new(Transcribe));
    r.register(Arc::new(Speak));
}

pub fn whisper_bin(cfg: &Config) -> Option<PathBuf> {
    cfg.voice.whisper_bin.as_deref().map(expand).or_else(|| {
        ["whisper-cli", "whisper-cpp", "whisper"]
            .iter()
            .find_map(|b| which(b))
    })
}

/// Status line for `jarvis doctor` and the GUI.
pub fn status(cfg: &Config) -> (bool, bool) {
    let stt = whisper_bin(cfg).is_some()
        && cfg
            .voice
            .whisper_model
            .as_deref()
            .map(|m| expand(m).is_file())
            .unwrap_or(false);
    let tts = (cfg.voice.piper_model.is_some()
        && (cfg.voice.piper_bin.is_some() || which("piper").is_some()))
        || which("espeak-ng").is_some();
    (stt, tts)
}

/// Speech to text. Converts any input to 16 kHz mono WAV with ffmpeg first.
pub async fn transcribe(path: &Path, lang: &str, ctx: &ToolContext) -> Result<String> {
    let bin = whisper_bin(&ctx.cfg).context("whisper.cpp não encontrado (veja docs/VOICE.md)")?;
    let model = ctx
        .cfg
        .voice
        .whisper_model
        .as_deref()
        .map(expand)
        .context("defina voice.whisper_model (arquivo ggml do whisper.cpp)")?;
    if !model.is_file() {
        bail!("modelo whisper não encontrado: {}", model.display());
    }
    if which("ffmpeg").is_none() {
        bail!("ffmpeg não instalado (sudo apt install ffmpeg)");
    }
    let tmp = tempfile::tempdir()?;
    let wav = tmp.path().join("in.wav");
    let a: Vec<String> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-i".into(),
        path.to_string_lossy().into(),
        "-ar".into(),
        "16000".into(),
        "-ac".into(),
        "1".into(),
        wav.to_string_lossy().into(),
    ];
    let r = run_command(
        build_command("ffmpeg", &a, tmp.path(), ctx, false),
        Duration::from_secs(600),
        ctx,
    )
    .await?;
    if !r.ok() {
        bail!("ffmpeg falhou ao preparar o áudio: {}", r.stderr);
    }
    let a: Vec<String> = vec![
        "-m".into(),
        model.to_string_lossy().into(),
        "-l".into(),
        lang.into(),
        "-nt".into(),
        "-f".into(),
        wav.to_string_lossy().into(),
    ];
    let r = run_command(
        build_command(&bin.to_string_lossy(), &a, tmp.path(), ctx, false),
        Duration::from_secs(3600),
        ctx,
    )
    .await?;
    if !r.ok() {
        bail!("whisper falhou: {}", r.stderr);
    }
    Ok(r.stdout.trim().to_string())
}

/// Text to speech into a WAV file.
pub async fn speak(text: &str, out: &Path, ctx: &ToolContext) -> Result<String> {
    if let Some(model) = ctx.cfg.voice.piper_model.as_deref().map(expand) {
        let bin = ctx
            .cfg
            .voice
            .piper_bin
            .as_deref()
            .map(expand)
            .or_else(|| which("piper"))
            .context("piper não encontrado")?;
        let tmp = tempfile::tempdir()?;
        let txt = tmp.path().join("in.txt");
        std::fs::write(&txt, text)?;
        // piper reads stdin; use sh redirection from a file we control (no interpolation of user text).
        let a: Vec<String> = vec![
            "-c".into(),
            "\"$0\" --model \"$1\" --output_file \"$2\" < \"$3\"".into(),
            bin.to_string_lossy().into(),
            model.to_string_lossy().into(),
            out.to_string_lossy().into(),
            txt.to_string_lossy().into(),
        ];
        let r = run_command(
            build_command("sh", &a, tmp.path(), ctx, false),
            Duration::from_secs(600),
            ctx,
        )
        .await?;
        if !r.ok() {
            bail!("piper falhou: {}", r.stderr);
        }
        return Ok("piper".into());
    }
    if which("espeak-ng").is_some() {
        let voice = ctx
            .cfg
            .voice
            .espeak_voice
            .clone()
            .unwrap_or_else(|| "pt-br".into());
        let a: Vec<String> = vec![
            "-v".into(),
            voice,
            "-w".into(),
            out.to_string_lossy().into(),
            "--".into(),
            text.into(),
        ];
        let r = run_command(
            build_command("espeak-ng", &a, &ctx.guard.cwd(), ctx, false),
            Duration::from_secs(300),
            ctx,
        )
        .await?;
        if !r.ok() {
            bail!("espeak-ng falhou: {}", r.stderr);
        }
        return Ok("espeak-ng".into());
    }
    bail!("nenhum motor de voz: instale espeak-ng (sudo apt install espeak-ng) ou configure piper (docs/VOICE.md)")
}

pub struct Transcribe;

#[async_trait]
impl Tool for Transcribe {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "transcribe".into(),
            description: "Transcreve áudio ou vídeo para texto localmente com whisper.cpp.".into(),
            parameters: json!({"type":"object","properties":{"path":{"type":"string"},"language":{"type":"string","description":"pt, en, auto..."}},"required":["path"]}),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let p = ctx.guard.resolve(arg_str(args, "path").unwrap_or(""));
        ctx.guard
            .assess_read(&p, format!("transcrever {}", p.display()))
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let p = ctx.guard.resolve(arg_str(&args, "path").unwrap_or(""));
        let lang = arg_str(&args, "language").unwrap_or("pt");
        if !lang.chars().all(|c| c.is_ascii_alphabetic()) || lang.len() > 5 {
            bail!("language inválido");
        }
        let text = transcribe(&p, lang, ctx).await?;
        Ok(ToolOutput::ok(text).untrusted(format!("audio:{}", p.display())))
    }
}

pub struct Speak;

#[async_trait]
impl Tool for Speak {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "speak".into(),
            description:
                "Sintetiza fala (WAV) a partir de texto com piper ou espeak-ng, localmente.".into(),
            parameters: json!({"type":"object","properties":{"text":{"type":"string"},"output":{"type":"string"}},"required":["text","output"]}),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let p = ctx.guard.resolve(arg_str(args, "output").unwrap_or(""));
        ctx.guard
            .assess_write(&p, Risk::Write, format!("gerar áudio {}", p.display()))
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let out = ctx
            .guard
            .resolve(arg_str(&args, "output").unwrap_or("fala.wav"));
        if let Some(d) = out.parent() {
            std::fs::create_dir_all(d)?;
        }
        let engine = speak(arg_str(&args, "text").unwrap_or(""), &out, ctx).await?;
        Ok(ToolOutput::ok(format!(
            "Áudio gerado em {} via {engine} ({} bytes)",
            out.display(),
            std::fs::metadata(&out)?.len()
        ))
        .with_artifact(out))
    }
}
