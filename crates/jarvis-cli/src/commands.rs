use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use jarvis_core::audit::Audit;
use jarvis_core::config::{data_dir, Config};
use jarvis_core::events::{AgentEvent, Approver, AutoApprove, EventSink, NonInteractive};
use jarvis_core::store::SessionStatus;
use jarvis_core::RunOutcome;
use jarvis_llm::router::Availability;
use jarvis_llm::{Part, Role};
use jarvis_tools::runtime::Runtime;
use tokio_util::sync::CancellationToken;

use crate::term::*;
use crate::{AuditCmd, Cli, ConfigCmd, ConsentCmd, MemoryCmd, ScheduleCmd, VoiceCmd};

fn load_config(cli: &Cli) -> Result<Config> {
    match &cli.config {
        Some(p) => Config::load_from(p),
        None => Config::load(),
    }
}

async fn runtime(cli: &Cli) -> Result<Runtime> {
    let rt = Runtime::init(load_config(cli)?).await?;
    if !cli.json {
        for w in &rt.warnings {
            eprintln!("{} {w}", yellow("⚠"));
        }
    }
    Ok(rt)
}

fn approver(cli: &Cli, allow: Option<&str>) -> Arc<dyn Approver> {
    if cli.yes {
        Arc::new(AutoApprove)
    } else if cli.non_interactive || !std::io::stdin().is_terminal() {
        Arc::new(NonInteractive {
            preapproved: allow
                .map(|a| a.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default(),
        })
    } else {
        Arc::new(crate::term::TerminalApprover::new())
    }
}

/// Cancel on Ctrl+C: first press cancels the run, second exits.
fn ctrlc_token() -> CancellationToken {
    let token = CancellationToken::new();
    let t = token.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\n{}", yellow("cancelando… (Ctrl+C de novo para sair)"));
            t.cancel();
            if tokio::signal::ctrl_c().await.is_ok() {
                std::process::exit(130);
            }
        }
    });
    token
}

fn exit_code(out: &RunOutcome) -> i32 {
    match out.status {
        SessionStatus::Completed => 0,
        SessionStatus::CompletedUnverified => 0,
        SessionStatus::Cancelled => 130,
        SessionStatus::LimitReached => 3,
        _ => 1,
    }
}

fn print_outcome(cli: &Cli, out: &RunOutcome) {
    if cli.json {
        println!(
            "{}",
            serde_json::json!({"session_id": out.session_id, "status": out.status, "summary": out.summary})
        );
        return;
    }
    let label = match out.status {
        SessionStatus::Completed => green("concluído e verificado"),
        SessionStatus::CompletedUnverified => yellow("concluído (sem verificação)"),
        SessionStatus::LimitReached => yellow("parado por limite"),
        SessionStatus::Cancelled => yellow("cancelado"),
        _ => red("falhou"),
    };
    println!(
        "\n{} {label}  {}",
        bold("Resultado:"),
        dim(&format!("sessão {}", out.session_id))
    );
    if !out.summary.trim().is_empty() {
        println!("{}", out.summary.trim());
    }
}

pub async fn run_task(cli: Arc<Cli>, task: String, allow: Option<String>) -> Result<i32> {
    if task.trim().is_empty() {
        bail!("informe a tarefa: jarvis run \"...\"");
    }
    let rt = runtime(&cli).await?;
    let approver = approver(&cli, allow.as_deref());
    let sink: Arc<dyn EventSink> = Arc::new(Printer { json: cli.json });
    let sel = rt
        .select_model(cli.model.as_deref(), &task, approver.as_ref())
        .await?;
    sink.emit(AgentEvent::ModelSelected {
        model: sel.provider.id(),
        reason: sel.reason.clone(),
    });
    let agent = rt.agent(&sel, approver, sink);
    let out = agent.start(&task, ctrlc_token()).await;
    rt.shutdown().await;
    let out = out?;
    print_outcome(&cli, &out);
    Ok(exit_code(&out))
}

