use std::collections::BTreeMap;
use std::sync::Arc;

use jarvis_core::config::{Config, CustomToolConfig, McpServerConfig};
use jarvis_core::tool::{Tool, ToolRegistry};
use jarvis_tools::media::*;
use jarvis_tools::testutil::context;
use serde_json::json;

fn python() -> &'static str {
    if cfg!(windows) {
        "python"
    } else {
        "python3"
    }
}

fn have(b: &str) -> bool {
    jarvis_tools::shell::which(b).is_some()
}

#[tokio::test]
async fn ffmpeg_pipeline_and_image_edit() {
    if !have("ffmpeg") {
        eprintln!("ignorado: ffmpeg ausente");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let st = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=10",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440",
            "-t",
            "2",
            "-pix_fmt",
            "yuv420p",
            "-shortest",
        ])
        .arg(dir.path().join("in.mp4"))
        .status()
        .unwrap();
    assert!(st.success());
    let info = MediaInfo
        .call(json!({"path": "in.mp4"}), &ctx)
        .await
        .unwrap();
    assert!(
        info.content.contains("320x240") || info.content.contains("320"),
        "{}",
        info.content
    );
    assert!(info.content.contains("áudio"), "{}", info.content);
    let out = MediaConvert.call(json!({"input": "in.mp4", "output": "out/audio.mp3", "audio_only": true, "audio_bitrate": "64k"}), &ctx).await.unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("mp3"), "{}", out.content);
    let out = MediaConvert.call(json!({"input": "in.mp4", "output": "frame.png", "start": "1", "frame": true, "width": 160}), &ctx).await.unwrap();
    assert!(out.content.contains("imagem 160x120"), "{}", out.content);
    // argument injection is rejected
    let bad = MediaConvert
        .call(
            json!({"input": "in.mp4", "output": "x.mp4", "start": "1; rm -rf /"}),
            &ctx,
        )
        .await;
    assert!(bad.is_err());
    let out = ImageEdit.call(json!({"input": "frame.png", "output": "small.jpg", "width": 80, "grayscale": true, "rotate": 90}), &ctx).await.unwrap();
    assert!(out.content.contains("(60x80)"), "{}", out.content);
    let info = MediaInfo
        .call(json!({"path": "small.jpg"}), &ctx)
        .await
        .unwrap();
    assert!(info.content.contains("Jpeg: 60x80"), "{}", info.content);
}

#[tokio::test]
async fn voice_and_ocr_report_missing_engines_clearly() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    std::fs::write(dir.path().join("a.wav"), b"RIFF").unwrap();
    if !have("whisper-cli") && !have("whisper-cpp") {
        let e = jarvis_tools::voice::Transcribe
            .call(json!({"path": "a.wav"}), &ctx)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("whisper"), "{e}");
    }
    if cfg!(target_os = "linux") && !have("espeak-ng") && !have("piper") {
        let e = jarvis_tools::voice::Speak
            .call(json!({"text": "olá", "output": "o.wav"}), &ctx)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("espeak-ng"), "{e}");
    }
    if !have("tesseract") {
        let e = Ocr.call(json!({"path": "a.wav"}), &ctx).await.unwrap_err();
        assert!(e.to_string().contains("tesseract"), "{e}");
    }
}

#[tokio::test]
async fn mcp_stdio_server_tools() {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/mock_mcp.py");
    let client = Arc::new(
        jarvis_tools::mcp::McpClient::spawn(&McpServerConfig {
            name: "mock".into(),
            command: python().into(),
            args: vec![script.to_string_lossy().into()],
            env: BTreeMap::new(),
        })
        .await
        .unwrap(),
    );
    let mut reg = ToolRegistry::new();
    assert_eq!(
        jarvis_tools::mcp::register_tools(&client, &mut reg)
            .await
            .unwrap(),
        2
    );
    let add = reg.get("mcp__mock__add").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    assert_eq!(
        add.assess(&json!({}), &ctx).risk,
        jarvis_core::Risk::Execute
    );
    let out = add.call(json!({"a": 2, "b": 40}), &ctx).await.unwrap();
    assert_eq!(out.content, "42");
    assert_eq!(out.untrusted_source.as_deref(), Some("mcp:mock"));
}

