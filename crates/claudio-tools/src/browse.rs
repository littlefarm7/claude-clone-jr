//! Interactive browser (Chromium/Chrome/Edge via the DevTools protocol).
//!
//! One browser per Claudio process, with its own profile in the data folder
//! (logins persist). By default the window is visible so the user can follow
//! along and log in by hand; without a display it runs headless.
//! Pages are untrusted content: after reading one, clicks and typing need the
//! user's approval (taint rule), and typing into password fields always does.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use claudio_core::config::data_dir;
use claudio_core::permissions::{Risk, RiskAssessment};
use claudio_core::tool::{arg_str, arg_u64, Tool, ToolContext, ToolOutput, ToolRegistry};
use claudio_llm::ToolSpec;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::web::{check_url, find_chromium};

pub fn register(r: &mut ToolRegistry) {
    r.register(Arc::new(BrowserSession));
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Session {
    _child: Child,
    ws: Ws,
    next_id: u64,
    headless: bool,
}

fn session() -> &'static Mutex<Option<Session>> {
    static S: std::sync::OnceLock<Mutex<Option<Session>>> = std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

fn has_display() -> bool {
    if cfg!(target_os = "linux") {
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
    } else {
        true
    }
}

impl Session {
    async fn launch(ctx: &ToolContext, visible: bool) -> Result<Self> {
        let chrome = find_chromium(&ctx.cfg).context(
            "nenhum navegador Chromium/Chrome/Edge encontrado (instale o Chromium ou o Google Chrome)",
        )?;
        let headless = !(visible && has_display());
        let profile = data_dir().join("browser-profile");
        std::fs::create_dir_all(&profile)?;
        let mut args: Vec<String> = vec![
            "--remote-debugging-port=0".into(),
            "--remote-allow-origins=*".into(),
            format!("--user-data-dir={}", profile.display()),
            "--no-first-run".into(),
            "--no-default-browser-check".into(),
            "--disable-dev-shm-usage".into(),
            "--window-size=1280,900".into(),
        ];
        if headless {
            args.push("--headless=new".into());
            args.push("--disable-gpu".into());
        }
        #[cfg(unix)]
        if unsafe { libc::geteuid() } == 0 || ctx.cfg.web.browser_no_sandbox_fallback {
            args.push("--no-sandbox".into());
        }
        args.push("about:blank".into());
        let mut child = Command::new(&chrome)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("falha ao abrir {}", chrome.display()))?;
        // Chromium prints "DevTools listening on ws://127.0.0.1:PORT/devtools/browser/ID".
        let stderr = child.stderr.take().context("stderr")?;
        let mut lines = BufReader::new(stderr).lines();
        let port = tokio::time::timeout(Duration::from_secs(30), async {
            let mut tail = Vec::new();
            while let Ok(Some(l)) = lines.next_line().await {
                if let Some(rest) = l.split("ws://127.0.0.1:").nth(1) {
                    let port: u16 = rest.split('/').next().unwrap_or("").parse()?;
                    return Ok(port);
                }
                tail.push(l);
                if tail.len() > 20 {
                    tail.remove(0);
                }
            }
            bail!("o navegador fechou: {}", tail.join("\n"))
        })
        .await
        .context("o navegador não respondeu em 30 s")??;
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });

        // Use the first page target.
        let list: Value = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/json/list"))
            .send()
            .await?
            .json()
            .await?;
        let ws_url = list
            .as_array()
            .and_then(|a| a.iter().find(|t| t["type"] == "page"))
            .and_then(|t| t["webSocketDebuggerUrl"].as_str())
            .map(String::from)
            .context("nenhuma aba encontrada no navegador")?;
        let (ws, _) = tokio_tungstenite::connect_async(ws_url.as_str())
            .await
            .context("falha ao conectar ao navegador")?;
        let mut s = Self {
            _child: child,
            ws,
            next_id: 0,
            headless,
        };
        s.cmd("Page.enable", json!({})).await?;
        s.cmd("Runtime.enable", json!({})).await?;
        Ok(s)
    }

    async fn cmd(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.ws
            .send(WsMessage::Text(
                json!({"id": id, "method": method, "params": params})
                    .to_string()
                    .into(),
            ))
            .await
            .context("conexão com o navegador perdida")?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let msg = tokio::time::timeout(left, self.ws.next())
                .await
                .with_context(|| format!("o navegador não respondeu a {method}"))?
                .context("conexão com o navegador fechada")??;
            let WsMessage::Text(t) = msg else { continue };
            let v: Value = serde_json::from_str(&t)?;
            if v["id"].as_u64() == Some(id) {
                if let Some(e) = v.get("error") {
                    bail!("{method}: {}", e["message"].as_str().unwrap_or("erro"));
                }
                return Ok(v["result"].clone());
            }
        }
    }

    async fn eval(&mut self, js: &str) -> Result<Value> {
        let r = self
            .cmd(
                "Runtime.evaluate",
                json!({"expression": js, "returnByValue": true, "awaitPromise": true}),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            bail!(
                "erro no script da página: {}",
                ex["exception"]["description"]
                    .as_str()
                    .or(ex["text"].as_str())
                    .unwrap_or("?")
            );
        }
        Ok(r["result"]["value"].clone())
    }

    async fn wait_loaded(&mut self) {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(20) {
            if let Ok(v) = self.eval("document.readyState").await {
                if v == "complete" {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        // Give scripts a moment to render.
        tokio::time::sleep(Duration::from_millis(600)).await;
    }

    async fn center_of(&mut self, id: u64) -> Result<(f64, f64)> {
        let v = self
            .eval(&format!(
                "(() => {{ const e = document.querySelector('[data-claudio-id=\"{id}\"]'); \
                 if (!e) return null; e.scrollIntoView({{block:'center'}}); \
                 const r = e.getBoundingClientRect(); return [r.left + r.width/2, r.top + r.height/2]; }})()"
            ))
            .await?;
        let a = v.as_array().with_context(|| {
            format!("elemento [{id}] não existe mais; use action=read para ver a página atual")
        })?;
        Ok((a[0].as_f64().unwrap_or(0.0), a[1].as_f64().unwrap_or(0.0)))
    }

    async fn click(&mut self, id: u64) -> Result<()> {
        let (x, y) = self.center_of(id).await?;
        for t in ["mouseMoved", "mousePressed", "mouseReleased"] {
            self.cmd(
                "Input.dispatchMouseEvent",
                json!({"type": t, "x": x, "y": y, "button": "left", "clickCount": 1}),
            )
            .await?;
        }
        Ok(())
    }

    async fn key(&mut self, key: &str) -> Result<()> {
        let (code, vk, text) = match key {
            "Enter" => ("Enter", 13, "\r"),
            "Tab" => ("Tab", 9, ""),
            "Escape" => ("Escape", 27, ""),
            "Backspace" => ("Backspace", 8, ""),
            "ArrowDown" => ("ArrowDown", 40, ""),
            "ArrowUp" => ("ArrowUp", 38, ""),
            "PageDown" => ("PageDown", 34, ""),
            "PageUp" => ("PageUp", 33, ""),
            other => bail!("tecla não suportada: {other}"),
        };
        self.cmd(
            "Input.dispatchKeyEvent",
            json!({"type": "keyDown", "key": key, "code": code, "windowsVirtualKeyCode": vk, "text": text}),
        )
        .await?;
        self.cmd(
            "Input.dispatchKeyEvent",
            json!({"type": "keyUp", "key": key, "code": code, "windowsVirtualKeyCode": vk}),
        )
        .await?;
        Ok(())
    }
}

/// Marks visible interactive elements with numbers and returns the page summary.
const READ_JS: &str = r#"(() => {
  const sel = 'a[href], button, input, textarea, select, summary, [role=button], [role=link], [role=tab], [role=menuitem], [role=checkbox], [contenteditable=true], [onclick]';
  let n = 0; const items = [];
  document.querySelectorAll('[data-claudio-id]').forEach(e => e.removeAttribute('data-claudio-id'));
  for (const e of document.querySelectorAll(sel)) {
    const r = e.getBoundingClientRect(); const st = getComputedStyle(e);
    if (r.width < 2 || r.height < 2 || st.visibility === 'hidden' || st.display === 'none') continue;
    if (e.disabled) continue;
    n += 1; e.setAttribute('data-claudio-id', n);
    const tag = e.tagName.toLowerCase();
    let label = (e.innerText || e.value || e.getAttribute('aria-label') || e.getAttribute('placeholder') || e.getAttribute('title') || e.getAttribute('name') || '').trim().replace(/\s+/g, ' ').slice(0, 80);
    const kind = tag === 'input' ? 'input:' + (e.type || 'text') : tag;
    let extra = '';
    if (tag === 'a') extra = ' -> ' + (e.getAttribute('href') || '').slice(0, 100);
    if (tag === 'select') extra = ' opções: ' + Array.from(e.options).slice(0, 15).map(o => o.text.trim()).join(' | ');
    if (e.type === 'checkbox' || e.type === 'radio') extra = e.checked ? ' [marcado]' : ' [desmarcado]';
    if (e.type === 'password') label = '(campo de senha)';
    items.push('[' + n + '] ' + kind + ' "' + label + '"' + extra);
    if (n >= 150) break;
  }
  const text = (document.body ? document.body.innerText : '').replace(/\n{3,}/g, '\n\n');
  return {title: document.title, url: location.href, text: text.slice(0, 12000), items: items};
})()"#;