pub async fn resume(cli: Arc<Cli>, id: &str) -> Result<i32> {
    let rt = runtime(&cli).await?;
    let rec = rt
        .store
        .get_session(id)?
        .context("sessão não encontrada (ou prefixo ambíguo)")?;
    let approver = approver(&cli, None);
    let sink: Arc<dyn EventSink> = Arc::new(Printer { json: cli.json });
    let sel = rt
        .select_model(cli.model.as_deref(), &rec.task, approver.as_ref())
        .await?;
    let agent = rt.agent(&sel, approver, sink);
    let out = agent.resume(&rec.id, ctrlc_token()).await;
    rt.shutdown().await;
    let out = out?;
    print_outcome(&cli, &out);
    Ok(exit_code(&out))
}

pub async fn chat(cli: Arc<Cli>, session: Option<String>) -> Result<i32> {
    let rt = runtime(&cli).await?;
    let approver = approver(&cli, None);
    let sink: Arc<dyn EventSink> = Arc::new(Printer { json: cli.json });
    let mut model_name = cli.model.clone();
    let mut session_id = match &session {
        Some(s) => Some(
            rt.store
                .get_session(s)?
                .context("sessão não encontrada")?
                .id,
        ),
        None => None,
    };
    println!(
        "{}",
        bold("Jarvis Core — chat. Comandos: /sair, /nova, /modelo <nome>, /plano, /sessoes")
    );
    let mut sel = rt
        .select_model(model_name.as_deref(), "", approver.as_ref())
        .await?;
    println!(
        "{}",
        dim(&format!("modelo: {} ({})", sel.provider.id(), sel.reason))
    );
    loop {
        print!("\n{} ", cyan("você ›"));
        let _ = std::io::stdout().flush();
        let line = tokio::task::spawn_blocking(|| {
            let mut s = String::new();
            match std::io::stdin().read_line(&mut s) {
                Ok(0) => None,
                Ok(_) => Some(s),
                Err(_) => None,
            }
        })
        .await?;
        let Some(line) = line else { break };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        match line.split_whitespace().next().unwrap_or("") {
            "/sair" | "/exit" | "/quit" => break,
            "/nova" | "/new" => {
                session_id = None;
                println!("{}", dim("nova conversa"));
                continue;
            }
            "/modelo" | "/model" => {
                model_name = line.split_whitespace().nth(1).map(String::from);
                match rt
                    .select_model(model_name.as_deref(), "", approver.as_ref())
                    .await
                {
                    Ok(s) => {
                        println!(
                            "{}",
                            dim(&format!("modelo: {} ({})", s.provider.id(), s.reason))
                        );
                        sel = s;
                    }
                    Err(e) => println!("{} {e:#}", red("erro:")),
                }
                continue;
            }
            "/plano" | "/plan" => {
                if let Some(id) = &session_id {
                    if let Some(r) = rt.store.get_session(id)? {
                        println!(
                            "{}",
                            if r.plan.steps.is_empty() {
                                "(sem plano)".into()
                            } else {
                                r.plan.render()
                            }
                        );
                    }
                }
                continue;
            }
            "/sessoes" | "/sessions" => {
                sessions(&cli, 10).await?;
                continue;
            }
            _ => {}
        }
        let agent = rt.agent(&sel, approver.clone(), sink.clone());
        let token = ctrlc_token();
        let out = match &session_id {
            Some(id) => agent.send(id, &line, token).await,
            None => agent.start(&line, token).await,
        };
        match out {
            Ok(out) => {
                session_id = Some(out.session_id.clone());
                if out.status != SessionStatus::Completed
                    && out.status != SessionStatus::CompletedUnverified
                {
                    print_outcome(&cli, &out);
                }
            }
            Err(e) => println!("{} {e:#}", red("erro:")),
        }
    }
    rt.shutdown().await;
    Ok(0)
}

