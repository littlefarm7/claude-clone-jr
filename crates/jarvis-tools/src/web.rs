//! Web search, page fetching (with SSRF protection) and headless browser.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use jarvis_core::config::Config;
use jarvis_core::permissions::{Risk, RiskAssessment};
use jarvis_core::tool::{arg_str, arg_u64, Tool, ToolContext, ToolOutput, ToolRegistry};
use jarvis_llm::ToolSpec;
use serde_json::{json, Value};

pub fn register(r: &mut ToolRegistry) {
    r.register(Arc::new(WebSearch));
    r.register(Arc::new(WebFetch));
    r.register(Arc::new(Browser));
}

fn client(timeout: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) jarvis-core/0.1")
        .build()
        .expect("http client")
}

pub fn is_private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            v.is_loopback() || v.is_private() || v.is_link_local() || v.is_unspecified() || v.is_broadcast()
                || v.octets()[0] == 100 && (64..128).contains(&v.octets()[1]) // CGNAT
                || v.octets()[0] == 0
        }
        IpAddr::V6(v) => {
            v.is_loopback()
                || v.is_unspecified()
                || (v.segments()[0] & 0xfe00) == 0xfc00
                || (v.segments()[0] & 0xffc0) == 0xfe80
                || v.to_ipv4_mapped()
                    .map(|m| is_private(&IpAddr::V4(m)))
                    .unwrap_or(false)
        }
    }
}

/// Reject non-http(s) URLs and, unless allowed, hosts that resolve to private addresses.
pub async fn check_url(url: &str, cfg: &Config) -> Result<url::Url> {
    let u = url::Url::parse(url).with_context(|| format!("URL inválida: {url}"))?;
    if !matches!(u.scheme(), "http" | "https") {
        bail!("apenas http/https são permitidos");
    }
    if !u.username().is_empty() || u.password().is_some() {
        bail!("URLs com credenciais não são permitidas");
    }
    if cfg.web.allow_private_hosts {
        return Ok(u);
    }
    let host = u.host_str().context("URL sem host")?;
    let port = u.port_or_known_default().unwrap_or(80);
    let addrs: Vec<_> = tokio::net::lookup_host((host.trim_matches(['[', ']']), port))
        .await
        .with_context(|| format!("não foi possível resolver {host}"))?
        .collect();
    if addrs.is_empty() || addrs.iter().any(|a| is_private(&a.ip())) {
        bail!(
            "{host} aponta para endereço local/privado; bloqueado (web.allow_private_hosts=false)"
        );
    }
    Ok(u)
}

/// GET with manual redirects so each hop is re-checked.
pub async fn fetch(url: &str, cfg: &Config, max_bytes: usize) -> Result<(String, String, Vec<u8>)> {
    let c = client(30);
    let mut current = check_url(url, cfg).await?;
    for _ in 0..6 {
        let resp = c
            .get(current.clone())
            .send()
            .await
            .with_context(|| format!("falha ao buscar {current}"))?;
        if resp.status().is_redirection() {
            let loc = resp
                .headers()
                .get("location")
                .and_then(|l| l.to_str().ok())
                .context("redirecionamento sem Location")?;
            let next = current.join(loc)?;
            current = check_url(next.as_str(), cfg).await?;
            continue;
        }
        let status = resp.status();
        let ctype = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let mut body = Vec::new();
        let mut resp = resp;
        while let Some(chunk) = resp.chunk().await? {
            body.extend_from_slice(&chunk);
            if body.len() > max_bytes {
                body.truncate(max_bytes);
                break;
            }
        }
        if !status.is_success() {
            bail!("HTTP {status} em {current}");
        }
        return Ok((current.to_string(), ctype, body));
    }
    bail!("redirecionamentos demais")
}

