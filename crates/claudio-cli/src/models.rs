//! `claudio models …` and `claudio llama-install`: configure API and local models.

use std::io::{IsTerminal, Write};

use anyhow::{bail, Context, Result};
use claudio_core::secrets::{mask, secrets_path_for, Secrets};
use claudio_llm::catalog::{api_model_config, local_model, preset, API_PRESETS, LOCAL_MODELS};
use claudio_llm::{ChatRequest, Message};
use claudio_tools::downloads::{self, LlamaVariant};
use tokio_util::sync::CancellationToken;

use crate::commands::{approver, load_config};
use crate::term::*;
use crate::{Cli, ModelsCmd};

/// Draw a one-line progress bar while a download runs.
async fn watch(
    progress: downloads::ProgressHandle,
    task: tokio::task::JoinHandle<Result<std::path::PathBuf>>,
) -> Result<std::path::PathBuf> {
    let tty = std::io::stderr().is_terminal();
    let mut last_stage = String::new();
    while !task.is_finished() {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let p = progress.lock().unwrap().clone();
        if p.stage != last_stage {
            if tty {
                eprintln!();
            }
            eprintln!("{}", dim(&p.stage));
            last_stage = p.stage.clone();
        }
        if tty {
            let mb = p.downloaded as f64 / 1_048_576.0;
            match p.total {
                Some(t) => eprint!(
                    "\r\x1b[2K  {:5.1}%  {mb:.0} / {:.0} MB",
                    p.fraction().unwrap_or(0.0) * 100.0,
                    t as f64 / 1_048_576.0
                ),
                None => eprint!("\r\x1b[2K  {mb:.0} MB"),
            }
            let _ = std::io::stderr().flush();
        }
    }
    if tty {
        eprintln!();
    }
    task.await?
}

fn ctrlc() -> CancellationToken {
    let t = CancellationToken::new();
    let t2 = t.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            t2.cancel();
        }
    });
    t
}

fn read_key(label: &str) -> Result<String> {
    if std::io::stdin().is_terminal() {
        Ok(rpassword::prompt_password(format!(
            "Chave de API para {label} (não aparece na tela): "
        ))?)
    } else {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        Ok(s)
    }
}