pub async fn sessions(cli: &Cli, limit: usize) -> Result<i32> {
    let rt = runtime(cli).await?;
    let list = rt.store.list_sessions(limit)?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(0);
    }
    if list.is_empty() {
        println!("Nenhuma sessão.");
    }
    for s in list {
        let (d, t) = s.plan.progress();
        let plan = if t > 0 {
            format!(" plano {d}/{t}")
        } else {
            String::new()
        };
        println!(
            "{}  {:<22} {}{}  {}",
            &s.id[..8],
            s.status.as_str(),
            &s.updated_at[..16.min(s.updated_at.len())],
            plan,
            s.title
        );
    }
    Ok(0)
}

pub async fn show(cli: &Cli, id: &str) -> Result<i32> {
    let rt = runtime(cli).await?;
    let rec = rt.store.get_session(id)?.context("sessão não encontrada")?;
    println!(
        "{} {}\nstatus: {}\nmodelo: {}\ntarefa: {}\n",
        bold("Sessão"),
        rec.id,
        rec.status.as_str(),
        rec.model,
        rec.task
    );
    if !rec.plan.steps.is_empty() {
        println!("{}\n{}\n", bold("Plano:"), rec.plan.render());
    }
    for m in rt.store.messages(&rec.id)? {
        match m.role {
            Role::System => continue,
            Role::User => println!("{} {}", cyan("usuário:"), m.text_content()),
            Role::Assistant => {
                let t = m.text_content();
                if !t.is_empty() {
                    println!("{} {t}", green("jarvis:"));
                }
                for c in m.tool_calls() {
                    println!(
                        "  {} {} {}",
                        cyan("→"),
                        c.name,
                        dim(&c
                            .arguments
                            .to_string()
                            .chars()
                            .take(200)
                            .collect::<String>())
                    );
                }
            }
            Role::Tool => {
                for p in &m.parts {
                    if let Part::ToolResult {
                        content, is_error, ..
                    } = p
                    {
                        let first: String = content
                            .lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(160)
                            .collect();
                        println!(
                            "    {} {}",
                            if *is_error { red("✗") } else { green("✓") },
                            dim(&first)
                        );
                    }
                }
            }
        }
    }
    if let Some(s) = rec.summary {
        println!("\n{}\n{s}", bold("Resumo:"));
    }
    Ok(0)
}

pub async fn history(cli: &Cli, q: &str) -> Result<i32> {
    let rt = runtime(cli).await?;
    for (sid, snip) in rt.store.search_history(q, 30)? {
        println!("{}  {}", &sid[..8], snip.replace('\n', " "));
    }
    Ok(0)
}

pub async fn memory(cli: &Cli, cmd: &MemoryCmd) -> Result<i32> {
    let rt = runtime(cli).await?;
    match cmd {
        MemoryCmd::List => {
            let list = rt.store.list_memories()?;
            if list.is_empty() {
                println!("Nenhuma memória.");
            }
            for m in list {
                println!(
                    "#{:<4} {:<10} {}  {}",
                    m.id,
                    m.kind,
                    m.content,
                    dim(&format!("[{}] {}", m.tags, m.source))
                );
            }
        }
        MemoryCmd::Add {
            content,
            kind,
            tags,
        } => {
            let id = rt
                .memory
                .save(kind, &content.join(" "), tags, "user")
                .await?;
            println!("memória #{id} salva");
        }
        MemoryCmd::Search { query } => {
            for h in rt.memory.search(&query.join(" "), 10).await? {
                println!("#{:<4} {:.2}  {}", h.record.id, h.score, h.record.content);
            }
        }
        MemoryCmd::Delete { id } => {
            if rt.store.delete_memory(*id)? {
                println!("memória #{id} apagada");
            } else {
                bail!("memória #{id} não existe");
            }
        }
        MemoryCmd::Clear { yes } => {
            if !yes {
                bail!("use --yes para confirmar a remoção de todas as memórias");
            }
            println!("{} memórias apagadas", rt.store.clear_memories()?);
        }
    }
    Ok(0)
}

