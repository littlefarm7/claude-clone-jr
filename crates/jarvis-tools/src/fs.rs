//! File and directory tools.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use jarvis_core::permissions::{PathVerdict, Risk, RiskAssessment};
use jarvis_core::tool::{arg_bool, arg_str, arg_u64, Tool, ToolContext, ToolOutput, ToolRegistry};
use jarvis_llm::ToolSpec;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub fn register(r: &mut ToolRegistry) {
    r.register(Arc::new(ReadFile));
    r.register(Arc::new(WriteFile));
    r.register(Arc::new(EditFile));
    r.register(Arc::new(ListDir));
    r.register(Arc::new(Glob));
    r.register(Arc::new(Grep));
    r.register(Arc::new(DeletePath));
    r.register(Arc::new(MovePath));
}

fn spec(name: &str, desc: &str, params: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: desc.into(),
        parameters: params,
    }
}

fn path_arg(args: &Value, key: &str, ctx: &ToolContext) -> PathBuf {
    ctx.guard.resolve(arg_str(args, key).unwrap_or("."))
}

pub fn sha256_file(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(std::fs::read(p)?)))
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|b| *b == 0)
}

/// Outside-workspace reads return untrusted content.
fn mark_source(out: ToolOutput, p: &Path, ctx: &ToolContext) -> ToolOutput {
    match ctx.guard.classify(p) {
        PathVerdict::Inside => out,
        _ => out.untrusted(format!("file:{}", p.display())),
    }
}

// ---------------------------------------------------------------------------

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        spec(
            "read_file",
            "Lê um arquivo de texto e devolve as linhas numeradas. Para PDF/DOCX/XLSX use doc_read.",
            json!({"type":"object","properties":{
                "path":{"type":"string"},
                "offset":{"type":"integer","description":"linha inicial (1 = primeira)"},
                "limit":{"type":"integer","description":"máximo de linhas (padrão 2000)"}
            },"required":["path"]}),
        )
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let p = path_arg(args, "path", ctx);
        ctx.guard.assess_read(&p, format!("ler {}", p.display()))
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let p = path_arg(&args, "path", ctx);
        let bytes = tokio::fs::read(&p)
            .await
            .with_context(|| format!("não foi possível ler {}", p.display()))?;
        if is_binary(&bytes) {
            return Ok(ToolOutput::err(format!(
                "{} parece binário ({} bytes). Use doc_read, media_info ou image_edit.",
                p.display(),
                bytes.len()
            )));
        }
        let text = String::from_utf8_lossy(&bytes);
        let offset = arg_u64(&args, "offset").unwrap_or(1).max(1) as usize;
        let limit = arg_u64(&args, "limit").unwrap_or(2000) as usize;
        let total = text.lines().count();
        let mut out = String::new();
        for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
            out.push_str(&format!("{:>6}\t{}\n", i + 1, line));
        }
        if offset - 1 + limit < total {
            out.push_str(&format!(
                "[... {} linhas restantes; use offset para continuar]\n",
                total - (offset - 1 + limit)
            ));
        }
        if out.is_empty() {
            out = "(arquivo vazio)".into();
        }
        Ok(mark_source(ToolOutput::ok(out), &p, ctx))
    }
}

// ---------------------------------------------------------------------------

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn spec(&self) -> ToolSpec {
        spec(
            "write_file",
            "Cria ou sobrescreve um arquivo de texto (cria pastas se preciso). Verifica a gravação relendo o arquivo.",
            json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}),
        )
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let p = path_arg(args, "path", ctx);
        let verb = if p.exists() { "sobrescrever" } else { "criar" };
        let n = arg_str(args, "content").map(|c| c.len()).unwrap_or(0);
        ctx.guard.assess_write(
            &p,
            Risk::Write,
            format!("{verb} {} ({n} bytes)", p.display()),
        )
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let p = path_arg(&args, "path", ctx);
        let content = arg_str(&args, "content").unwrap_or("");
        if let Some(dir) = p.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let existed = p.exists();
        tokio::fs::write(&p, content)
            .await
            .with_context(|| format!("escrevendo {}", p.display()))?;
        let expected = hex::encode(Sha256::digest(content.as_bytes()));
        let actual = sha256_file(&p)?;
        if expected != actual {
            bail!(
                "verificação falhou: o conteúdo gravado em {} difere do esperado",
                p.display()
            );
        }
        Ok(ToolOutput::ok(format!(
            "{} {} ({} bytes, {} linhas). Verificado (sha256 {}).",
            if existed { "Sobrescrito" } else { "Criado" },
            p.display(),
            content.len(),
            content.lines().count(),
            &actual[..12]
        ))
        .with_artifact(p))
    }
}