pub async fn models_cmd(cli: &Cli, cmd: Option<&ModelsCmd>) -> Result<i32> {
    match cmd {
        None | Some(ModelsCmd::List) => crate::commands::models(cli).await,
        Some(ModelsCmd::Catalog) => {
            let res = claudio_llm::router::detect_resources();
            println!("{}", dim(&format!("RAM total: {} MB", res.mem_total_mb)));
            for m in LOCAL_MODELS {
                let state = if downloads::is_downloaded(m) {
                    green("baixado")
                } else {
                    dim("—")
                };
                let fit = if m.fits(res.mem_total_mb) {
                    green("cabe")
                } else {
                    red("pouca RAM")
                };
                println!(
                    "{:<17} {:>6.1} GB  RAM≥{:>2} GB  {fit:<10} {state}  {}",
                    bold(m.id),
                    m.size_mb as f64 / 1024.0,
                    m.min_ram_mb / 1000,
                    m.name
                );
            }
            println!("\nBaixe com: claudio models download <id>");
            Ok(0)
        }
        Some(ModelsCmd::Download { id_or_url, no_use }) => {
            let mut cfg = load_config(cli)?;
            let (url, dest, entry) = if let Some(m) = local_model(id_or_url) {
                let res = claudio_llm::router::detect_resources();
                if !m.fits(res.mem_total_mb) {
                    eprintln!(
                        "{} {} pede ~{} GB de RAM; este computador tem {} GB",
                        yellow("⚠"),
                        m.name,
                        m.min_ram_mb / 1000,
                        res.mem_total_mb / 1000
                    );
                }
                let dest = downloads::model_path(m);
                (
                    downloads::model_url(m),
                    dest.clone(),
                    m.model_config(&dest.to_string_lossy()),
                )
            } else if id_or_url.starts_with("https://") {
                let name = downloads::file_name_from_url(id_or_url)?;
                let dest = claudio_core::config::models_dir().join(name);
                (
                    id_or_url.clone(),
                    dest.clone(),
                    downloads::custom_gguf_config(&dest),
                )
            } else {
                bail!("'{id_or_url}' não está no catálogo (veja `claudio models catalog`) nem é uma URL https");
            };
            if dest.exists() {
                println!("já baixado: {}", dest.display());
            } else {
                let progress = downloads::new_progress();
                let (p2, c) = (progress.clone(), ctrlc());
                let task =
                    tokio::spawn(async move { downloads::download(&url, &dest, &p2, &c).await });
                let path = watch(progress, task).await?;
                println!("{} {}", green("baixado:"), path.display());
            }
            if !no_use {
                downloads::use_local_model(&mut cfg, entry);
                let p = cfg.save()?;
                println!("modelo local configurado como padrão ({})", p.display());
                if claudio_llm::local::find_binary(
                    cfg.local
                        .llama_server
                        .as_deref()
                        .map(claudio_core::config::expand)
                        .as_deref(),
                )
                .is_none()
                {
                    println!(
                        "{} falta o llama-server: rode `claudio llama-install`",
                        yellow("⚠")
                    );
                }
            }
            Ok(0)
        }
        Some(ModelsCmd::UseFile { path }) => {
            let path = claudio_llm::platform::canonicalize(path)
                .with_context(|| format!("{} não existe", path.display()))?;
            if !path.to_string_lossy().to_lowercase().ends_with(".gguf") {
                bail!("o arquivo deve ser .gguf");
            }
            let mut cfg = load_config(cli)?;
            downloads::use_local_model(&mut cfg, downloads::custom_gguf_config(&path));
            cfg.save()?;
            println!("modelo local: {}", path.display());
            Ok(0)
        }
        Some(ModelsCmd::Providers) => {
            for p in API_PRESETS {
                println!(
                    "{:<11} {:<30} {}",
                    bold(p.id),
                    p.label,
                    dim(&p.models.join(", "))
                );
                if !p.key_url.is_empty() {
                    println!("{:<11} {}", "", dim(&format!("chave: {}", p.key_url)));
                }
            }
            Ok(0)
        }
        Some(ModelsCmd::AddApi {
            provider,
            name,
            model,
            base_url,
            default,
        }) => {
            let pr = preset(provider).with_context(|| {
                format!("provedor desconhecido: {provider} (veja `claudio models providers`)")
            })?;
            let model = model
                .clone()
                .or_else(|| pr.models.first().map(|s| s.to_string()))
                .context("informe --model")?;
            let name = name.clone().unwrap_or_else(|| pr.id.to_string());
            if pr.id == "custom" && base_url.is_none() {
                bail!("para 'custom' informe --base-url");
            }
            let mut cfg = load_config(cli)?;
            let entry = api_model_config(pr, &name, &model, base_url.as_deref());
            match cfg.models.iter_mut().find(|m| m.name == name) {
                Some(m) => *m = entry,
                None => cfg.models.push(entry),
            }
            if pr.needs_key {
                let key = read_key(pr.label)?;
                if key.trim().is_empty() {
                    bail!("nenhuma chave informada");
                }
                let path = secrets_path_for(&cfg);
                let mut s = Secrets::load_from(&path)?;
                s.set(&name, &key);
                s.save_to(&path)?;
                println!("chave {} salva em {}", mask(key.trim()), path.display());
            }
            if *default {
                cfg.default_model = Some(name.clone());
            }
            cfg.save()?;
            println!(
                "{} modelo '{name}' ({}: {model})",
                green("adicionado:"),
                pr.label
            );
            Ok(0)
        }
        Some(ModelsCmd::Remove { name }) => {
            let mut cfg = load_config(cli)?;
            let before = cfg.models.len();
            cfg.models.retain(|m| &m.name != name);
            if cfg.models.len() == before {
                bail!("modelo '{name}' não existe");
            }
            if cfg.default_model.as_deref() == Some(name.as_str()) {
                cfg.default_model = cfg.models.first().map(|m| m.name.clone());
            }
            let path = secrets_path_for(&cfg);
            let mut s = Secrets::load_from(&path).unwrap_or_default();
            if s.keys.remove(name).is_some() {
                s.save_to(&path)?;
            }
            cfg.save()?;
            println!("modelo '{name}' removido");
            Ok(0)
        }
        Some(ModelsCmd::Use { name }) => {
            let mut cfg = load_config(cli)?;
            if cfg.model(name).is_none() {
                bail!("modelo '{name}' não existe");
            }
            cfg.default_model = Some(name.clone());
            cfg.save()?;
            println!("modelo padrão: {name}");
            Ok(0)
        }
        Some(ModelsCmd::Test { name }) => {
            let cfg = load_config(cli)?;
            let m = cfg
                .model(name)
                .with_context(|| format!("modelo '{name}' não existe"))?
                .clone();
            let rt = claudio_tools::runtime::Runtime::init(cfg).await?;
            let sel = rt
                .select_model(Some(name), "teste", approver(cli, None).as_ref())
                .await?;
            let t0 = std::time::Instant::now();
            let resp = sel
                .provider
                .chat(ChatRequest {
                    messages: vec![Message::user("Responda apenas: OK")],
                    tools: vec![],
                    max_tokens: 16,
                    temperature: Some(0.0),
                })
                .await;
            rt.shutdown().await;
            match resp {
                Ok(r) => {
                    println!(
                        "{} {} respondeu em {} ms: {}",
                        green("ok:"),
                        m.name,
                        t0.elapsed().as_millis(),
                        r.message.text_content().trim()
                    );
                    Ok(0)
                }
                Err(e) => {
                    println!("{} {e:#}", red("falhou:"));
                    Ok(1)
                }
            }
        }
    }
}

