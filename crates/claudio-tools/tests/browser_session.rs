//! Interactive browser against a local site (real Chromium, headless).

use axum::{extract::Query, response::Html, routing::get, Router};
use claudio_core::config::Config;
use claudio_core::tool::Tool;
use claudio_tools::browse::BrowserSession;
use claudio_tools::testutil;
use serde_json::json;
use std::collections::HashMap;

async fn home() -> Html<&'static str> {
    Html(
        r#"<html><head><title>Loja Teste</title></head><body>
        <h1>Bem-vindo</h1>
        <form action="/buscar"><input name="q" placeholder="Buscar produto"><button>Buscar</button></form>
        <input type="password" name="senha" placeholder="senha">
        <select id="cor"><option value="a">Azul</option><option value="v">Verde</option></select>
        <a href="/sobre">Sobre nós</a>
        </body></html>"#,
    )
}

async fn buscar(Query(q): Query<HashMap<String, String>>) -> Html<String> {
    Html(format!(
        "<html><head><title>Resultados</title></head><body><p>Você buscou: {}</p></body></html>",
        q.get("q").cloned().unwrap_or_default()
    ))
}

async fn sobre() -> Html<&'static str> {
    Html("<html><head><title>Sobre</title></head><body><p>Somos uma loja de teste.</p></body></html>")
}

#[tokio::test]
async fn navigates_types_and_clicks() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = Config::default();
    if claudio_tools::web::find_chromium(&cfg).is_none() {
        eprintln!("ignorado: Chromium não encontrado");
        return;
    }
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    tokio::spawn(async move {
        let app = Router::new()
            .route("/", get(home))
            .route("/buscar", get(buscar))
            .route("/sobre", get(sobre));
        axum::serve(l, app).await.unwrap()
    });
    std::env::set_var("CLAUDIO_DATA_DIR", tmp.path().join("data"));
    cfg.web.allow_private_hosts = true;
    cfg.web.browser_no_sandbox_fallback = true;
    cfg.permissions.workspace_roots = vec![tmp.path().join("ws").to_string_lossy().into()];
    let ctx = testutil::context_with(cfg);
    let b = BrowserSession;

    let out = b
        .call(
            json!({"action":"open","url": format!("{base}/"),"visible": false}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(
        out.content.contains("Título: Loja Teste"),
        "{}",
        out.content
    );
    assert!(
        out.content.contains(r#"input:text "Buscar produto""#),
        "{}",
        out.content
    );
    assert!(out.content.contains("(campo de senha)"), "{}", out.content);
    assert!(
        out.untrusted_source.is_some(),
        "pages are untrusted content"
    );
    let id_of = |text: &str, needle: &str| -> u64 {
        let line = text.lines().find(|l| l.contains(needle)).unwrap();
        line[1..line.find(']').unwrap()].parse().unwrap()
    };
    let search = id_of(&out.content, "Buscar produto");
    let pw = id_of(&out.content, "campo de senha");
    let cor = id_of(&out.content, "select");

    // Password fields are off limits.
    let e = b
        .call(json!({"action":"type","id": pw,"text":"x"}), &ctx)
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("campo de senha"));

    let out = b
        .call(json!({"action":"select","id": cor,"value":"Verde"}), &ctx)
        .await
        .unwrap();
    assert!(out.content.contains("Loja Teste"));

    let out = b
        .call(
            json!({"action":"type","id": search,"text":"cadeira azul","submit": true}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(
        out.content.contains("Você buscou: cadeira azul"),
        "{}",
        out.content
    );

    let out = b.call(json!({"action":"back"}), &ctx).await.unwrap();
    assert!(out.content.contains("Loja Teste"), "{}", out.content);
    let link = id_of(&out.content, "Sobre nós");
    let out = b
        .call(json!({"action":"click","id": link}), &ctx)
        .await
        .unwrap();
    assert!(
        out.content.contains("Somos uma loja de teste"),
        "{}",
        out.content
    );

    let out = b
        .call(json!({"action":"screenshot","output":"pagina.png"}), &ctx)
        .await
        .unwrap();
    let png = std::fs::read(tmp.path().join("ws/pagina.png")).unwrap();
    assert!(png.starts_with(b"\x89PNG"), "{}", out.content);

    b.call(json!({"action":"close"}), &ctx).await.unwrap();
}