// ---------------------------------------------------------------------------

pub struct EditFile;

#[async_trait]
impl Tool for EditFile {
    fn spec(&self) -> ToolSpec {
        spec(
            "edit_file",
            "Substitui um trecho exato de um arquivo. old_text deve aparecer exatamente uma vez (ou use replace_all).",
            json!({"type":"object","properties":{
                "path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"},
                "replace_all":{"type":"boolean"}
            },"required":["path","old_text","new_text"]}),
        )
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let p = path_arg(args, "path", ctx);
        ctx.guard
            .assess_write(&p, Risk::Write, format!("editar {}", p.display()))
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let p = path_arg(&args, "path", ctx);
        let old = arg_str(&args, "old_text").unwrap_or("");
        let new = arg_str(&args, "new_text").unwrap_or("");
        if old.is_empty() {
            return Ok(ToolOutput::err(
                "old_text não pode ser vazio (use write_file para arquivos novos)",
            ));
        }
        let text = tokio::fs::read_to_string(&p)
            .await
            .with_context(|| format!("lendo {}", p.display()))?;
        let count = text.matches(old).count();
        let all = arg_bool(&args, "replace_all").unwrap_or(false);
        if count == 0 {
            return Ok(ToolOutput::err("old_text não encontrado. Leia o arquivo de novo e copie o trecho exato (incluindo espaços)."));
        }
        if count > 1 && !all {
            return Ok(ToolOutput::err(format!(
                "old_text aparece {count} vezes; inclua mais contexto ou use replace_all=true"
            )));
        }
        let updated = if all {
            text.replace(old, new)
        } else {
            text.replacen(old, new, 1)
        };
        tokio::fs::write(&p, &updated).await?;
        let check = tokio::fs::read_to_string(&p).await?;
        if check != updated || (!new.is_empty() && !check.contains(new)) {
            bail!("verificação falhou após editar {}", p.display());
        }
        Ok(ToolOutput::ok(format!(
            "Editado {} ({} substituição(ões)). Verificado.",
            p.display(),
            if all { count } else { 1 }
        ))
        .with_artifact(p))
    }
}

// ---------------------------------------------------------------------------

pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn spec(&self) -> ToolSpec {
        spec(
            "list_dir",
            "Lista o conteúdo de um diretório (tipo, tamanho, nome).",
            json!({"type":"object","properties":{"path":{"type":"string"},"hidden":{"type":"boolean"}}}),
        )
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let p = path_arg(args, "path", ctx);
        ctx.guard.assess_read(&p, format!("listar {}", p.display()))
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let p = path_arg(&args, "path", ctx);
        let hidden = arg_bool(&args, "hidden").unwrap_or(false);
        let mut entries = Vec::new();
        let mut rd = tokio::fs::read_dir(&p)
            .await
            .with_context(|| format!("listando {}", p.display()))?;
        while let Some(e) = rd.next_entry().await? {
            let name = e.file_name().to_string_lossy().to_string();
            if !hidden && name.starts_with('.') {
                continue;
            }
            let meta = e.metadata().await.ok();
            let (kind, size) = match &meta {
                Some(m) if m.is_dir() => ("dir ", String::from("-")),
                Some(m) => ("file", human_size(m.len())),
                None => ("?   ", String::new()),
            };
            entries.push(format!("{kind} {size:>9}  {name}"));
        }
        entries.sort();
        if entries.len() > 1000 {
            let n = entries.len();
            entries.truncate(1000);
            entries.push(format!("[... {} entradas omitidas]", n - 1000));
        }
        Ok(ToolOutput::ok(format!(
            "{}:\n{}",
            p.display(),
            if entries.is_empty() {
                "(vazio)".into()
            } else {
                entries.join("\n")
            }
        )))
    }
}

pub fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n}B")
    } else {
        format!("{v:.1}{}", U[i])
    }
}

// ---------------------------------------------------------------------------

pub struct Glob;

