//! `jarvis` — command-line interface for Jarvis Core.

mod commands;
mod term;

use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "jarvis", version, about = "Jarvis Core — agente autônomo local-first")]
pub struct Cli {
    /// Modelo a usar (nome em [[models]] da configuração)
    #[arg(long, short, global = true)]
    pub model: Option<String>,
    /// Aprova automaticamente ações que pedem confirmação (exceto administrativas)
    #[arg(long, global = true)]
    pub yes: bool,
    /// Nunca pergunta: nega tudo que exigir aprovação
    #[arg(long, global = true)]
    pub non_interactive: bool,
    /// Eventos em JSON Lines (para scripts)
    #[arg(long, global = true)]
    pub json: bool,
    /// Arquivo de configuração alternativo
    #[arg(long, global = true)]
    pub config: Option<std::path::PathBuf>,
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Conversa interativa (padrão)
    Chat {
        /// Continua uma sessão existente
        #[arg(long)]
        session: Option<String>,
    },
    /// Executa uma tarefa até concluir
    Run {
        task: Vec<String>,
        /// Ferramentas pré-aprovadas no modo não interativo (separadas por vírgula)
        #[arg(long)]
        allow: Option<String>,
    },
    /// Retoma uma sessão interrompida
    Resume { id: String },
    /// Lista sessões
    Sessions {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Mostra uma sessão (plano e mensagens)
    Show { id: String },
    /// Pesquisa no histórico
    History { query: Vec<String> },
    /// Memória persistente
    Memory {
        #[command(subcommand)]
        cmd: MemoryCmd,
    },
    /// Modelos configurados e disponibilidade
    Models,
    /// Inicia o llama-server com um GGUF (fica em primeiro plano)
    Serve {
        gguf: std::path::PathBuf,
        #[arg(long, default_value_t = 9931)]
        port: u16,
        #[arg(long, default_value_t = 0)]
        gpu_layers: u32,
        #[arg(long, default_value_t = 8192)]
        ctx: u32,
    },
    /// Lista ferramentas disponíveis
    Tools,
    /// Auditoria
    Audit {
        #[command(subcommand)]
        cmd: AuditCmd,
    },
    /// Configuração
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Consentimentos para provedores remotos
    Consent {
        #[command(subcommand)]
        cmd: ConsentCmd,
    },
    /// Verifica o ambiente (dependências, modelos, permissões)
    Doctor,
}

#[derive(Subcommand)]
pub enum MemoryCmd {
    List,
    Add {
        content: Vec<String>,
        #[arg(long, default_value = "note")]
        kind: String,
        #[arg(long, default_value = "")]
        tags: String,
    },
    Search { query: Vec<String> },
    Delete { id: i64 },
    Clear {
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum AuditCmd {
    Verify,
    Tail {
        #[arg(default_value_t = 20)]
        n: usize,
    },
}

#[derive(Subcommand)]
pub enum ConfigCmd {
    /// Cria ~/.config/jarvis/config.toml com valores padrão
    Init,
    Path,
    Show,
}

#[derive(Subcommand)]
pub enum ConsentCmd {
    List,
    Revoke { key: String },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("JARVIS_LOG").unwrap_or_else(|_| "warn".into()))
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let code = match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{} {e:#}", term::red("erro:"));
            2
        }
    };
    std::process::exit(code);
}

async fn run(cli: Cli) -> Result<i32> {
    let cli = Arc::new(cli);
    match &cli.cmd {
        None => commands::chat(cli.clone(), None).await,
        Some(Cmd::Chat { session }) => commands::chat(cli.clone(), session.clone()).await,
        Some(Cmd::Run { task, allow }) => commands::run_task(cli.clone(), task.join(" "), allow.clone()).await,
        Some(Cmd::Resume { id }) => commands::resume(cli.clone(), id).await,
        Some(Cmd::Sessions { limit }) => commands::sessions(&cli, *limit).await,
        Some(Cmd::Show { id }) => commands::show(&cli, id).await,
        Some(Cmd::History { query }) => commands::history(&cli, &query.join(" ")).await,
        Some(Cmd::Memory { cmd }) => commands::memory(&cli, cmd).await,
        Some(Cmd::Models) => commands::models(&cli).await,
        Some(Cmd::Serve { gguf, port, gpu_layers, ctx }) => commands::serve(&cli, gguf, *port, *gpu_layers, *ctx).await,
        Some(Cmd::Tools) => commands::tools(&cli).await,
        Some(Cmd::Audit { cmd }) => commands::audit(cmd).await,
        Some(Cmd::Config { cmd }) => commands::config(&cli, cmd).await,
        Some(Cmd::Consent { cmd }) => commands::consent(&cli, cmd).await,
        Some(Cmd::Doctor) => commands::doctor(&cli).await,
    }
}