pub async fn models(cli: &Cli) -> Result<i32> {
    let rt = runtime(cli).await?;
    let avail = rt.availability().await;
    let res = jarvis_llm::router::detect_resources();
    println!(
        "{}",
        dim(&format!(
            "RAM livre: {} MB de {} MB, {} threads",
            res.mem_available_mb, res.mem_total_mb, res.cpu_threads
        ))
    );
    for m in &rt.cfg.models {
        let a = match avail.get(&m.name) {
            Some(Availability::Ready) => green("pronto"),
            Some(Availability::Startable) => green("inicia sob demanda"),
            Some(Availability::Unavailable(why)) => red(&format!("indisponível: {why}")),
            None => "?".into(),
        };
        let default = if rt.cfg.default_model.as_deref() == Some(&m.name) {
            "*"
        } else {
            " "
        };
        println!(
            "{default}{:<10} {:<14} {:<28} {} {}",
            m.name,
            format!("{:?}", m.provider).to_lowercase(),
            m.model,
            if m.is_local() {
                cyan("local ")
            } else {
                yellow("remoto")
            },
            a
        );
    }
    Ok(0)
}

pub async fn serve(cli: &Cli, gguf: &Path, port: u16, gpu_layers: u32, ctx: u32) -> Result<i32> {
    let cfg = load_config(cli)?;
    println!(
        "Iniciando llama-server em 127.0.0.1:{port} com {}",
        gguf.display()
    );
    let server = jarvis_llm::local::LlamaServer::start(&jarvis_llm::local::LlamaServerOptions {
        binary: cfg
            .local
            .llama_server
            .as_deref()
            .map(jarvis_core::config::expand),
        gguf: gguf.to_path_buf(),
        port,
        ctx_size: ctx,
        gpu_layers,
        threads: cfg.local.threads,
        extra_args: vec![],
    })
    .await?;
    println!(
        "{} {} — Ctrl+C para parar",
        green("pronto:"),
        server.base_url
    );
    tokio::signal::ctrl_c().await?;
    server.stop().await;
    Ok(0)
}

pub async fn tools(cli: &Cli) -> Result<i32> {
    let rt = runtime(cli).await?;
    for s in rt.tools.specs() {
        println!(
            "{:<26} {}",
            bold(&s.name),
            s.description.lines().next().unwrap_or("")
        );
    }
    Ok(0)
}

pub async fn audit(cmd: &AuditCmd) -> Result<i32> {
    let path = data_dir().join("audit.jsonl");
    match cmd {
        AuditCmd::Verify => match Audit::verify(&path) {
            Ok(n) => println!(
                "{} {n} entradas íntegras ({})",
                green("ok:"),
                path.display()
            ),
            Err(e) => {
                println!("{} {e}", red("auditoria comprometida:"));
                return Ok(1);
            }
        },
        AuditCmd::Tail { n } => {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            for l in &lines[lines.len().saturating_sub(*n)..] {
                println!("{l}");
            }
        }
    }
    Ok(0)
}

pub async fn config(cli: &Cli, cmd: &ConfigCmd) -> Result<i32> {
    match cmd {
        ConfigCmd::Init => {
            let p = Config::save_default_if_missing()?;
            println!("configuração em {}", p.display());
        }
        ConfigCmd::Path => println!(
            "{}",
            cli.config.clone().unwrap_or_else(Config::path).display()
        ),
        ConfigCmd::Show => {
            let c = load_config(cli)?;
            println!("{}", toml_string(&c)?);
        }
    }
    Ok(0)
}

fn toml_string(c: &Config) -> Result<String> {
    // api_key is skip_serializing, so secrets never print.
    Ok(toml::to_string_pretty(c).unwrap_or_else(|e| format!("(erro ao serializar: {e})")))
}

