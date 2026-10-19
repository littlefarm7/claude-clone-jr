//! Audio, video and images: ffprobe/ffmpeg, the `image` crate, OCR.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use jarvis_core::permissions::{Risk, RiskAssessment};
use jarvis_core::tool::{arg_bool, arg_str, arg_u64, Tool, ToolContext, ToolOutput, ToolRegistry};
use jarvis_llm::ToolSpec;
use serde_json::{json, Value};

use crate::shell::{build_command, run_command, which};

pub fn register(r: &mut ToolRegistry) {
    r.register(Arc::new(MediaInfo));
    r.register(Arc::new(MediaConvert));
    r.register(Arc::new(ImageEdit));
    r.register(Arc::new(Ocr));
}

pub async fn probe(p: &Path, ctx: &ToolContext) -> Result<Value> {
    if which("ffprobe").is_none() {
        bail!("ffprobe não instalado (sudo apt install ffmpeg)");
    }
    let args: Vec<String> = ["-v", "error", "-print_format", "json", "-show_format", "-show_streams"].iter().map(|s| s.to_string()).chain([p.to_string_lossy().to_string()]).collect();
    let res = run_command(build_command("ffprobe", &args, &ctx.guard.cwd(), ctx, false), Duration::from_secs(60), ctx).await?;
    if !res.ok() {
        bail!("ffprobe falhou: {}", res.stderr);
    }
    Ok(serde_json::from_str(&res.stdout)?)
}

pub fn summarize_probe(v: &Value) -> String {
    let f = &v["format"];
    let mut s = format!(
        "Formato: {} | duração: {} s | tamanho: {} bytes | bitrate: {}\n",
        f["format_long_name"].as_str().unwrap_or("?"),
        f["duration"].as_str().unwrap_or("?"),
        f["size"].as_str().unwrap_or("?"),
        f["bit_rate"].as_str().unwrap_or("?")
    );
    for st in v["streams"].as_array().cloned().unwrap_or_default() {
        match st["codec_type"].as_str() {
            Some("video") => s.push_str(&format!(
                "- vídeo: {} {}x{} @ {} fps\n",
                st["codec_name"].as_str().unwrap_or("?"),
                st["width"],
                st["height"],
                st["r_frame_rate"].as_str().unwrap_or("?")
            )),
            Some("audio") => s.push_str(&format!(
                "- áudio: {} {} Hz, {} canais\n",
                st["codec_name"].as_str().unwrap_or("?"),
                st["sample_rate"].as_str().unwrap_or("?"),
                st["channels"]
            )),
            Some(t) => s.push_str(&format!("- {t}: {}\n", st["codec_name"].as_str().unwrap_or("?"))),
            None => {}
        }
    }
    if let Some(tags) = f["tags"].as_object() {
        for (k, val) in tags.iter().take(10) {
            s.push_str(&format!("  {k}: {}\n", val.as_str().unwrap_or("")));
        }
    }
    s
}

pub struct MediaInfo;

#[async_trait]
impl Tool for MediaInfo {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "media_info".into(),
            description: "Informações de áudio, vídeo ou imagem (duração, codecs, resolução, metadados).".into(),
            parameters: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let p = ctx.guard.resolve(arg_str(args, "path").unwrap_or(""));
        ctx.guard.assess_read(&p, format!("inspecionar {}", p.display()))
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let p = ctx.guard.resolve(arg_str(&args, "path").unwrap_or(""));
        if let Ok(img) = image::ImageReader::open(&p).and_then(|r| r.with_guessed_format()) {
            if let Some(fmt) = img.format() {
                if let Ok((w, h)) = img.into_dimensions() {
                    return Ok(ToolOutput::ok(format!("Imagem {fmt:?}: {w}x{h} px, {} bytes", std::fs::metadata(&p)?.len())));
                }
            }
        }
        Ok(ToolOutput::ok(summarize_probe(&probe(&p, ctx).await?)))
    }
}

fn valid_time(s: &str) -> bool {
    regex::Regex::new(r"^\d+(\.\d+)?$|^\d{1,2}:\d{2}(:\d{2}(\.\d+)?)?$").unwrap().is_match(s)
}

pub struct MediaConvert;

