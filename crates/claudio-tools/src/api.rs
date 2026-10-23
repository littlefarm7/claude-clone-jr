//! `http_request`: call web APIs and external services (GET/POST/PUT/PATCH/DELETE).
//!
//! Keys are never shown to the model: headers and bodies may contain
//! `{{secret:NAME}}`, replaced at call time with the value stored by the user
//! (`claudio secret set NAME`, or the environment variable NAME). Every call
//! that uses a secret or sends data needs the user's approval, and secret
//! values are removed from the response before the model sees it.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use claudio_core::permissions::{Risk, RiskAssessment};
use claudio_core::secrets::{secrets_path_for, Secrets};
use claudio_core::tool::{arg_str, Tool, ToolContext, ToolOutput, ToolRegistry};
use claudio_llm::ToolSpec;
use serde_json::{json, Value};

use crate::web::check_url;

pub fn register(r: &mut ToolRegistry) {
    r.register(Arc::new(HttpRequest));
}

/// Prefix of API secrets inside the key store (model keys use the model name).
pub const SECRET_PREFIX: &str = "api:";

fn secret_names(text: &str) -> Vec<String> {
    let re = regex::Regex::new(r"\{\{secret:([A-Za-z0-9_.-]+)\}\}").unwrap();
    let mut v: Vec<String> = re.captures_iter(text).map(|c| c[1].to_string()).collect();
    v.sort();
    v.dedup();
    v
}

fn lookup_secret(name: &str, store: &Secrets) -> Option<String> {
    store
        .keys
        .get(&format!("{SECRET_PREFIX}{name}"))
        .cloned()
        .or_else(|| std::env::var(name).ok())
        .filter(|v| !v.is_empty())
}

/// Replace `{{secret:NAME}}` placeholders. Returns the text and the values used.
pub fn fill_secrets(text: &str, store: &Secrets) -> Result<(String, Vec<String>)> {
    let mut out = text.to_string();
    let mut used = Vec::new();
    for name in secret_names(text) {
        let value = lookup_secret(&name, store).with_context(|| {
            format!("segredo '{name}' não configurado (peça ao usuário para rodar: claudio secret set {name})")
        })?;
        out = out.replace(&format!("{{{{secret:{name}}}}}"), &value);
        used.push(value);
    }
    Ok((out, used))
}

/// Remove secret values from text shown to the model.
pub fn redact_values(text: &str, values: &[String]) -> String {
    let mut s = text.to_string();
    for v in values.iter().filter(|v| v.len() >= 4) {
        s = s.replace(v.as_str(), "[segredo]");
    }
    s
}

pub struct HttpRequest;

