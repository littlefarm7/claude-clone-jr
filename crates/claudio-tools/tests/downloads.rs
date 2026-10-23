//! Downloader and llama.cpp installer against a local HTTP server.

use std::io::Write;
use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use claudio_tools::downloads::*;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Files {
    model: Arc<Vec<u8>>,
    zip: Arc<Vec<u8>>,
    sha: String,
    base: Arc<std::sync::Mutex<String>>,
}

async fn model(State(f): State<Files>, headers: HeaderMap) -> impl IntoResponse {
    let data = f.model.clone();
    let mut h = HeaderMap::new();
    h.insert("x-linked-etag", format!("\"{}\"", f.sha).parse().unwrap());
    if let Some(r) = headers.get("range").and_then(|v| v.to_str().ok()) {
        let start: usize = r
            .trim_start_matches("bytes=")
            .trim_end_matches('-')
            .parse()
            .unwrap();
        return (StatusCode::PARTIAL_CONTENT, h, data[start..].to_vec()).into_response();
    }
    (StatusCode::OK, h, data.to_vec()).into_response()
}

async fn bad_sha(State(f): State<Files>) -> impl IntoResponse {
    let mut h = HeaderMap::new();
    h.insert(
        "x-linked-etag",
        format!("\"{}\"", "0".repeat(64)).parse().unwrap(),
    );
    (StatusCode::OK, h, f.model.to_vec())
}

async fn release(State(f): State<Files>) -> impl IntoResponse {
    let base = f.base.lock().unwrap().clone();
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
    // Like GitHub today: the newest entry is a source-only version and the
    // binary builds come after it (published as pre-releases).
    Json(serde_json::json!([
        {"tag_name": "b10000", "draft": true, "assets": [
            {"name": format!("llama-b10000-bin-{os}-{arch}.zip"), "browser_download_url": format!("{base}/nope")}
        ]},
        {"tag_name": "v0.7.0", "assets": []},
        {"tag_name": "b9999", "prerelease": true, "assets": [
            {"name": "llama-b9999-bin-ubuntu-vulkan-x64.zip", "browser_download_url": format!("{base}/nope")},
            {"name": format!("llama-b9999-bin-{os}-{arch}.zip"), "browser_download_url": format!("{base}/llama.zip")}
        ]}
    ]))
}

async fn llama_zip(State(f): State<Files>) -> impl IntoResponse {
    f.zip.to_vec()
}

fn make_zip() -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        let exe = claudio_llm::platform::exe_name("llama-server");
        z.start_file(format!("build/bin/{exe}"), o).unwrap();
        z.write_all(b"#!/bin/sh\necho llama\n").unwrap();
        z.start_file("build/bin/libggml.so", o).unwrap();
        z.write_all(b"lib").unwrap();
        z.finish().unwrap();
    }
    buf.into_inner()
}

async fn serve() -> String {
    let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let sha = hex::encode(Sha256::digest(&bytes));
    let files = Files {
        model: Arc::new(bytes),
        zip: Arc::new(make_zip()),
        sha,
        base: Arc::default(),
    };
    let app = Router::new()
        .route("/m.gguf", get(model))
        .route("/bad.gguf", get(bad_sha))
        .route("/release", get(release))
        .route("/llama.zip", get(llama_zip))
        .with_state(files.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    *files.base.lock().unwrap() = base.clone();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    base
}

#[tokio::test]
async fn download_verifies_and_resumes() {
    let base = serve().await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("m.gguf");
    let p = new_progress();
    download(
        &format!("{base}/m.gguf"),
        &dest,
        &p,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::metadata(&dest).unwrap().len(), 300_000);
    assert_eq!(p.lock().unwrap().fraction(), Some(1.0));

    // Resume: a partial file with the first 100 kB must be continued, not restarted.
    let dest2 = dir.path().join("m2.gguf");
    let full = std::fs::read(&dest).unwrap();
    std::fs::write(dir.path().join("m2.gguf.part"), &full[..100_000]).unwrap();
    let p = new_progress();
    download(
        &format!("{base}/m.gguf"),
        &dest2,
        &p,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&dest2).unwrap(), full);

    // Wrong checksum is rejected and nothing is left behind.
    let bad = dir.path().join("bad.gguf");
    let e = download(
        &format!("{base}/bad.gguf"),
        &bad,
        &new_progress(),
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("SHA-256"), "{e}");
    assert!(!bad.exists());

    // Cancelled before starting.
    let c = CancellationToken::new();
    c.cancel();
    assert!(download(
        &format!("{base}/m.gguf"),
        &dir.path().join("c.gguf"),
        &new_progress(),
        &c
    )
    .await
    .is_err());

    // Plain http to remote hosts is refused.
    assert!(download(
        "http://example.com/x.gguf",
        &dir.path().join("x.gguf"),
        &new_progress(),
        &CancellationToken::new()
    )
    .await
    .is_err());
}

#[tokio::test]
async fn installs_llama_cpp_from_release() {
    let base = serve().await;
    let data = tempfile::tempdir().unwrap();
    std::env::set_var("CLAUDIO_DATA_DIR", data.path());
    let p = new_progress();
    let server = install_llama_cpp(
        &format!("{base}/release"),
        LlamaVariant::Cpu,
        &p,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(server.ends_with(claudio_llm::platform::exe_name("llama-server")));
    assert!(server.starts_with(data.path().join("llama.cpp").join("b9999")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&server).unwrap().permissions().mode() & 0o111,
            0o111
        );
    }
    // the downloaded archive is removed after extraction
    let leftovers: Vec<_> = std::fs::read_dir(data.path().join("llama.cpp"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".zip"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn use_local_model_sets_default() {
    let mut cfg = claudio_core::config::Config::default();
    let m = claudio_llm::catalog::local_model("qwen2.5-7b").unwrap();
    use_local_model(&mut cfg, m.model_config("/m/q.gguf"));
    assert_eq!(cfg.default_model.as_deref(), Some("local"));
    let local = cfg.model("local").unwrap();
    assert_eq!(local.gguf_path.as_deref(), Some("/m/q.gguf"));
    assert_eq!(local.context_window, 16384);
    assert_eq!(cfg.models.iter().filter(|m| m.name == "local").count(), 1);
}