#[async_trait]
impl Tool for MediaConvert {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "media_convert".into(),
            description: "Converte/edita áudio e vídeo com ffmpeg: formato pela extensão de saída, recorte (start/duration), \
                redimensionar (width), extrair áudio (audio_only), remover áudio (no_audio), quadro único (frame=true com saída .png/.jpg), \
                qualidade (crf), fps, volume."
                .into(),
            parameters: json!({"type":"object","properties":{
                "input":{"type":"string"},"output":{"type":"string"},
                "start":{"type":"string","description":"segundos ou HH:MM:SS"},"duration":{"type":"string"},
                "width":{"type":"integer"},"fps":{"type":"integer"},"crf":{"type":"integer"},
                "audio_only":{"type":"boolean"},"no_audio":{"type":"boolean"},"frame":{"type":"boolean"},
                "volume":{"type":"number"},"audio_bitrate":{"type":"string","description":"ex.: 128k"}
            },"required":["input","output"]}),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let input = ctx.guard.resolve(arg_str(args, "input").unwrap_or(""));
        let out = ctx.guard.resolve(arg_str(args, "output").unwrap_or(""));
        let r = ctx.guard.assess_read(&input, String::new());
        if r.force.is_some() {
            return r;
        }
        let risk = if out.exists() { Risk::Destructive } else { Risk::Write };
        ctx.guard.assess_write(&out, risk, format!("ffmpeg {} → {}{}", input.display(), out.display(), if out.exists() { " (sobrescreve)" } else { "" }))
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        if which("ffmpeg").is_none() {
            bail!("ffmpeg não instalado (sudo apt install ffmpeg)");
        }
        let input = ctx.guard.resolve(arg_str(&args, "input").unwrap_or(""));
        let out = ctx.guard.resolve(arg_str(&args, "output").unwrap_or(""));
        if input == out {
            bail!("entrada e saída não podem ser o mesmo arquivo");
        }
        if let Some(d) = out.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut a: Vec<String> = vec!["-hide_banner".into(), "-nostdin".into(), "-y".into()];
        if let Some(s) = arg_str(&args, "start") {
            if !valid_time(s) {
                bail!("start inválido: {s}");
            }
            a.extend(["-ss".into(), s.into()]);
        }
        a.extend(["-i".into(), input.to_string_lossy().into()]);
        if let Some(d) = arg_str(&args, "duration") {
            if !valid_time(d) {
                bail!("duration inválido: {d}");
            }
            a.extend(["-t".into(), d.into()]);
        }
        let mut vf = Vec::new();
        if let Some(w) = arg_u64(&args, "width") {
            vf.push(format!("scale={w}:-2"));
        }
        if let Some(f) = arg_u64(&args, "fps") {
            vf.push(format!("fps={f}"));
        }
        if arg_bool(&args, "frame").unwrap_or(false) {
            a.extend(["-frames:v".into(), "1".into()]);
        }
        if arg_bool(&args, "audio_only").unwrap_or(false) {
            a.push("-vn".into());
        } else if !vf.is_empty() {
            a.extend(["-vf".into(), vf.join(",")]);
        }
        if arg_bool(&args, "no_audio").unwrap_or(false) {
            a.push("-an".into());
        }
        if let Some(c) = arg_u64(&args, "crf") {
            a.extend(["-crf".into(), c.min(51).to_string()]);
        }
        if let Some(v) = args.get("volume").and_then(|v| v.as_f64()) {
            a.extend(["-af".into(), format!("volume={:.2}", v.clamp(0.0, 10.0))]);
        }
        if let Some(b) = arg_str(&args, "audio_bitrate") {
            if !regex::Regex::new(r"^\d{2,4}k$").unwrap().is_match(b) {
                bail!("audio_bitrate inválido: {b}");
            }
            a.extend(["-b:a".into(), b.into()]);
        }
        a.push(out.to_string_lossy().into());
        let res = run_command(build_command("ffmpeg", &a, &ctx.guard.cwd(), ctx, true), Duration::from_secs(3600), ctx).await?;
        if !res.ok() || !out.exists() {
            let tail: String = res.stderr.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            return Ok(ToolOutput::err(format!("ffmpeg falhou:\n{tail}")));
        }
        let summary = match image::image_dimensions(&out) {
            Ok((w, h)) => format!("imagem {w}x{h}"),
            Err(_) => probe(&out, ctx).await.map(|v| summarize_probe(&v)).unwrap_or_default(),
        };
        Ok(ToolOutput::ok(format!("Gerado {} ({} bytes)\n{summary}", out.display(), std::fs::metadata(&out)?.len())).with_artifact(out))
    }
}

pub struct ImageEdit;