pub async fn consent(cli: &Cli, cmd: &ConsentCmd) -> Result<i32> {
    let rt = runtime(cli).await?;
    match cmd {
        ConsentCmd::List => {
            for m in &rt.cfg.models {
                if !m.is_local() {
                    let k = jarvis_core::consent::consent_key(m);
                    println!(
                        "{:<10} {:<50} {}",
                        m.name,
                        k,
                        if rt.store.has_consent(&k)? {
                            green("autorizado")
                        } else {
                            dim("não autorizado")
                        }
                    );
                }
            }
        }
        ConsentCmd::Revoke { key } => {
            let key = rt
                .cfg
                .model(key)
                .map(jarvis_core::consent::consent_key)
                .unwrap_or(key.clone());
            println!(
                "{}",
                if rt.store.revoke_consent(&key)? {
                    "revogado"
                } else {
                    "não havia consentimento"
                }
            );
        }
    }
    Ok(0)
}

pub async fn doctor(cli: &Cli) -> Result<i32> {
    let cfg = load_config(cli)?;
    let hint = |linux: &'static str, mac: &'static str, win: &'static str| -> &'static str {
        if cfg!(target_os = "macos") {
            mac
        } else if cfg!(windows) {
            win
        } else {
            linux
        }
    };
    let check = |ok: bool, label: &str, hint: &str| {
        if ok {
            println!("{} {label}", green("✓"));
        } else {
            println!("{} {label} {}", yellow("·"), dim(hint));
        }
    };
    println!("{}", bold("Jarvis doctor"));
    println!(
        "config: {} {}",
        Config::path().display(),
        if Config::path().exists() {
            ""
        } else {
            "(padrões; crie com `jarvis config init`)"
        }
    );
    if cfg.insecure_file {
        println!(
            "{} config legível por outros usuários: chmod 600 {}",
            red("✗"),
            Config::path().display()
        );
    }
    println!("dados: {}", data_dir().display());
    let res = jarvis_llm::router::detect_resources();
    println!(
        "RAM livre: {} MB / {} MB — threads: {}",
        res.mem_available_mb, res.mem_total_mb, res.cpu_threads
    );
    let w = |n: &str| jarvis_tools::shell::which(n).is_some();
    let llama = jarvis_llm::local::find_binary(
        cfg.local
            .llama_server
            .as_deref()
            .map(jarvis_core::config::expand)
            .as_deref(),
    )
    .is_some();
    check(
        llama,
        "llama-server (modelos locais)",
        hint(
            "→ compile o llama.cpp (docs/MODELOS-LOCAIS.md) ou defina local.llama_server",
            "→ brew install llama.cpp",
            "→ winget install ggml.llamacpp (ou baixe o zip do llama.cpp) e defina local.llama_server",
        ),
    );
    let sandbox = jarvis_tools::shell::sandbox_available();
    if cfg!(windows) {
        println!(
            "{} isolamento do terminal: não disponível no Windows (aprovação continua obrigatória)",
            yellow("·")
        );
    } else {
        check(
            sandbox,
            "isolamento do terminal (bubblewrap / sandbox-exec)",
            hint(
                "→ sudo apt install bubblewrap (e permita user namespaces)",
                "→ sandbox-exec faz parte do macOS",
                "",
            ),
        );
    }
    check(
        w("pdftotext"),
        "poppler (leitura fiel de PDF; sem ele usa leitor interno)",
        hint(
            "→ sudo apt install poppler-utils",
            "→ brew install poppler",
            "→ choco install poppler",
        ),
    );
    check(
        w("pandoc"),
        "pandoc (DOCX/PPTX ricos)",
        hint(
            "→ sudo apt install pandoc",
            "→ brew install pandoc",
            "→ winget install JohnMacFarlane.Pandoc",
        ),
    );
    check(
        w("soffice"),
        "LibreOffice (conversões)",
        hint(
            "→ sudo apt install libreoffice",
            "→ brew install --cask libreoffice",
            "→ winget install TheDocumentFoundation.LibreOffice",
        ),
    );
    check(
        w("pdflatex") || w("xelatex") || w("tectonic"),
        "LaTeX",
        hint(
            "→ sudo apt install texlive-latex-recommended (ou tectonic)",
            "→ brew install tectonic",
            "→ winget install MiKTeX.MiKTeX (ou tectonic)",
        ),
    );
    check(
        w("ffmpeg") && w("ffprobe"),
        "ffmpeg/ffprobe (áudio, vídeo e microfone)",
        hint(
            "→ sudo apt install ffmpeg",
            "→ brew install ffmpeg",
            "→ winget install Gyan.FFmpeg",
        ),
    );
    check(
        w("tesseract"),
        "Tesseract (OCR)",
        hint(
            "→ sudo apt install tesseract-ocr tesseract-ocr-por",
            "→ brew install tesseract tesseract-lang",
            "→ winget install UB-Mannheim.TesseractOCR",
        ),
    );
    check(
        w("whisper-cli") || w("whisper-cpp"),
        "whisper.cpp (transcrição)",
        hint(
            "→ compile whisper.cpp; veja docs/VOICE.md",
            "→ brew install whisper-cpp",
            "→ baixe o whisper.cpp; veja docs/VOICE.md",
        ),
    );
    check(
        w("piper") || w("espeak-ng") || cfg!(windows) || (cfg!(target_os = "macos") && w("say")),
        "síntese de voz (piper, espeak-ng, say ou SAPI)",
        "→ sudo apt install espeak-ng",
    );
    check(
        jarvis_tools::web::find_chromium(&cfg).is_some(),
        "Chromium/Chrome/Edge (navegador headless)",
        hint(
            "→ sudo apt install chromium",
            "→ brew install --cask chromium",
            "→ o Microsoft Edge já vem com o Windows",
        ),
    );
    for key in ["ANTHROPIC_API_KEY", "GEMINI_API_KEY", "OPENAI_API_KEY"] {
        check(
            std::env::var(key).map(|v| !v.is_empty()).unwrap_or(false),
            &format!("{key} definido"),
            "(opcional)",
        );
    }
    Ok(0)
}

