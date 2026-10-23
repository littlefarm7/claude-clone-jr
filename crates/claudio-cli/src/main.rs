//! `claudio` — command-line interface for Claudio Code.

mod commands;
mod models;
mod term;

use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "claudio",
    version,
    about = "Claudio Code — agente autônomo local-first"
)]
pub struct Cli {
    /// Modelo a usar (nome em [[models]] da configuração)
    #[arg(long, short, global = true)]
    pub model: Option<String>,
    /// Aprova escrita/execução/rede automaticamente (nunca ações destrutivas ou administrativas)
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
        /// Roda em segundo plano (log em ~/.local/share/claudio/logs)
        #[arg(long)]
        detach: bool,
    },
    /// Executa um workflow TOML
    Workflow {
        file: std::path::PathBuf,
        /// Variáveis extras: nome=valor
        #[arg(long = "var")]
        vars: Vec<String>,
    },
    /// Agendamentos (executados pelo `claudio daemon`)
    Schedule {
        #[command(subcommand)]
        cmd: ScheduleCmd,
    },
    /// Executa agendamentos em primeiro plano (sem abrir portas de rede)
    Daemon {
        /// Verifica uma vez e sai
        #[arg(long)]
        once: bool,
    },
    /// Voz local
    Voice {
        #[command(subcommand)]
        cmd: VoiceCmd,
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
    Models {
        #[command(subcommand)]
        cmd: Option<ModelsCmd>,
    },
    /// Instala o llama.cpp (llama-server) oficial para rodar modelos locais
    LlamaInstall {
        /// cpu ou vulkan (GPU AMD/Intel/NVIDIA)
        #[arg(long, default_value = "cpu")]
        variant: String,
    },
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
    /// Chaves de serviços externos usadas pelo agente como {{secret:NOME}}
    Secret {
        #[command(subcommand)]
        cmd: SecretCmd,
    },
}

#[derive(Subcommand)]
pub enum SecretCmd {
    /// Guarda uma chave (pedida sem aparecer na tela)
    Set { name: String },
    /// Lista os nomes (nunca os valores)
    List,
    /// Remove uma chave
    Remove { name: String },
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
    Search {
        query: Vec<String>,
    },
    Delete {
        id: i64,
    },
    Clear {
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum ModelsCmd {
    /// Lista os modelos configurados (padrão)
    List,
    /// Catálogo de modelos locais recomendados para baixar
    Catalog,
    /// Baixa um modelo do catálogo (id) ou de uma URL .gguf e passa a usá-lo
    Download {
        id_or_url: String,
        /// Só baixa, sem trocar o modelo padrão
        #[arg(long)]
        no_use: bool,
    },
    /// Usa um arquivo .gguf que já está no computador
    UseFile { path: std::path::PathBuf },
    /// Lista os provedores de API disponíveis
    Providers,
    /// Adiciona um modelo por API. A chave é lida do terminal (não aparece) ou da entrada padrão
    AddApi {
        /// anthropic, openai, gemini, openrouter, groq, ollama, lmstudio, custom
        #[arg(long)]
        provider: String,
        /// Nome do modelo no Claudio (ex.: claude)
        #[arg(long)]
        name: Option<String>,
        /// Modelo do provedor (ex.: claude-sonnet-5-5)
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
        /// Torna este o modelo padrão
        #[arg(long)]
        default: bool,
    },
    /// Remove um modelo da configuração (e sua chave salva)
    Remove { name: String },
    /// Define o modelo padrão
    Use { name: String },
    /// Envia uma mensagem curta para conferir se o modelo responde
    Test { name: String },
}

#[derive(Subcommand)]
pub enum ScheduleCmd {
    /// Adiciona: --cron "0 8 * * 1-5" e --task "..." ou --workflow arquivo.toml
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        cron: String,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        workflow: Option<std::path::PathBuf>,
        /// Ferramentas pré-aprovadas (separadas por vírgula)
        #[arg(long, default_value = "")]
        allow: String,
        /// Executa também no próximo ciclo do daemon
        #[arg(long)]
        now: bool,
    },
    List,
    Delete {
        id: i64,
    },
}

#[derive(Subcommand)]
pub enum VoiceCmd {
    /// Transcreve um arquivo de áudio/vídeo
    Transcribe {
        file: std::path::PathBuf,
        #[arg(long, default_value = "pt")]
        lang: String,
    },
    /// Fala um texto (gera WAV e toca com aplay/paplay se disponível)
    Say {
        text: Vec<String>,
        #[arg(long, default_value = "fala.wav")]
        out: std::path::PathBuf,
    },
    /// Ouve uma frase do microfone (para sozinho na pausa), transcreve e executa como tarefa
    Listen,
    /// Conversa por voz: ouve, responde falando e volta a ouvir (Ctrl+C sai)
    Chat,
    /// Instala o reconhecimento de voz e a voz em português (uma vez)
    Setup {
        /// Usa a sua conta na nuvem (OpenAI, Groq ou Gemini) para entender a voz
        #[arg(long)]
        cloud: bool,
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
    /// Cria ~/.config/claudio/config.toml com valores padrão
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
    // Behave like a normal Unix tool when piped into `head`.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("CLAUDIO_LOG")
                .unwrap_or_else(|_| "warn".into()),
        )
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
        Some(Cmd::Run {
            task,
            allow,
            detach,
        }) => {
            if *detach {
                commands::detach()
            } else {
                commands::run_task(cli.clone(), task.join(" "), allow.clone()).await
            }
        }
        Some(Cmd::Workflow { file, vars }) => commands::workflow(cli.clone(), file, vars).await,
        Some(Cmd::Schedule { cmd }) => commands::schedule(&cli, cmd).await,
        Some(Cmd::Daemon { once }) => commands::daemon(cli.clone(), *once).await,
        Some(Cmd::Voice { cmd }) => commands::voice(cli.clone(), cmd).await,
        Some(Cmd::Resume { id }) => commands::resume(cli.clone(), id).await,
        Some(Cmd::Sessions { limit }) => commands::sessions(&cli, *limit).await,
        Some(Cmd::Show { id }) => commands::show(&cli, id).await,
        Some(Cmd::History { query }) => commands::history(&cli, &query.join(" ")).await,
        Some(Cmd::Memory { cmd }) => commands::memory(&cli, cmd).await,
        Some(Cmd::Models { cmd }) => models::models_cmd(&cli, cmd.as_ref()).await,
        Some(Cmd::LlamaInstall { variant }) => models::llama_install(&cli, variant).await,
        Some(Cmd::Serve {
            gguf,
            port,
            gpu_layers,
            ctx,
        }) => commands::serve(&cli, gguf, *port, *gpu_layers, *ctx).await,
        Some(Cmd::Tools) => commands::tools(&cli).await,
        Some(Cmd::Audit { cmd }) => commands::audit(cmd).await,
        Some(Cmd::Config { cmd }) => commands::config(&cli, cmd).await,
        Some(Cmd::Consent { cmd }) => commands::consent(&cli, cmd).await,
        Some(Cmd::Doctor) => commands::doctor(&cli).await,
        Some(Cmd::Secret { cmd }) => models::secret_cmd(&cli, cmd).await,
    }
}