#[async_trait]
impl Tool for HttpRequest {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "http_request".into(),
            description: "Chama uma API ou serviço web (GET, POST, PUT, PATCH, DELETE) e devolve status, cabeçalhos \
                principais e corpo. Para chaves use {{secret:NOME}} nos cabeçalhos ou no corpo (o valor nunca aparece \
                para você). Envios de dados pedem aprovação do usuário."
                .into(),
            parameters: json!({"type":"object","properties":{
                "method":{"type":"string","enum":["GET","POST","PUT","PATCH","DELETE","HEAD"]},
                "url":{"type":"string"},
                "headers":{"type":"object","additionalProperties":{"type":"string"}},
                "body":{"description":"texto, ou objeto JSON (enviado como application/json)"},
                "max_chars":{"type":"integer"}
            },"required":["url"]}),
        }
    }

    fn assess(&self, args: &Value, _: &ToolContext) -> RiskAssessment {
        let method = arg_str(args, "method").unwrap_or("GET").to_uppercase();
        let url = arg_str(args, "url").unwrap_or("");
        let host = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(String::from))
            .unwrap_or_else(|| url.to_string());
        let all = format!(
            "{} {}",
            args.get("headers")
                .map(|h| h.to_string())
                .unwrap_or_default(),
            args.get("body").map(|b| b.to_string()).unwrap_or_default()
        );
        let secrets = secret_names(&all);
        let mut ra = RiskAssessment::new(Risk::Network, format!("{method} {url}"));
        if !secrets.is_empty() {
            // Always ask: an injected instruction could try to send a key elsewhere.
            ra = RiskAssessment::new(
                Risk::Network,
                format!(
                    "{method} {url} usando a(s) chave(s) {} (enviada(s) para {host})",
                    secrets.join(", ")
                ),
            )
            .ask(format!("usar chave(s) guardada(s) com {host}"));
        } else if !matches!(method.as_str(), "GET" | "HEAD") {
            ra = ra.ask(format!("enviar dados para {host}"));
        }
        ra
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let method = arg_str(&args, "method").unwrap_or("GET").to_uppercase();
        let url = arg_str(&args, "url").context("url")?;
        let store = Secrets::load_from(&secrets_path_for(&ctx.cfg)).unwrap_or_default();
        let (url, mut used) = fill_secrets(url, &store)?;
        let target = check_url(&url, &ctx.cfg).await?;
        let method = reqwest::Method::from_bytes(method.as_bytes()).context("método inválido")?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("claudio-code/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let mut req = client.request(method.clone(), target.clone());
        if let Some(h) = args.get("headers").and_then(|h| h.as_object()) {
            for (k, v) in h {
                let (v, u) = fill_secrets(v.as_str().unwrap_or(&v.to_string()), &store)?;
                used.extend(u);
                req = req.header(k.as_str(), v);
            }
        }
        match args.get("body") {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => {
                let (s, u) = fill_secrets(s, &store)?;
                used.extend(u);
                req = req.body(s);
            }
            Some(v) => {
                let (s, u) = fill_secrets(&v.to_string(), &store)?;
                used.extend(u);
                req = req.header("content-type", "application/json").body(s);
            }
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("falha ao chamar {target}"))?;
        let status = resp.status();
        let mut head = String::new();
        for k in [
            "content-type",
            "location",
            "retry-after",
            "x-ratelimit-remaining",
        ] {
            if let Some(v) = resp.headers().get(k).and_then(|v| v.to_str().ok()) {
                head.push_str(&format!("{k}: {v}\n"));
            }
        }
        let max = args
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(20_000)
            .min(200_000) as usize;
        let mut body = Vec::new();
        let mut resp = resp;
        while let Some(chunk) = resp.chunk().await? {
            body.extend_from_slice(&chunk);
            if body.len() > ctx.cfg.web.max_fetch_bytes {
                break;
            }
        }
        let mut text = String::from_utf8_lossy(&body).into_owned();
        if text.chars().count() > max {
            text = text.chars().take(max).collect::<String>() + "\n[… resposta cortada]";
        }
        let content = redact_values(&format!("HTTP {status}\n{head}\n{text}"), &used);
        let mut out =
            ToolOutput::ok(content).untrusted(format!("api:{}", target.host_str().unwrap_or("")));
        out.is_error = !status.is_success();
        if status.is_redirection() {
            out.is_error = false;
        }
        Ok(out)
    }
}

/// Validate a secret name for the CLI.
pub fn check_secret_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
    {
        bail!("nome inválido: use letras, números, _ . -");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_filled_and_redacted() {
        let mut s = Secrets::default();
        s.keys.insert("api:GITHUB".into(), "ghp_abc123".into());
        let (t, used) = fill_secrets("Bearer {{secret:GITHUB}}", &s).unwrap();
        assert_eq!(t, "Bearer ghp_abc123");
        assert_eq!(redact_values("eco: ghp_abc123", &used), "eco: [segredo]");
        assert!(fill_secrets("{{secret:NOPE_XYZ_123}}", &s).is_err());
        assert_eq!(
            secret_names("{{secret:A}} {{secret:B}} {{secret:A}}"),
            vec!["A", "B"]
        );
    }
}