#[async_trait]
impl Tool for Glob {
    fn spec(&self) -> ToolSpec {
        spec(
            "glob",
            "Encontra arquivos por padrão glob (ex.: **/*.pdf), respeitando .gitignore.",
            json!({"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern"]}),
        )
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let p = path_arg(args, "path", ctx);
        ctx.guard.assess_read(
            &p,
            format!(
                "procurar {} em {}",
                arg_str(args, "pattern").unwrap_or(""),
                p.display()
            ),
        )
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let root = path_arg(&args, "path", ctx);
        let pattern = arg_str(&args, "pattern").unwrap_or("*").to_string();
        let res = tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            let matcher = globset::GlobBuilder::new(&pattern)
                .literal_separator(false)
                .build()?
                .compile_matcher();
            let mut out = Vec::new();
            for e in ignore::WalkBuilder::new(&root)
                .hidden(true)
                .build()
                .flatten()
            {
                let path = e.path();
                let rel = path.strip_prefix(&root).unwrap_or(path);
                if e.file_type().map(|t| t.is_file()).unwrap_or(false)
                    && (matcher.is_match(rel) || matcher.is_match(path))
                {
                    out.push(path.display().to_string());
                    if out.len() >= 1000 {
                        break;
                    }
                }
            }
            Ok(out)
        })
        .await??;
        Ok(ToolOutput::ok(if res.is_empty() {
            "Nenhum arquivo encontrado.".into()
        } else {
            format!("{} arquivo(s):\n{}", res.len(), res.join("\n"))
        }))
    }
}

// ---------------------------------------------------------------------------

pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn spec(&self) -> ToolSpec {
        spec(
            "grep",
            "Pesquisa conteúdo com expressão regular em arquivos de texto (respeita .gitignore).",
            json!({"type":"object","properties":{
                "pattern":{"type":"string"},"path":{"type":"string"},
                "glob":{"type":"string","description":"filtra arquivos, ex.: *.rs"},
                "ignore_case":{"type":"boolean"},"max_results":{"type":"integer"}
            },"required":["pattern"]}),
        )
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let p = path_arg(args, "path", ctx);
        ctx.guard.assess_read(
            &p,
            format!(
                "pesquisar '{}' em {}",
                arg_str(args, "pattern").unwrap_or(""),
                p.display()
            ),
        )
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let root = path_arg(&args, "path", ctx);
        let re = regex::RegexBuilder::new(arg_str(&args, "pattern").unwrap_or(""))
            .case_insensitive(arg_bool(&args, "ignore_case").unwrap_or(false))
            .build()?;
        let filter = match arg_str(&args, "glob") {
            Some(g) => Some(globset::Glob::new(g)?.compile_matcher()),
            None => None,
        };
        let max = arg_u64(&args, "max_results").unwrap_or(200) as usize;
        let guard = ctx.guard.clone();
        let res = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            for e in ignore::WalkBuilder::new(&root).build().flatten() {
                if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let path = e.path();
                if matches!(guard.classify(path), PathVerdict::Blocked(_)) {
                    continue;
                }
                if let Some(f) = &filter {
                    if !f.is_match(path.file_name().unwrap_or_default()) && !f.is_match(path) {
                        continue;
                    }
                }
                let Ok(bytes) = std::fs::read(path) else {
                    continue;
                };
                if is_binary(&bytes) {
                    continue;
                }
                let text = String::from_utf8_lossy(&bytes);
                for (i, line) in text.lines().enumerate() {
                    if re.is_match(line) {
                        let l: String = line.chars().take(300).collect();
                        out.push(format!("{}:{}: {}", path.display(), i + 1, l));
                        if out.len() >= max {
                            return out;
                        }
                    }
                }
            }
            out
        })
        .await?;
        Ok(ToolOutput::ok(if res.is_empty() {
            "Nenhuma ocorrência.".into()
        } else {
            format!("{} ocorrência(s):\n{}", res.len(), res.join("\n"))
        }))
    }
}

// ---------------------------------------------------------------------------

pub struct DeletePath;

/// Move to the freedesktop trash so deletions are reversible.
pub fn move_to_trash(p: &Path) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME não definido")?;
    let trash = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"))
        .join("Trash");
    let files = trash.join("files");
    let info = trash.join("info");
    std::fs::create_dir_all(&files)?;
    std::fs::create_dir_all(&info)?;
    let base = p
        .file_name()
        .context("caminho sem nome")?
        .to_string_lossy()
        .to_string();
    let mut name = base.clone();
    let mut i = 1;
    while files.join(&name).exists() || info.join(format!("{name}.trashinfo")).exists() {
        name = format!("{base}.{i}");
        i += 1;
    }
    let dest = files.join(&name);
    if std::fs::rename(p, &dest).is_err() {
        // Different filesystem: copy then remove.
        copy_recursive(p, &dest)?;
        if p.is_dir() {
            std::fs::remove_dir_all(p)?;
        } else {
            std::fs::remove_file(p)?;
        }
    }
    std::fs::write(
        info.join(format!("{name}.trashinfo")),
        format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            p.display(),
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S")
        ),
    )?;
    Ok(dest)
}