/// Re-launch this command without `--detach`, detached from the terminal.
pub fn detach() -> Result<i32> {
    let logs = data_dir().join("logs");
    std::fs::create_dir_all(&logs)?;
    let log = logs.join(format!(
        "run-{}.log",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    let f = std::fs::File::create(&log)?;
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--detach")
        .collect();
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.args(&args)
        .arg("--non-interactive")
        .stdin(std::process::Stdio::null())
        .stdout(f.try_clone()?)
        .stderr(f);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
        cmd.creation_flags(0x0000_0008 | 0x0000_0200 | 0x0800_0000);
    }
    let child = cmd.spawn()?;
    println!(
        "Em segundo plano: pid {} — log {}",
        child.id(),
        log.display()
    );
    println!(
        "Acompanhe com `jarvis sessions` e `tail -f {}`.",
        log.display()
    );
    Ok(0)
}

pub async fn workflow(cli: Arc<Cli>, file: &Path, extra: &[String]) -> Result<i32> {
    let mut wf = jarvis_core::workflow::Workflow::load(file)?;
    for kv in extra {
        let (k, v) = kv.split_once('=').context("use --var nome=valor")?;
        wf.vars.insert(k.into(), v.into());
    }
    let rt = runtime(&cli).await?;
    let approver = approver(&cli, Some(&wf.allowed_tools.join(",")));
    let sink: Arc<dyn EventSink> = Arc::new(Printer { json: cli.json });
    let task_text: String = wf
        .steps
        .iter()
        .filter_map(|s| s.task.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let sel = rt
        .select_model(cli.model.as_deref(), &task_text, approver.as_ref())
        .await?;
    let agent = rt.agent(&sel, approver, sink);
    println!("{} {}", bold("Workflow:"), wf.name);
    let out = jarvis_core::workflow::run_workflow(&agent, &wf, ctrlc_token()).await;
    rt.shutdown().await;
    let out = out?;
    for l in &out.log {
        println!("  {l}");
    }
    println!(
        "{} sessão {}",
        if out.ok {
            green("concluído")
        } else {
            red("falhou")
        },
        out.session_id
    );
    Ok(if out.ok { 0 } else { 1 })
}

pub async fn schedule(cli: &Cli, cmd: &ScheduleCmd) -> Result<i32> {
    let rt = runtime(cli).await?;
    match cmd {
        ScheduleCmd::Add {
            name,
            cron,
            task,
            workflow,
            allow,
            now,
        } => {
            jarvis_core::workflow::parse_cron(cron)?;
            let (kind, payload) = match (task, workflow) {
                (Some(t), None) => ("task", t.clone()),
                (None, Some(w)) => {
                    let abs = std::fs::canonicalize(w)
                        .with_context(|| format!("{} não existe", w.display()))?;
                    jarvis_core::workflow::Workflow::load(&abs)?;
                    ("workflow", abs.to_string_lossy().to_string())
                }
                _ => bail!("informe --task ou --workflow"),
            };
            let id = rt.store.add_schedule(name, cron, kind, &payload, allow)?;
            let baseline = if *now {
                chrono::Utc::now() - chrono::Duration::days(366)
            } else {
                chrono::Utc::now()
            };
            rt.store.set_schedule_last_run(id, &baseline.to_rfc3339())?;
            println!("agendamento #{id} criado. Rode `jarvis daemon` (ou o serviço systemd --user) para executá-lo.");
        }
        ScheduleCmd::List => {
            for s in rt.store.list_schedules()? {
                println!(
                    "#{:<3} {:<20} {:<16} {:<8} {}  {}",
                    s.id,
                    s.name,
                    s.cron,
                    s.kind,
                    s.payload.chars().take(60).collect::<String>(),
                    dim(&format!("última: {}", s.last_run.unwrap_or_default()))
                );
            }
        }
        ScheduleCmd::Delete { id } => println!(
            "{}",
            if rt.store.delete_schedule(*id)? {
                "removido"
            } else {
                "não existe"
            }
        ),
    }
    Ok(0)
}

pub async fn daemon(cli: Arc<Cli>, once: bool) -> Result<i32> {
    let lock = data_dir().join("daemon.pid");
    if let Ok(pid) = std::fs::read_to_string(&lock) {
        let other = pid
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|p| *p != std::process::id());
        if other.map(jarvis_llm::platform::pid_alive).unwrap_or(false) {
            bail!("daemon já em execução (pid {})", pid.trim());
        }
    }
    std::fs::create_dir_all(data_dir())?;
    std::fs::write(&lock, std::process::id().to_string())?;
    let rt = runtime(&cli).await?;
    let sink: Arc<dyn EventSink> = Arc::new(Printer { json: cli.json });
    println!(
        "{}",
        dim("daemon de agendamentos iniciado (nenhuma porta de rede é aberta)")
    );
    let cancel = ctrlc_token();
    loop {
        for s in rt.store.list_schedules()?.into_iter().filter(|s| s.enabled) {
            let now = chrono::Utc::now();
            if !jarvis_core::workflow::is_due(&s.cron, s.last_run.as_deref(), now, now)
                .unwrap_or(false)
            {
                continue;
            }
            rt.store.set_schedule_last_run(s.id, &now.to_rfc3339())?;
            println!("{} #{} {}", bold("executando agendamento"), s.id, s.name);
            let allowed: Vec<String> = s
                .allowed_tools
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            let approver: Arc<dyn Approver> = Arc::new(NonInteractive {
                preapproved: allowed,
            });
            let result: Result<()> = async {
                if s.kind == "workflow" {
                    let wf = jarvis_core::workflow::Workflow::load(Path::new(&s.payload))?;
                    let sel = rt
                        .select_model(cli.model.as_deref(), &wf.name, approver.as_ref())
                        .await?;
                    let agent = rt.agent(&sel, approver.clone(), sink.clone());
                    let out =
                        jarvis_core::workflow::run_workflow(&agent, &wf, cancel.child_token())
                            .await?;
                    println!(
                        "  workflow {}: {}",
                        if out.ok { "ok" } else { "falhou" },
                        out.log.join("; ")
                    );
                } else {
                    let sel = rt
                        .select_model(cli.model.as_deref(), &s.payload, approver.as_ref())
                        .await?;
                    let agent = rt.agent(&sel, approver.clone(), sink.clone());
                    let out = agent.start(&s.payload, cancel.child_token()).await?;
                    println!("  tarefa: {}", out.status.as_str());
                }
                Ok(())
            }
            .await;
            if let Err(e) = result {
                println!("{} agendamento #{}: {e:#}", red("erro"), s.id);
            }
        }
        if once || cancel.is_cancelled() {
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(20)) => {}
            _ = cancel.cancelled() => break,
        }
    }
    rt.shutdown().await;
    let _ = std::fs::remove_file(&lock);
    Ok(0)
}

