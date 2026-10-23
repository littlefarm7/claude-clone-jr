//! `http_request` against a local API: methods, JSON bodies, secrets.

use axum::{http::HeaderMap, routing::post, Json, Router};
use claudio_core::config::Config;
use claudio_core::permissions::Forced;
use claudio_core::secrets::{secrets_path_for, Secrets};
use claudio_core::tool::Tool;
use claudio_tools::api::HttpRequest;
use claudio_tools::testutil;
use serde_json::{json, Value};

async fn echo(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
    Json(json!({
        "auth": headers.get("authorization").and_then(|v| v.to_str().ok()),
        "got": body,
    }))
}

#[tokio::test]
async fn calls_api_with_secret_and_hides_it() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    tokio::spawn(async move {
        axum::serve(l, Router::new().route("/v1/items", post(echo)))
            .await
            .unwrap()
    });

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::default();
    cfg.web.allow_private_hosts = true; // local test server
    cfg.source = Some(dir.path().join("config.toml"));
    let mut store = Secrets::default();
    store.set("api:MEU_SERVICO", "tok_super_secreto_42");
    store.save_to(&secrets_path_for(&cfg)).unwrap();
    let ctx = testutil::context_with(cfg);

    let args = json!({
        "method": "POST",
        "url": format!("{base}/v1/items"),
        "headers": {"Authorization": "Bearer {{secret:MEU_SERVICO}}"},
        "body": {"nome": "teste"}
    });
    // Using a stored key always needs the user's approval and names the host.
    let ra = HttpRequest.assess(&args, &ctx);
    assert!(matches!(ra.force, Some(Forced::Ask(_))), "{ra:?}");
    assert!(ra.summary.contains("MEU_SERVICO") && ra.summary.contains("127.0.0.1"));

    let out = HttpRequest.call(args, &ctx).await.unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("HTTP 200"), "{}", out.content);
    assert!(out.content.contains(r#""nome":"teste""#), "{}", out.content);
    // The server received the real key; the model only sees a placeholder.
    assert!(out.content.contains("Bearer [segredo]"), "{}", out.content);
    assert!(!out.content.contains("tok_super_secreto_42"));

    // Unknown secret: clear error, nothing sent.
    let bad =
        json!({"url": format!("{base}/v1/items"), "headers": {"X": "{{secret:NAO_EXISTE_987}}"}});
    let e = HttpRequest.call(bad, &ctx).await.unwrap_err();
    assert!(format!("{e:#}").contains("claudio secret set NAO_EXISTE_987"));

    // Private hosts stay blocked by default.
    let ctx2 = testutil::context_with(Config::default());
    let blocked = HttpRequest
        .call(json!({"url": format!("{base}/v1/items")}), &ctx2)
        .await
        .unwrap_err();
    assert!(format!("{blocked:#}").contains("privado"), "{blocked:#}");
}