fn copy_recursive(from: &Path, to: &Path) -> Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_recursive(&e.path(), &to.join(e.file_name()))?;
        }
    } else {
        std::fs::copy(from, to)?;
    }
    Ok(())
}

#[async_trait]
impl Tool for DeletePath {
    fn spec(&self) -> ToolSpec {
        spec(
            "delete_path",
            "Move um arquivo ou pasta para a lixeira (reversível). Só dentro do workspace.",
            json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
        )
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let p = path_arg(args, "path", ctx);
        let ra = ctx.guard.assess_write(
            &p,
            Risk::Destructive,
            format!("mover para a lixeira: {}", p.display()),
        );
        if ctx.guard.workspace.iter().any(|w| w == &p) {
            return ra.deny("não é permitido apagar a raiz do workspace");
        }
        ra
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let p = path_arg(&args, "path", ctx);
        if !p.exists() {
            return Ok(ToolOutput::err(format!("{} não existe", p.display())));
        }
        let dest = move_to_trash(&p)?;
        if p.exists() {
            bail!("verificação falhou: {} ainda existe", p.display());
        }
        Ok(ToolOutput::ok(format!(
            "{} movido para a lixeira ({}).",
            p.display(),
            dest.display()
        )))
    }
}

// ---------------------------------------------------------------------------

pub struct MovePath;

#[async_trait]
impl Tool for MovePath {
    fn spec(&self) -> ToolSpec {
        spec(
            "move_path",
            "Move ou renomeia um arquivo/pasta dentro do workspace (copy=true para copiar).",
            json!({"type":"object","properties":{"from":{"type":"string"},"to":{"type":"string"},"copy":{"type":"boolean"}},"required":["from","to"]}),
        )
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let from = path_arg(args, "from", ctx);
        let to = path_arg(args, "to", ctx);
        let copy = arg_bool(args, "copy").unwrap_or(false);
        let risk = if to.exists() {
            Risk::Destructive
        } else {
            Risk::Write
        };
        let summary = format!(
            "{} {} → {}{}",
            if copy { "copiar" } else { "mover" },
            from.display(),
            to.display(),
            if to.exists() { " (sobrescreve)" } else { "" }
        );
        let src = if copy {
            ctx.guard.assess_read(&from, summary.clone())
        } else {
            ctx.guard.assess_write(&from, risk, summary.clone())
        };
        let dst = ctx.guard.assess_write(&to, risk, summary);
        match (src.force, dst.force) {
            (Some(jarvis_core::permissions::Forced::Deny(r)), _)
            | (_, Some(jarvis_core::permissions::Forced::Deny(r))) => dst_with(risk, r, true),
            (Some(jarvis_core::permissions::Forced::Ask(r)), _) => dst_with(risk, r, false),
            _ => RiskAssessment::new(risk, dst.summary),
        }
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let from = path_arg(&args, "from", ctx);
        let to = path_arg(&args, "to", ctx);
        if let Some(d) = to.parent() {
            std::fs::create_dir_all(d)?;
        }
        if arg_bool(&args, "copy").unwrap_or(false) {
            copy_recursive(&from, &to)?;
        } else if std::fs::rename(&from, &to).is_err() {
            copy_recursive(&from, &to)?;
            if from.is_dir() {
                std::fs::remove_dir_all(&from)?;
            } else {
                std::fs::remove_file(&from)?;
            }
        }
        if !to.exists() {
            bail!("verificação falhou: {} não existe", to.display());
        }
        Ok(
            ToolOutput::ok(format!("Feito: {} → {}", from.display(), to.display()))
                .with_artifact(to),
        )
    }
}

fn dst_with(risk: Risk, reason: String, deny: bool) -> RiskAssessment {
    let ra = RiskAssessment::new(risk, reason.clone());
    if deny {
        ra.deny(reason)
    } else {
        ra.ask(reason)
    }
}