pub async fn voice(cli: Arc<Cli>, cmd: &VoiceCmd) -> Result<i32> {
    let cfg = load_config(&cli)?;
    let dir = tempfile::tempdir()?;
    let mut c2 = cfg.clone();
    c2.permissions
        .workspace_roots
        .push(dir.path().to_string_lossy().to_string());
    let ctx = jarvis_tools::testutil::context_with(c2);
    match cmd {
        VoiceCmd::Transcribe { file, lang } => {
            let p = jarvis_llm::platform::canonicalize(file)?;
            println!("{}", jarvis_tools::voice::transcribe(&p, lang, &ctx).await?);
        }
        VoiceCmd::Say { text, out } => {
            let engine = jarvis_tools::voice::speak(&text.join(" "), out, &ctx).await?;
            println!("áudio em {} ({engine})", out.display());
            play_wav(out).await;
        }
        VoiceCmd::Listen { seconds } => {
            let wav = dir.path().join("mic.wav");
            println!("gravando {seconds} s…");
            record_mic(&wav, *seconds).await?;
            let text = jarvis_tools::voice::transcribe(&wav, "pt", &ctx).await?;
            println!("{} {text}", cyan("você disse:"));
            if !text.trim().is_empty() {
                return run_task(cli.clone(), text, None).await;
            }
        }
    }
    Ok(0)
}