pub fn html_to_text(html: &[u8], width: usize) -> String {
    let text = html2text::from_read(html, width);
    // Collapse runs of blank lines.
    let mut out = String::new();
    let mut blank = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

fn decode_entities(s: &str) -> String {
    html2text::from_read(s.as_bytes(), 10_000)
        .trim()
        .to_string()
}

/// Parse DuckDuckGo's HTML results page.
pub fn parse_ddg(html: &str) -> Vec<SearchResult> {
    let a_re =
        regex::Regex::new(r#"(?s)<a[^>]*class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#)
            .unwrap();
    let s_re = regex::Regex::new(r#"(?s)class="result__snippet"[^>]*>(.*?)</a>"#).unwrap();
    let tag = regex::Regex::new(r"<[^>]+>").unwrap();
    let snippets: Vec<String> = s_re
        .captures_iter(html)
        .map(|c| decode_entities(&tag.replace_all(&c[1], "")))
        .collect();
    a_re.captures_iter(html)
        .enumerate()
        .map(|(i, c)| {
            let mut href = c[1].replace("&amp;", "&");
            if let Some(pos) = href.find("uddg=") {
                let enc = href[pos + 5..].split('&').next().unwrap_or("");
                href = url::form_urlencoded::parse(format!("x={enc}").as_bytes())
                    .next()
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or(href);
            }
            if href.starts_with("//") {
                href = format!("https:{href}");
            }
            SearchResult {
                title: decode_entities(&tag.replace_all(&c[2], "")),
                url: href,
                snippet: snippets.get(i).cloned().unwrap_or_default(),
            }
        })
        .collect()
}

pub async fn search(query: &str, n: usize, cfg: &Config) -> Result<(String, Vec<SearchResult>)> {
    let c = client(20);
    if let Some(base) = &cfg.web.searxng_url {
        let url = format!("{}/search", base.trim_end_matches('/'));
        let v: Value = c
            .get(&url)
            .query(&[("q", query), ("format", "json")])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let results = v["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .take(n)
            .map(|r| SearchResult {
                title: r["title"].as_str().unwrap_or("").into(),
                url: r["url"].as_str().unwrap_or("").into(),
                snippet: r["content"].as_str().unwrap_or("").into(),
            })
            .collect();
        return Ok(("searxng".into(), results));
    }
    if let Ok(key) = std::env::var("BRAVE_API_KEY") {
        let v: Value = c
            .get("https://api.search.brave.com/res/v1/web/search")
            .query(&[("q", query), ("count", &n.to_string())])
            .header("X-Subscription-Token", key)
            .header("Accept", "application/json")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let results = v["web"]["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .take(n)
            .map(|r| SearchResult {
                title: r["title"].as_str().unwrap_or("").into(),
                url: r["url"].as_str().unwrap_or("").into(),
                snippet: decode_entities(r["description"].as_str().unwrap_or("")),
            })
            .collect();
        return Ok(("brave".into(), results));
    }
    let html = c
        .post("https://html.duckduckgo.com/html/")
        .form(&[("q", query)])
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let mut r = parse_ddg(&html);
    r.truncate(n);
    Ok(("duckduckgo".into(), r))
}

pub struct WebSearch;

#[async_trait]
impl Tool for WebSearch {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_search".into(),
            description: "Pesquisa na web (SearXNG local se configurado, Brave se houver chave, senão DuckDuckGo). Retorna títulos, URLs e trechos.".into(),
            parameters: json!({"type":"object","properties":{"query":{"type":"string"},"max_results":{"type":"integer"}},"required":["query"]}),
        }
    }
    fn assess(&self, args: &Value, _: &ToolContext) -> RiskAssessment {
        RiskAssessment::new(
            Risk::Network,
            format!("pesquisar na web: {}", arg_str(args, "query").unwrap_or("")),
        )
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let q = arg_str(&args, "query").unwrap_or("");
        let n = arg_u64(&args, "max_results").unwrap_or(8).min(20) as usize;
        let (engine, results) = search(q, n, &ctx.cfg).await?;
        if results.is_empty() {
            return Ok(ToolOutput::ok(format!("Nenhum resultado ({engine})."))
                .untrusted(format!("search:{engine}")));
        }
        let text = results
            .iter()
            .enumerate()
            .map(|(i, r)| format!("{}. {}\n   {}\n   {}", i + 1, r.title, r.url, r.snippet))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput::ok(format!("Resultados ({engine}):\n{text}"))
            .untrusted(format!("search:{engine}")))
    }
}

pub struct WebFetch;

#[async_trait]
impl Tool for WebFetch {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_fetch".into(),
            description: "Baixa uma página (GET) e devolve o texto. PDFs são convertidos em texto. save_to grava o arquivo bruto no workspace.".into(),
            parameters: json!({"type":"object","properties":{
                "url":{"type":"string"},"max_chars":{"type":"integer"},"save_to":{"type":"string"}
            },"required":["url"]}),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let url = arg_str(args, "url").unwrap_or("");
        match arg_str(args, "save_to") {
            Some(p) => {
                let p = ctx.guard.resolve(p);
                let ra = ctx.guard.assess_write(
                    &p,
                    Risk::Write,
                    format!("baixar {url} → {}", p.display()),
                );
                if ra.force.is_some() {
                    ra
                } else {
                    RiskAssessment::new(Risk::Write, format!("baixar {url} → {}", p.display()))
                }
            }
            None => RiskAssessment::new(Risk::Network, format!("abrir {url}")),
        }
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let url = arg_str(&args, "url").unwrap_or("");
        let (final_url, ctype, body) = fetch(url, &ctx.cfg, ctx.cfg.web.max_fetch_bytes).await?;
        let src = format!("web:{final_url}");
        if let Some(p) = arg_str(&args, "save_to") {
            let p = ctx.guard.resolve(p);
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(&p, &body)?;
            return Ok(ToolOutput::ok(format!(
                "Salvo {} ({} bytes, {ctype}) de {final_url}",
                p.display(),
                body.len()
            ))
            .with_artifact(p));
        }
        let max = arg_u64(&args, "max_chars").unwrap_or(20_000) as usize;
        let text =
            if ctype.contains("html") || body.starts_with(b"<!") || body.starts_with(b"<html") {
                html_to_text(&body, 120)
            } else if ctype.contains("pdf") || body.starts_with(b"%PDF") {
                let tmp = tempfile::Builder::new().suffix(".pdf").tempfile()?;
                std::fs::write(tmp.path(), &body)?;
                crate::docs::pdf_text(tmp.path()).await.unwrap_or_else(|e| {
                    format!("(PDF de {} bytes; extração falhou: {e})", body.len())
                })
            } else if ctype.starts_with("text/")
                || ctype.contains("json")
                || ctype.contains("xml")
                || std::str::from_utf8(&body).is_ok()
            {
                String::from_utf8_lossy(&body).into_owned()
            } else {
                return Ok(ToolOutput::ok(format!(
                    "Conteúdo binário ({ctype}, {} bytes). Use save_to para gravar.",
                    body.len()
                ))
                .untrusted(src));
            };
        let text = jarvis_core::safety::truncate_middle(&text, max);
        Ok(ToolOutput::ok(format!("URL: {final_url}\nTipo: {ctype}\n\n{text}")).untrusted(src))
    }
}

// ---------------------------------------------------------------------------

pub fn find_chromium(cfg: &Config) -> Option<PathBuf> {
    if let Some(p) = &cfg.web.chromium {
        return Some(jarvis_core::config::expand(p));
    }
    // Linux names, then the macOS app bundles / Windows installs found by `find_program`.
    let names = [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "brave-browser",
        "Google Chrome",
        "Chromium",
        "Microsoft Edge",
        "chrome",
        "msedge",
    ];
    if let Some(p) = jarvis_llm::platform::find_program(&names) {
        return Some(p);
    }
    // Playwright-managed browsers.
    let base =
        std::env::var("PLAYWRIGHT_BROWSERS_PATH").unwrap_or_else(|_| "/opt/pw-browsers".into());
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&base)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with("chromium-"))
                .unwrap_or(false)
        })
        .collect();
    dirs.sort();
    dirs.into_iter()
        .rev()
        .map(|d| d.join("chrome-linux/chrome"))
        .find(|p| p.is_file())
}