#[async_trait]
impl Tool for ImageEdit {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "image_edit".into(),
            description: "Edita imagens: redimensionar (width/height, mantém proporção se faltar um), recortar (crop: x,y,w,h), \
                girar (rotate 90/180/270), espelhar (flip h|v), tons de cinza, converter formato pela extensão de saída (png, jpg, webp, bmp, gif, tiff)."
                .into(),
            parameters: json!({"type":"object","properties":{
                "input":{"type":"string"},"output":{"type":"string"},
                "width":{"type":"integer"},"height":{"type":"integer"},
                "crop":{"type":"object","properties":{"x":{"type":"integer"},"y":{"type":"integer"},"w":{"type":"integer"},"h":{"type":"integer"}}},
                "rotate":{"type":"integer","enum":[90,180,270]},"flip":{"type":"string","enum":["h","v"]},
                "grayscale":{"type":"boolean"},"blur":{"type":"number"}
            },"required":["input","output"]}),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let input = ctx.guard.resolve(arg_str(args, "input").unwrap_or(""));
        let out = ctx.guard.resolve(arg_str(args, "output").unwrap_or(""));
        let r = ctx.guard.assess_read(&input, String::new());
        if r.force.is_some() {
            return r;
        }
        let risk = if out.exists() { Risk::Destructive } else { Risk::Write };
        ctx.guard.assess_write(&out, risk, format!("editar imagem {} → {}", input.display(), out.display()))
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let input = ctx.guard.resolve(arg_str(&args, "input").unwrap_or(""));
        let out = ctx.guard.resolve(arg_str(&args, "output").unwrap_or(""));
        let a = args.clone();
        let out2 = out.clone();
        let (w, h) = tokio::task::spawn_blocking(move || -> Result<(u32, u32)> {
            let mut img = image::open(&input).with_context(|| format!("abrindo {}", input.display()))?;
            if let Some(c) = a.get("crop") {
                let g = |k: &str| c[k].as_u64().unwrap_or(0) as u32;
                img = img.crop_imm(g("x"), g("y"), g("w").max(1), g("h").max(1));
            }
            let (tw, th) = (arg_u64(&a, "width").map(|v| v as u32), arg_u64(&a, "height").map(|v| v as u32));
            if tw.is_some() || th.is_some() {
                let (ow, oh) = (img.width() as f64, img.height() as f64);
                let w = tw.unwrap_or_else(|| (ow * th.unwrap() as f64 / oh).round() as u32).max(1);
                let h = th.unwrap_or_else(|| (oh * w as f64 / ow).round() as u32).max(1);
                img = img.resize_exact(w, h, image::imageops::FilterType::Lanczos3);
            }
            img = match arg_u64(&a, "rotate") {
                Some(90) => img.rotate90(),
                Some(180) => img.rotate180(),
                Some(270) => img.rotate270(),
                _ => img,
            };
            img = match arg_str(&a, "flip") {
                Some("h") => img.fliph(),
                Some("v") => img.flipv(),
                _ => img,
            };
            if arg_bool(&a, "grayscale").unwrap_or(false) {
                img = image::DynamicImage::ImageLuma8(img.to_luma8());
            }
            if let Some(s) = a.get("blur").and_then(|v| v.as_f64()) {
                img = img.blur(s.clamp(0.1, 50.0) as f32);
            }
            if let Some(d) = out2.parent() {
                std::fs::create_dir_all(d)?;
            }
            let ext = out2.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if ext == "jpg" || ext == "jpeg" {
                img = image::DynamicImage::ImageRgb8(img.to_rgb8()); // JPEG has no alpha
            }
            img.save(&out2).with_context(|| format!("salvando {}", out2.display()))?;
            Ok(image::image_dimensions(&out2)?)
        })
        .await??;
        Ok(ToolOutput::ok(format!("Imagem salva em {} ({w}x{h}).", out.display())).with_artifact(out))
    }
}

pub struct Ocr;

#[async_trait]
impl Tool for Ocr {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ocr".into(),
            description: "Extrai texto de imagens ou PDFs digitalizados com Tesseract (lang ex.: por, eng).".into(),
            parameters: json!({"type":"object","properties":{"path":{"type":"string"},"lang":{"type":"string"}},"required":["path"]}),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let p = ctx.guard.resolve(arg_str(args, "path").unwrap_or(""));
        ctx.guard.assess_read(&p, format!("OCR em {}", p.display()))
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        if which("tesseract").is_none() {
            bail!("tesseract não instalado (sudo apt install tesseract-ocr tesseract-ocr-por)");
        }
        let p = ctx.guard.resolve(arg_str(&args, "path").unwrap_or(""));
        let lang = arg_str(&args, "lang").unwrap_or("por+eng");
        if !regex::Regex::new(r"^[a-z_+]{3,40}$").unwrap().is_match(lang) {
            bail!("lang inválido");
        }
        let tmp = tempfile::tempdir()?;
        let mut images = vec![p.clone()];
        if p.extension().map(|e| e.eq_ignore_ascii_case("pdf")).unwrap_or(false) {
            let prefix = tmp.path().join("pg");
            let a = vec!["-r".into(), "200".into(), "-png".into(), p.to_string_lossy().to_string(), prefix.to_string_lossy().to_string()];
            run_command(build_command("pdftoppm", &a, tmp.path(), ctx, false), Duration::from_secs(300), ctx).await?;
            images = std::fs::read_dir(tmp.path())?.flatten().map(|e| e.path()).collect();
            images.sort();
        }
        let mut text = String::new();
        for img in images {
            let a = vec![img.to_string_lossy().to_string(), "-".into(), "-l".into(), lang.into()];
            let res = run_command(build_command("tesseract", &a, tmp.path(), ctx, false), Duration::from_secs(300), ctx).await?;
            text.push_str(&res.stdout);
            text.push('\n');
        }
        Ok(ToolOutput::ok(text).untrusted(format!("ocr:{}", p.display())))
    }
}