#[tokio::test]
async fn custom_tools_do_not_use_a_shell() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::default();
    cfg.permissions.workspace_roots = vec![dir.path().to_string_lossy().into()];
    cfg.custom_tools = vec![CustomToolConfig {
        name: "eco".into(),
        description: "eco".into(),
        // Print argv[1] verbatim: proves the argument is not interpreted by a shell.
        command: vec![
            python().into(),
            "-c".into(),
            "import sys; print(sys.argv[1])".into(),
            "texto={text}".into(),
        ],
        params: [("text".to_string(), "texto".to_string())]
            .into_iter()
            .collect(),
        risk: "read".into(),
    }];
    let mut reg = ToolRegistry::new();
    let mut warnings = vec![];
    jarvis_tools::custom::register(&mut reg, &cfg, &mut warnings);
    let t = reg.get("eco").unwrap();
    let ctx = jarvis_tools::testutil::context_with(cfg);
    let out = t
        .call(json!({"text": "$(whoami); rm -rf /"}), &ctx)
        .await
        .unwrap();
    assert!(
        out.content.contains("texto=$(whoami); rm -rf /"),
        "{}",
        out.content
    );
}

#[tokio::test]
async fn skills_are_discovered_and_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let sk = dir.path().join("skills/relatorio");
    std::fs::create_dir_all(&sk).unwrap();
    std::fs::write(sk.join("SKILL.md"), "---\nname: relatorio-mensal\ndescription: Como montar o relatório mensal\n---\nPasso 1: ...\n").unwrap();
    let cfg = Config {
        skills_dirs: vec![dir.path().join("skills").to_string_lossy().into()],
        ..Default::default()
    };
    let skills = jarvis_tools::skills::discover(&cfg);
    assert_eq!(skills[0].name, "relatorio-mensal");
    assert!(jarvis_tools::skills::catalog(&skills).contains("relatório mensal"));
    let tool = jarvis_tools::skills::SkillLoad { skills };
    let out = tool
        .call(json!({"name": "relatorio-mensal"}), &context(dir.path()))
        .await
        .unwrap();
    assert!(out.content.contains("Passo 1"));
}

#[tokio::test]
async fn espeak_tts_and_tesseract_ocr_when_installed() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    if have("espeak-ng") {
        let out = jarvis_tools::voice::Speak
            .call(
                json!({"text": "Olá, eu sou o Jarvis.", "output": "ola.wav"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.content.contains("espeak-ng"), "{}", out.content);
        let wav = std::fs::read(dir.path().join("ola.wav")).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert!(wav.len() > 10_000);
    } else if cfg!(target_os = "macos") && have("say") {
        // Native macOS synthesis.
        let out = jarvis_tools::voice::Speak
            .call(
                json!({"text": "Olá, eu sou o Jarvis.", "output": "ola.wav"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.content.contains("say"), "{}", out.content);
        assert!(std::fs::metadata(dir.path().join("ola.wav")).unwrap().len() > 1000);
    }
    if have("tesseract") && have("pdftoppm") {
        // Render a PDF with known text, then OCR it.
        let pdf = jarvis_tools::pdfgen::render(None, "# FATURA 2026\nTotal a pagar: 1234 reais\n");
        std::fs::write(dir.path().join("scan.pdf"), pdf).unwrap();
        let out = Ocr
            .call(json!({"path": "scan.pdf", "lang": "por"}), &ctx)
            .await
            .unwrap();
        assert!(
            out.content.contains("FATURA") && out.content.contains("1234"),
            "{}",
            out.content
        );
    }
}