pub async fn llama_install(cli: &Cli, variant: &str) -> Result<i32> {
    let v = match variant {
        "cpu" => LlamaVariant::Cpu,
        "vulkan" | "gpu" => LlamaVariant::Vulkan,
        other => bail!("variante inválida: {other} (use cpu ou vulkan)"),
    };
    let mut cfg = load_config(cli)?;
    let api = downloads::llama_releases_api();
    let progress = downloads::new_progress();
    let (p2, c) = (progress.clone(), ctrlc());
    let task = tokio::spawn(async move { downloads::install_llama_cpp(&api, v, &p2, &c).await });
    let server = watch(progress, task).await?;
    cfg.local.llama_server = Some(server.to_string_lossy().into_owned());
    cfg.save()?;
    println!("{} {}", green("llama-server instalado:"), server.display());
    Ok(0)
}

pub async fn secret_cmd(cli: &Cli, cmd: &crate::SecretCmd) -> Result<i32> {
    use claudio_tools::api::{check_secret_name, SECRET_PREFIX};
    let cfg = load_config(cli)?;
    let path = claudio_core::secrets::secrets_path_for(&cfg);
    let mut store = Secrets::load_from(&path)?;
    match cmd {
        crate::SecretCmd::Set { name } => {
            check_secret_name(name)?;
            let v = read_key(name)?;
            if v.trim().is_empty() {
                bail!("valor vazio");
            }
            store.set(&format!("{SECRET_PREFIX}{name}"), &v);
            store.save_to(&path)?;
            println!(
                "{} guardada. O agente pode usar {{{{secret:{name}}}}} em http_request.",
                green(name)
            );
        }
        crate::SecretCmd::List => {
            for k in store
                .keys
                .keys()
                .filter_map(|k| k.strip_prefix(SECRET_PREFIX))
            {
                println!("{k}");
            }
        }
        crate::SecretCmd::Remove { name } => {
            store.keys.remove(&format!("{SECRET_PREFIX}{name}"));
            store.save_to(&path)?;
            println!("{name} removida");
        }
    }
    Ok(0)
}
