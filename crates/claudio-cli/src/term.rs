//! Terminal output: event printer and interactive approver.

use std::io::{IsTerminal, Write};

use async_trait::async_trait;
use claudio_core::events::{AgentEvent, ApprovalDecision, ApprovalRequest, Approver, EventSink};
use claudio_core::permissions::Risk;
use tokio::sync::Mutex;

fn color(code: &str, s: &str) -> String {
    if std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}
pub fn red(s: &str) -> String {
    color("31", s)
}
pub fn green(s: &str) -> String {
    color("32", s)
}
pub fn yellow(s: &str) -> String {
    color("33", s)
}
pub fn cyan(s: &str) -> String {
    color("36", s)
}
pub fn dim(s: &str) -> String {
    color("2", s)
}
pub fn bold(s: &str) -> String {
    color("1", s)
}

pub struct Printer {
    pub json: bool,
}

impl EventSink for Printer {
    fn emit(&self, e: AgentEvent) {
        if self.json {
            println!("{}", serde_json::to_string(&e).unwrap_or_default());
            return;
        }
        let tty = std::io::stderr().is_terminal();
        if tty {
            eprint!("\r\x1b[2K");
        }
        match e {
            AgentEvent::SessionStarted {
                session_id,
                model,
                resumed,
            } => {
                println!(
                    "{}",
                    dim(&format!(
                        "● sessão {} {} — {model}",
                        &session_id[..8],
                        if resumed { "(continuação)" } else { "" }
                    ))
                )
            }
            AgentEvent::ModelSelected { model, reason } => {
                println!("{}", dim(&format!("● modelo {model}: {reason}")))
            }
            AgentEvent::ModelCall { n, phase } => {
                if tty {
                    eprint!("{}", dim(&format!("  … pensando ({phase}, chamada {n})")));
                    let _ = std::io::stderr().flush();
                }
            }
            AgentEvent::AssistantText { text } => println!("\n{text}\n"),
            AgentEvent::PlanUpdated { plan } => println!("{}\n{}", bold("Plano:"), plan.render()),
            AgentEvent::ToolCall {
                name,
                summary,
                risk,
                ..
            } => {
                let s: String = summary.chars().take(160).collect();
                println!(
                    "{} {} {}",
                    cyan("→"),
                    bold(&name),
                    dim(&format!("[{}] {s}", risk.label()))
                )
            }
            AgentEvent::ToolDecision { decision, .. } if decision.contains("negado") => {
                println!("  {}", red(&decision))
            }
            AgentEvent::ToolDecision { .. } => {}
            AgentEvent::ToolResult {
                ok, preview, ms, ..
            } => {
                let first: String = preview
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" ⏎ ")
                    .chars()
                    .take(200)
                    .collect();
                let mark = if ok { green("✓") } else { red("✗") };
                println!("  {mark} {} {}", dim(&format!("({ms} ms)")), dim(&first));
            }
            AgentEvent::Warning { message } => println!("{} {message}", yellow("⚠")),
            AgentEvent::VerificationStarted { round } => println!(
                "{}",
                yellow(&format!("◆ verificando o resultado (rodada {round})"))
            ),
            AgentEvent::VerificationResult { passed, notes } => {
                if passed {
                    println!("{}", green("◆ verificação: PASS"))
                } else {
                    println!("{} {notes}", red("◆ verificação: FAIL —"))
                }
            }
            AgentEvent::LimitReached { limit } => {
                println!("{} {limit}", yellow("⏹ limite atingido:"))
            }
            AgentEvent::Finished { status, .. } => {
                println!("{}", dim(&format!("● fim: {}", status.as_str())))
            }
            AgentEvent::Log { message } => println!("{}", dim(&message)),
        }
        let _ = std::io::stdout().flush();
    }
}

pub struct TerminalApprover {
    lock: Mutex<()>,
}

impl TerminalApprover {
    pub fn new() -> Self {
        Self {
            lock: Mutex::new(()),
        }
    }
}

#[async_trait]
impl Approver for TerminalApprover {
    async fn approve(&self, req: ApprovalRequest) -> ApprovalDecision {
        let _g = self.lock.lock().await;
        if !std::io::stdin().is_terminal() {
            eprintln!(
                "{} {} negado (entrada não interativa): {}",
                yellow("⚠"),
                req.tool,
                req.summary
            );
            return ApprovalDecision::Deny;
        }
        let risk = match req.risk {
            Risk::Admin | Risk::Destructive => red(req.risk.label()),
            _ => yellow(req.risk.label()),
        };
        println!(
            "\n{} {} ({risk}) — {}",
            bold("Aprovação necessária:"),
            bold(&req.tool),
            req.reason
        );
        println!("  {}", req.summary);
        let args = serde_json::to_string_pretty(&req.args).unwrap_or_default();
        if args.len() < 1500 {
            println!("{}", dim(&args));
        }
        let opts = if req.allow_session {
            "[s]im / [n]ão / [t]odas desta ferramenta na sessão"
        } else {
            "[s]im / [n]ão"
        };
        print!("  {opts}: ");
        let _ = std::io::stdout().flush();
        let line = tokio::task::spawn_blocking(|| {
            let mut s = String::new();
            std::io::stdin().read_line(&mut s).ok();
            s
        })
        .await
        .unwrap_or_default();
        match line.trim().to_lowercase().as_str() {
            "s" | "sim" | "y" | "yes" => ApprovalDecision::AllowOnce,
            "t" | "todas" | "a" | "all" if req.allow_session => ApprovalDecision::AllowSession,
            _ => ApprovalDecision::Deny,
        }
    }
}