pub struct BrowserSession;

fn is_password_field_js(id: u64) -> String {
    format!(
        "(() => {{ const e = document.querySelector('[data-claudio-id=\"{id}\"]'); return !!e && e.type === 'password'; }})()"
    )
}

#[async_trait]
impl Tool for BrowserSession {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser_session".into(),
            description: "Navegador interativo (janela visível para o usuário acompanhar). Ações: \
                open (abre url), read (texto da página + elementos numerados [n]), click (id), \
                type (id, text, submit=true aperta Enter), select (id, value), press (key: Enter, Tab, Escape, PageDown...), \
                scroll (direction: down/up), back, screenshot (output), close. \
                Sempre use read depois de open/click para ver os números atuais. Nunca digite senhas do usuário: \
                peça para ele fazer login na janela do navegador."
                .into(),
            parameters: json!({"type":"object","properties":{
                "action":{"type":"string","enum":["open","read","click","type","select","press","scroll","back","screenshot","close"]},
                "url":{"type":"string"},
                "id":{"type":"integer"},
                "text":{"type":"string"},
                "value":{"type":"string"},
                "submit":{"type":"boolean"},
                "key":{"type":"string"},
                "direction":{"type":"string","enum":["down","up"]},
                "output":{"type":"string"},
                "visible":{"type":"boolean","description":"padrão true (janela visível); false = sem janela"}
            },"required":["action"]}),
        }
    }

    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let action = arg_str(args, "action").unwrap_or("read");
        let id = arg_u64(args, "id").unwrap_or(0);
        match action {
            "open" => RiskAssessment::new(
                Risk::Network,
                format!("abrir no navegador: {}", arg_str(args, "url").unwrap_or("")),
            ),
            "read" | "scroll" | "close" => {
                RiskAssessment::new(Risk::Read, format!("navegador: {action}"))
            }
            "screenshot" => {
                let out = ctx
                    .guard
                    .resolve(arg_str(args, "output").unwrap_or("captura-navegador.png"));
                ctx.guard.assess_write(
                    &out,
                    Risk::Write,
                    format!("salvar captura do navegador em {}", out.display()),
                )
            }
            "type" => {
                let text = arg_str(args, "text").unwrap_or("");
                let shown: String = text.chars().take(80).collect();
                let mut ra = RiskAssessment::new(
                    Risk::Network,
                    format!("digitar \"{shown}\" no elemento [{id}] da página"),
                );
                if arg_str(args, "text").map(|t| t.len()).unwrap_or(0) > 0
                    && args
                        .get("submit")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                {
                    ra = ra.ask("enviar um formulário no site");
                }
                ra
            }
            "click" => RiskAssessment::new(
                Risk::Network,
                format!("clicar no elemento [{id}] da página"),
            ),
            _ => RiskAssessment::new(Risk::Network, format!("navegador: {action}")),
        }
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let action = arg_str(&args, "action").unwrap_or("read").to_string();
        let mut guard = session().lock().await;
        if action == "close" {
            *guard = None;
            return Ok(ToolOutput::ok("Navegador fechado."));
        }
        if guard.is_none() {
            let visible = args
                .get("visible")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            *guard = Some(Session::launch(ctx, visible).await?);
        }
        let s = guard.as_mut().unwrap();
        let id = arg_u64(&args, "id");
        let mut note = String::new();
        match action.as_str() {
            "open" => {
                let url = check_url(arg_str(&args, "url").context("url")?, &ctx.cfg).await?;
                s.cmd("Page.navigate", json!({"url": url.as_str()})).await?;
                s.wait_loaded().await;
            }
            "read" => {}
            "click" => {
                s.click(id.context("id")?).await?;
                s.wait_loaded().await;
            }
            "type" => {
                let id = id.context("id")?;
                if s.eval(&is_password_field_js(id)).await? == json!(true) {
                    bail!("é um campo de senha: peça ao usuário para digitar a senha diretamente na janela do navegador");
                }
                s.click(id).await?;
                s.eval(&format!(
                    "(() => {{ const e = document.querySelector('[data-claudio-id=\"{id}\"]'); \
                     if (e && 'value' in e) {{ e.value = ''; e.dispatchEvent(new Event('input', {{bubbles:true}})); }} }})()"
                ))
                .await?;
                let text = arg_str(&args, "text").unwrap_or("");
                s.cmd("Input.insertText", json!({"text": text})).await?;
                if args
                    .get("submit")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    s.key("Enter").await?;
                    s.wait_loaded().await;
                }
            }
            "select" => {
                let id = id.context("id")?;
                let value = arg_str(&args, "value").unwrap_or("");
                let ok = s
                    .eval(&format!(
                        "(() => {{ const e = document.querySelector('[data-claudio-id=\"{id}\"]'); if (!e || !e.options) return false; \
                         const v = {}; const o = Array.from(e.options).find(o => o.value === v || o.text.trim() === v); \
                         if (!o) return false; e.value = o.value; e.dispatchEvent(new Event('change', {{bubbles:true}})); return true; }})()",
                        serde_json::to_string(value)?
                    ))
                    .await?;
                if ok != json!(true) {
                    bail!("opção '{value}' não encontrada no elemento [{id}]");
                }
            }
            "press" => {
                s.key(arg_str(&args, "key").unwrap_or("Enter")).await?;
                s.wait_loaded().await;
            }
            "scroll" => {
                let dy = if arg_str(&args, "direction") == Some("up") {
                    -700
                } else {
                    700
                };
                s.eval(&format!("window.scrollBy(0, {dy})")).await?;
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
            "back" => {
                s.eval("history.back()").await?;
                s.wait_loaded().await;
            }
            "screenshot" => {
                let out: PathBuf = ctx
                    .guard
                    .resolve(arg_str(&args, "output").unwrap_or("captura-navegador.png"));
                let r = s
                    .cmd("Page.captureScreenshot", json!({"format": "png"}))
                    .await?;
                let data = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    r["data"].as_str().unwrap_or(""),
                )?;
                if let Some(d) = out.parent() {
                    std::fs::create_dir_all(d)?;
                }
                std::fs::write(&out, &data)?;
                return Ok(ToolOutput::ok(format!(
                    "Captura salva em {} ({} bytes).",
                    out.display(),
                    data.len()
                ))
                .with_artifact(out));
            }
            other => bail!("ação desconhecida: {other}"),
        }
        if s.headless
            && action == "open"
            && args.get("visible").and_then(|v| v.as_bool()) != Some(false)
        {
            note = "(sem tela disponível: navegador rodando sem janela)\n".into();
        }
        let page = s.eval(READ_JS).await?;
        let items = page["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|i| i.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let url = page["url"].as_str().unwrap_or("").to_string();
        let body = format!(
            "{note}Título: {}\nURL: {url}\n\nElementos interativos:\n{items}\n\nTexto da página:\n{}",
            page["title"].as_str().unwrap_or(""),
            page["text"].as_str().unwrap_or("")
        );
        Ok(
            ToolOutput::ok(claudio_core::safety::truncate_middle(&body, 20_000))
                .untrusted(format!("browser:{url}")),
        )
    }
}