pub struct Browser;

#[async_trait]
impl Tool for Browser {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser".into(),
            description: "Abre uma página no Chromium headless (executa JavaScript). action: text (texto renderizado), \
                screenshot (PNG) ou pdf. Útil para páginas dinâmicas."
                .into(),
            parameters: json!({"type":"object","properties":{
                "url":{"type":"string"},
                "action":{"type":"string","enum":["text","screenshot","pdf"]},
                "output":{"type":"string","description":"arquivo de saída para screenshot/pdf"},
                "width":{"type":"integer"},"height":{"type":"integer"}
            },"required":["url"]}),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let url = arg_str(args, "url").unwrap_or("");
        let action = arg_str(args, "action").unwrap_or("text");
        if action == "text" {
            return RiskAssessment::new(Risk::Network, format!("abrir no navegador: {url}"));
        }
        let out = ctx
            .guard
            .resolve(arg_str(args, "output").unwrap_or(if action == "pdf" {
                "pagina.pdf"
            } else {
                "captura.png"
            }));
        ctx.guard.assess_write(
            &out,
            Risk::Write,
            format!("{action} de {url} → {}", out.display()),
        )
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let url = check_url(arg_str(&args, "url").unwrap_or(""), &ctx.cfg).await?;
        let chrome = find_chromium(&ctx.cfg).context(
            "Chromium não encontrado (sudo apt install chromium, ou web.chromium na config)",
        )?;
        let action = arg_str(&args, "action").unwrap_or("text");
        let profile = tempfile::tempdir()?;
        let w = arg_u64(&args, "width").unwrap_or(1280);
        let h = arg_u64(&args, "height").unwrap_or(900);
        let mut a: Vec<String> = vec![
            "--headless=new".into(),
            "--disable-gpu".into(),
            "--no-first-run".into(),
            "--disable-extensions".into(),
            "--disable-dev-shm-usage".into(),
            format!("--user-data-dir={}", profile.path().display()),
            format!("--window-size={w},{h}"),
            "--virtual-time-budget=5000".into(),
        ];
        #[cfg(unix)]
        if unsafe { libc::geteuid() } == 0 {
            a.push("--no-sandbox".into()); // Chromium refuses to run as root otherwise.
        }
        let out_path = match action {
            "screenshot" => Some(
                ctx.guard
                    .resolve(arg_str(&args, "output").unwrap_or("captura.png")),
            ),
            "pdf" => Some(
                ctx.guard
                    .resolve(arg_str(&args, "output").unwrap_or("pagina.pdf")),
            ),
            _ => None,
        };
        match (action, &out_path) {
            ("screenshot", Some(p)) => a.push(format!("--screenshot={}", p.display())),
            ("pdf", Some(p)) => a.push(format!("--print-to-pdf={}", p.display())),
            _ => a.push("--dump-dom".into()),
        }
        if let Some(p) = &out_path {
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d)?;
            }
        }
        a.push(url.to_string());
        let cmd = crate::shell::build_command(
            &chrome.to_string_lossy(),
            &a,
            &ctx.guard.cwd(),
            ctx,
            false,
        );
        let mut res = crate::shell::run_command(cmd, Duration::from_secs(60), ctx).await?;
        if !res.ok() && res.stderr.contains("No usable sandbox") {
            // Some distros (e.g. Ubuntu 24.04 AppArmor) forbid Chromium's namespace sandbox.
            if !ctx.cfg.web.browser_no_sandbox_fallback {
                bail!(
                    "o Chromium não conseguiu criar seu sandbox neste sistema. Defina \
                     web.browser_no_sandbox_fallback = true para permitir rodar sem ele (menos seguro)"
                );
            }
            a.insert(0, "--no-sandbox".into());
            let cmd = crate::shell::build_command(
                &chrome.to_string_lossy(),
                &a,
                &ctx.guard.cwd(),
                ctx,
                false,
            );
            res = crate::shell::run_command(cmd, Duration::from_secs(60), ctx).await?;
        }
        if let Some(p) = out_path {
            if !p.exists() {
                bail!(
                    "o navegador não gerou {}: {}",
                    p.display(),
                    res.render(2000)
                );
            }
            let size = std::fs::metadata(&p)?.len();
            return Ok(ToolOutput::ok(format!(
                "Gerado {} ({} bytes) a partir de {url}",
                p.display(),
                size
            ))
            .with_artifact(p));
        }
        if res.stdout.trim().is_empty() {
            bail!("navegador sem saída: {}", res.render(2000));
        }
        let text = html_to_text(res.stdout.as_bytes(), 120);
        Ok(ToolOutput::ok(format!(
            "URL: {url}\n\n{}",
            jarvis_core::safety::truncate_middle(&text, 20_000)
        ))
        .untrusted(format!("browser:{url}")))
    }
}
