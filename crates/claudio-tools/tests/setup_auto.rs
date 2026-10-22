//! Automatic "IA no meu computador" setup against a local HTTP server
//! (llama.cpp release + model mirror), as a non-technical user would run it.

use std::io::Write;

use axum::{routing::get, Json, Router};
use claudio_core::config::Config;
use claudio_tools::downloads::{model_path, new_progress, LlamaVariant};
use claudio_tools::setup::{ai_label, ai_ready, recommend_local, setup_local, Hardware};
use tokio_util::sync::CancellationToken;

fn make_zip() -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        let exe = claudio_llm::platform::exe_name("llama-server");
        z.start_file(format!("build/bin/{exe}"), o).unwrap();
        z.write_all(b"#!/bin/sh\necho llama\n").unwrap();
        z.finish().unwrap();
    }
    buf.into_inner()
}

async fn serve(model_file: &'static str) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    let b2 = base.clone();
    let os = if cfg!(windows) {
        "win-cpu"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "ubuntu"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    let release = serde_json::json!({
        "tag_name": "b7777",
        "assets": [{"name": format!("llama-b7777-bin-{os}-{arch}.zip"), "browser_download_url": format!("{b2}/llama.zip")}]
    });
    let zip = make_zip();
    let app = Router::new()
        .route("/release", get(move || async move { Json(release) }))
        .route("/llama.zip", get(move || async move { zip }))
        .route(
            &format!("/mirror/{model_file}"),
            get(|| async { vec![7u8; 200_000] }),
        );
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    base
}

#[tokio::test]
async fn one_click_local_setup() {
    let hw = Hardware {
        ram_mb: 4_000,
        gpu: None,
        vulkan: false,
        free_disk_mb: None,
    };
    let plan = recommend_local(&hw);
    assert_eq!(plan.model.id, "qwen2.5-1.5b");
    assert_eq!(plan.variant, LlamaVariant::Cpu);

    let base = serve(plan.model.file).await;
    let data = tempfile::tempdir().unwrap();
    std::env::set_var("CLAUDIO_DATA_DIR", data.path());
    std::env::set_var("CLAUDIO_LLAMA_RELEASES_API", format!("{base}/release"));
    std::env::set_var("CLAUDIO_MODELS_MIRROR", format!("{base}/mirror/"));
    // Make sure no llama-server from the machine is picked up.
    std::env::set_var("PATH", data.path());

    let mut cfg = Config::default();
    assert!(!ai_ready(&cfg));
    let phases = std::sync::Mutex::new(Vec::<String>::new());
    let p = new_progress();
    setup_local(&mut cfg, &plan, &p, &CancellationToken::new(), |s| {
        phases.lock().unwrap().push(s.to_string())
    })
    .await
    .unwrap();

    let phases = phases.into_inner().unwrap();
    assert_eq!(phases.len(), 2, "{phases:?}");
    assert!(phases[0].contains("etapa 1 de 2"));
    assert!(phases[1].contains("etapa 2 de 2"));
    let server = cfg.local.llama_server.clone().unwrap();
    assert!(server.contains("b7777"), "{server}");
    assert_eq!(cfg.default_model.as_deref(), Some("local"));
    assert!(cfg.local.autostart);
    assert_eq!(cfg.local.gpu_layers, 0);
    let gguf = model_path(plan.model);
    assert!(gguf.starts_with(data.path()));
    assert_eq!(std::fs::metadata(&gguf).unwrap().len(), 200_000);
    assert!(ai_ready(&cfg));
    assert_eq!(ai_label(&cfg, "local"), "No computador · Qwen2.5 1.5B");

    // Running it again reuses what is already installed (no new phases).
    let again = std::sync::Mutex::new(0);
    setup_local(
        &mut cfg,
        &plan,
        &new_progress(),
        &CancellationToken::new(),
        |_| *again.lock().unwrap() += 1,
    )
    .await
    .unwrap();
    assert_eq!(*again.lock().unwrap(), 0);
}
