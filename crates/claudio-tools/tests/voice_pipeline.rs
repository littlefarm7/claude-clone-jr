//! Voice pipeline: microphone stream → end-of-speech detection → recognition,
//! and one-click voice setup against a local mirror.

use std::io::Write;
use std::time::Duration;

use claudio_core::config::Config;
use claudio_tools::downloads::new_progress;
use claudio_tools::speech;
use tokio_util::sync::CancellationToken;

/// Real speech from espeak-ng as raw s16le 16 kHz mono, with silence around it.
fn spoken_raw(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let wav = dir.join("fala.wav");
    let ok = std::process::Command::new("espeak-ng")
        .args(["-v", "pt-br", "-w"])
        .arg(&wav)
        .arg("Olá Claudio, que horas são agora?")
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let speech = speech::read_wav(&wav).ok()?;
    let mut samples = vec![0.0f32; 16_000]; // 1 s of silence first
    samples.extend(speech);
    samples.extend(vec![0.0f32; 32_000]); // 2 s of silence after
    let raw = dir.join("mic.raw");
    let mut f = std::fs::File::create(&raw).ok()?;
    for s in samples {
        f.write_all(&((s * 32767.0) as i16).to_le_bytes()).ok()?;
    }
    Some(raw)
}

// The fake microphone is a shell command (sh); Windows uses the native microphone.
#[cfg(unix)]
#[tokio::test]
async fn listens_and_transcribes() {
    let dir = tempfile::tempdir().unwrap();
    // Silence only: nobody spoke, nothing is returned.
    let quiet = dir.path().join("silencio.raw");
    std::fs::write(&quiet, vec![0u8; 16_000 * 2 * 3]).unwrap();
    std::env::set_var(
        "CLAUDIO_MIC_CMD",
        format!("sh -c 'cat {} ; sleep 5'", quiet.display()),
    );
    let r = speech::listen_utterance(
        &CancellationToken::new(),
        Duration::from_millis(1500),
        |_| {},
    )
    .await
    .unwrap();
    assert!(r.is_none());

    let Some(raw) = spoken_raw(dir.path()) else {
        eprintln!("ignorado: espeak-ng ausente");
        return;
    };
    // The "microphone" plays the file at real speed through a pipe.
    std::env::set_var(
        "CLAUDIO_MIC_CMD",
        format!("sh -c 'cat {} ; sleep 3'", raw.display()),
    );
    let states = std::sync::Mutex::new(Vec::new());
    let utt = speech::listen_utterance(&CancellationToken::new(), Duration::from_secs(10), |s| {
        states.lock().unwrap().push(s)
    })
    .await
    .unwrap()
    .expect("speech detected");
    let secs = utt.len() as f32 / 16_000.0;
    assert!((1.0..5.0).contains(&secs), "utterance of {secs} s");
    assert_eq!(
        states.into_inner().unwrap(),
        vec![speech::ListenState::Waiting, speech::ListenState::Hearing]
    );

    // Recognition with whisper.cpp built in (real model when available,
    // otherwise the random-weight test model from scripts/make_tiny_whisper.py).
    let Ok(model) = std::env::var("CLAUDIO_TEST_WHISPER") else {
        eprintln!("reconhecimento ignorado: defina CLAUDIO_TEST_WHISPER");
        return;
    };
    let text = speech::transcribe_local(model.into(), utt, "pt-BR")
        .await
        .unwrap();
    eprintln!("transcrição: {text:?}");
    if std::env::var("CLAUDIO_TEST_WHISPER_REAL").is_ok() {
        assert!(text.to_lowercase().contains("claudio"), "{text}");
    }
}

#[tokio::test]
async fn one_click_voice_setup_from_mirror() {
    use axum::{routing::get, Router};
    // Mirror with the recognition model, a piper package and the voice.
    let exe = claudio_llm::platform::exe_name("piper");
    let script = b"#!/bin/sh\necho piper\n";
    // Same package format as the official piper release for this system.
    let mut pkg = Vec::new();
    if cfg!(windows) {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(&mut pkg));
        z.start_file(
            format!("piper/{exe}"),
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        z.write_all(script).unwrap();
        z.finish().unwrap();
    } else {
        let enc = flate2::write::GzEncoder::new(&mut pkg, flate2::Compression::fast());
        let mut t = tar::Builder::new(enc);
        let mut h = tar::Header::new_gnu();
        h.set_size(script.len() as u64);
        h.set_mode(0o755);
        t.append_data(&mut h, format!("piper/{exe}"), &script[..])
            .unwrap();
        t.into_inner().unwrap().finish().unwrap();
    }
    let app = Router::new()
        .route("/m/{file}", get(|| async { vec![1u8; 4096] }))
        .route("/r/{file}", get(move || async move { pkg }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

    let data = tempfile::tempdir().unwrap();
    std::env::set_var("CLAUDIO_DATA_DIR", data.path());
    std::env::set_var("CLAUDIO_MODELS_MIRROR", format!("{base}/m"));
    std::env::set_var("CLAUDIO_PIPER_RELEASE", format!("{base}/r"));

    let mut cfg = Config::default();
    let phases = std::sync::Mutex::new(Vec::new());
    speech::setup_voice(&mut cfg, &new_progress(), &CancellationToken::new(), |p| {
        phases.lock().unwrap().push(p.to_string())
    })
    .await
    .unwrap();
    let phases = phases.into_inner().unwrap();
    assert!(phases[0].contains("etapa 1 de 2") && phases[1].contains("etapa 2 de 2"));
    let model = cfg.voice.whisper_model.clone().unwrap();
    assert!(model.ends_with(".bin") && std::path::Path::new(&model).is_file());
    assert!(cfg.voice.piper_bin.as_deref().unwrap().contains("piper"));
    assert!(cfg
        .voice
        .piper_model
        .as_deref()
        .unwrap()
        .ends_with("pt_BR-faber-medium.onnx"));
    assert_eq!(cfg.voice.stt, "local");
    let st = speech::status(&cfg);
    assert!(st.piper);
    assert_eq!(st.local_stt, speech::cpu_supports_local_stt());
}