/// Play a WAV file with the player available on this OS.
async fn play_wav(path: &Path) {
    use tokio::process::Command;
    let status = if cfg!(target_os = "macos") {
        Command::new("afplay").arg(path).status().await
    } else if cfg!(windows) {
        Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(New-Object Media.SoundPlayer $env:JARVIS_WAV).PlaySync()",
            ])
            .env("JARVIS_WAV", path)
            .status()
            .await
    } else {
        let Some(player) = ["paplay", "pw-play", "aplay"]
            .into_iter()
            .find(|p| jarvis_tools::shell::which(p).is_some())
        else {
            return;
        };
        Command::new(player).arg(path).status().await
    };
    if let Err(e) = status {
        eprintln!("não foi possível tocar o áudio: {e}");
    }
}

/// Record from the default microphone (16 kHz mono WAV).
async fn record_mic(wav: &Path, seconds: u32) -> Result<()> {
    use tokio::process::Command;
    let secs = seconds.to_string();
    let status = if cfg!(target_os = "linux") && jarvis_tools::shell::which("arecord").is_some() {
        Command::new("arecord")
            .args(["-q", "-f", "S16_LE", "-r", "16000", "-c", "1", "-d", &secs])
            .arg(wav)
            .status()
            .await?
    } else {
        // macOS (AVFoundation) and Windows (DirectShow) through ffmpeg.
        let ffmpeg = jarvis_tools::shell::which("ffmpeg")
            .context("gravar do microfone requer ffmpeg (ou arecord no Linux)")?;
        let input: Vec<&str> = if cfg!(target_os = "macos") {
            vec!["-f", "avfoundation", "-i", ":0"]
        } else if cfg!(windows) {
            vec!["-f", "dshow", "-i", "audio=default"]
        } else {
            vec!["-f", "pulse", "-i", "default"]
        };
        Command::new(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(input)
            .args(["-t", &secs, "-ar", "16000", "-ac", "1"])
            .arg(wav)
            .status()
            .await?
    };
    if !status.success() {
        bail!("falha ao gravar do microfone");
    }
    Ok(())
}
