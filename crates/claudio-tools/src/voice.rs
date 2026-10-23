//! Voice I/O prepared for local engines: whisper.cpp (STT), piper / espeak-ng (TTS).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use claudio_core::config::{expand, Config};
use claudio_core::permissions::{Risk, RiskAssessment};
use claudio_core::tool::{arg_str, Tool, ToolContext, ToolOutput, ToolRegistry};
use claudio_llm::ToolSpec;
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

/// Status line for `claudio doctor` and the GUI.
pub fn status(cfg: &Config) -> (bool, bool) {
    let stt = crate::speech::status(cfg).local_stt
        || whisper_bin(cfg).is_some()
            && cfg
                .voice
                .whisper_model
                .as_deref()
                .map(|m| expand(m).is_file())
                .unwrap_or(false);
    let tts = (cfg.voice.piper_model.is_some()
        && (cfg.voice.piper_bin.is_some() || which("piper").is_some()))
        || which("espeak-ng").is_some()
        || cfg!(windows)
        || (cfg!(target_os = "macos") && which("say").is_some());
    (stt, tts)
}

/// Speech to text. Converts any input to 16 kHz mono WAV with ffmpeg first.
pub async fn transcribe(path: &Path, lang: &str, ctx: &ToolContext) -> Result<String> {
    // Built-in whisper.cpp (installed with "Ativar voz") when available.
    if let Some(model) = crate::speech::whisper_model_path(&ctx.cfg)
        .filter(|_| crate::speech::cpu_supports_local_stt())
    {
        let is_wav = path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("wav"))
            .unwrap_or(false);
        let tmp = tempfile::tempdir()?;
        let wav = if is_wav {
            path.to_path_buf()
        } else {
            if which("ffmpeg").is_none() {
                bail!("para transcrever este formato instale o ffmpeg (ou envie um arquivo .wav)");
            }
            let out = tmp.path().join("in.wav");
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
                out.to_string_lossy().into(),
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
            out
        };
        let samples = crate::speech::read_wav(&wav)?;
        return crate::speech::transcribe_local(model, samples, lang).await;
    }
    let bin = whisper_bin(&ctx.cfg).context(
        "reconhecimento de voz não instalado: ative em Configurações → Voz (ou instale o whisper.cpp)",
    )?;
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
        // piper reads the text from stdin: pipe it directly (no shell, no interpolation).
        use tokio::io::AsyncWriteExt;
        let mut child = tokio::process::Command::new(bin)
            .arg("--model")
            .arg(&model)
            .arg("--output_file")
            .arg(out)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("falha ao iniciar o piper")?;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(text.as_bytes()).await?;
        drop(stdin);
        let o = tokio::time::timeout(Duration::from_secs(600), child.wait_with_output())
            .await
            .context("piper: tempo esgotado")??;
        if !o.status.success() {
            bail!("piper falhou: {}", String::from_utf8_lossy(&o.stderr));
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
    if cfg!(target_os = "macos") && which("say").is_some() {
        // Built-in macOS speech synthesis.
        let a: Vec<String> = vec![
            "-o".into(),
            out.to_string_lossy().into(),
            "--data-format=LEI16@22050".into(),
            "--".into(),
            text.into(),
        ];
        let r = run_command(
            build_command("say", &a, &ctx.guard.cwd(), ctx, false),
            Duration::from_secs(300),
            ctx,
        )
        .await?;
        if !r.ok() {
            bail!("say falhou: {}", r.stderr);
        }
        return Ok("say (macOS)".into());
    }
    if cfg!(windows) {
        // Built-in Windows SAPI via System.Speech; text and path travel in env vars.
        let script = "Add-Type -AssemblyName System.Speech; \
            $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
            $s.SetOutputToWaveFile($env:CLAUDIO_TTS_OUT); $s.Speak($env:CLAUDIO_TTS_TEXT); $s.Dispose()";
        let a: Vec<String> = [
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut cmd = build_command("powershell", &a, &ctx.guard.cwd(), ctx, false);
        cmd.env("CLAUDIO_TTS_OUT", out)
            .env("CLAUDIO_TTS_TEXT", text);
        let r = run_command(cmd, Duration::from_secs(300), ctx).await?;
        if !r.ok() || !out.exists() {
            bail!("síntese de voz do Windows falhou: {}", r.stderr);
        }
        return Ok("SAPI (Windows)".into());
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
