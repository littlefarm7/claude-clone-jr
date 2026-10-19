//! Documents: read and create PDF, DOCX, XLSX/ODS, CSV, PPTX, ODT, LaTeX.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use calamine::{open_workbook_auto, Data, Reader};
use claudio_core::permissions::{Risk, RiskAssessment};
use claudio_core::tool::{arg_str, arg_u64, Tool, ToolContext, ToolOutput, ToolRegistry};
use claudio_llm::ToolSpec;
use serde_json::{json, Value};

use crate::shell::which;

pub fn register(r: &mut ToolRegistry) {
    r.register(Arc::new(DocRead));
    r.register(Arc::new(DocCreate));
    r.register(Arc::new(DocConvert));
    r.register(Arc::new(LatexCompile));
}

fn ext(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

async fn run_quiet(program: &str, args: &[&str], timeout: u64) -> Result<std::process::Output> {
    let resolved = which(program).unwrap_or_else(|| PathBuf::from(program));
    let mut c = tokio::process::Command::new(resolved);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000);
    c.args(args)
        .env_clear()
        .envs(crate::shell::scrubbed_env())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(timeout), c.output())
        .await
        .with_context(|| format!("{program}: tempo esgotado"))?
        .with_context(|| format!("falha ao executar {program}"))
}

pub async fn pdf_text(p: &Path) -> Result<String> {
    if which("pdftotext").is_none() {
        // Pure-Rust fallback (Windows/macOS without poppler). Less faithful layout.
        let path = p.to_path_buf();
        let text = tokio::task::spawn_blocking(move || pdf_extract::extract_text(&path))
            .await?
            .map_err(|e| anyhow::anyhow!("não foi possível extrair o texto do PDF: {e} (instale poppler para melhor resultado)"))?;
        if text.trim().is_empty() {
            return Ok(
                "(PDF sem texto extraível — provavelmente digitalizado; use a ferramenta ocr)"
                    .into(),
            );
        }
        return Ok(text);
    }
    let out = run_quiet(
        "pdftotext",
        &["-layout", "-enc", "UTF-8", &p.to_string_lossy(), "-"],
        120,
    )
    .await?;
    if !out.status.success() {
        bail!("pdftotext falhou: {}", String::from_utf8_lossy(&out.stderr));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        return Ok(
            "(PDF sem texto extraível — provavelmente digitalizado; OCR requer tesseract)".into(),
        );
    }
    Ok(text)
}

fn cell(d: &Data) -> String {
    match d {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{}", *f as i64)
            } else {
                format!("{f}")
            }
        }
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => b.to_string(),
        Data::DateTime(dt) => dt.to_string(),
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("#ERR {e:?}"),
    }
}

pub fn spreadsheet_text(p: &Path, sheet: Option<&str>, max_rows: usize) -> Result<String> {
    let mut wb =
        open_workbook_auto(p).with_context(|| format!("abrindo planilha {}", p.display()))?;
    let names = wb.sheet_names().to_vec();
    let mut out = format!("Planilhas: {}\n", names.join(", "));
    for name in names
        .iter()
        .filter(|n| sheet.map(|s| s == n.as_str()).unwrap_or(true))
    {
        let range = wb.worksheet_range(name)?;
        let (rows, cols) = range.get_size();
        out.push_str(&format!("\n## {name} ({rows} linhas × {cols} colunas)\n"));
        for row in range.rows().take(max_rows) {
            out.push_str(&row.iter().map(cell).collect::<Vec<_>>().join("\t"));
            out.push('\n');
        }
        if rows > max_rows {
            out.push_str(&format!("[... {} linhas omitidas]\n", rows - max_rows));
        }
    }
    Ok(out)
}

pub fn csv_text(p: &Path, max_rows: usize) -> Result<String> {
    let data = std::fs::read(p)?;
    let delim = {
        let head = String::from_utf8_lossy(&data[..data.len().min(4096)]).into_owned();
        (*b",;\t|")
            .into_iter()
            .max_by_key(|d| head.matches(*d as char).count())
            .unwrap_or(b',')
    };
    let mut r = csv::ReaderBuilder::new()
        .delimiter(delim)
        .flexible(true)
        .has_headers(false)
        .from_reader(data.as_slice());
    let mut out = String::new();
    let mut n = 0;
    let mut cols = 0;
    for rec in r.records() {
        let rec = rec?;
        cols = cols.max(rec.len());
        if n < max_rows {
            out.push_str(&rec.iter().collect::<Vec<_>>().join("\t"));
            out.push('\n');
        }
        n += 1;
    }
    Ok(format!(
        "CSV: {n} linhas × {cols} colunas (separador '{}')\n{out}{}",
        delim as char,
        if n > max_rows {
            format!("[... {} linhas omitidas]\n", n - max_rows)
        } else {
            String::new()
        }
    ))
}

/// Extract text from any supported document type.
pub async fn read_document(p: &Path, sheet: Option<&str>, max_rows: usize) -> Result<String> {
    match ext(p).as_str() {
        "pdf" => pdf_text(p).await,
        "docx" | "docm" | "dotx" => crate::ooxml::docx_text(p),
        "odt" => crate::ooxml::odt_text(p),
        "pptx" => crate::ooxml::pptx_text(p),
        "xlsx" | "xlsm" | "xls" | "xlsb" | "ods" => spreadsheet_text(p, sheet, max_rows),
        "csv" | "tsv" => csv_text(p, max_rows),
        "html" | "htm" => Ok(crate::web::html_to_text(&std::fs::read(p)?, 120)),
        "doc" | "ppt" | "rtf" | "odp" => {
            // Legacy formats: convert to text with LibreOffice when available.
            if which("soffice").is_none() {
                bail!(
                    "formato .{} requer LibreOffice (sudo apt install libreoffice)",
                    ext(p)
                );
            }
            let tmp = tempfile::tempdir()?;
            let out = run_quiet(
                "soffice",
                &[
                    "--headless",
                    "--convert-to",
                    "txt:Text",
                    "--outdir",
                    &tmp.path().to_string_lossy(),
                    &p.to_string_lossy(),
                ],
                180,
            )
            .await?;
            let txt = tmp
                .path()
                .join(p.with_extension("txt").file_name().unwrap());
            std::fs::read_to_string(&txt).with_context(|| {
                format!("conversão falhou: {}", String::from_utf8_lossy(&out.stderr))
            })
        }
        _ => {
            let bytes = std::fs::read(p)?;
            if bytes.iter().take(8192).any(|b| *b == 0) {
                bail!("formato .{} não suportado para leitura de texto", ext(p));
            }
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        }
    }
}

pub struct DocRead;

#[async_trait]
impl Tool for DocRead {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "doc_read".into(),
            description: "Extrai o texto de documentos: PDF, DOCX, ODT, PPTX, XLSX/XLS/ODS, CSV, HTML, TeX, MD, DOC/RTF (via LibreOffice).".into(),
            parameters: json!({"type":"object","properties":{
                "path":{"type":"string"},"sheet":{"type":"string"},"max_rows":{"type":"integer"},"max_chars":{"type":"integer"}
            },"required":["path"]}),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let p = ctx.guard.resolve(arg_str(args, "path").unwrap_or(""));
        ctx.guard
            .assess_read(&p, format!("ler documento {}", p.display()))
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let p = ctx.guard.resolve(arg_str(&args, "path").unwrap_or(""));
        let text = read_document(
            &p,
            arg_str(&args, "sheet"),
            arg_u64(&args, "max_rows").unwrap_or(200) as usize,
        )
        .await?;
        let max = arg_u64(&args, "max_chars").unwrap_or(30_000) as usize;
        // Documents often come from outside: treat their content as data.
        Ok(
            ToolOutput::ok(claudio_core::safety::truncate_middle(&text, max))
                .untrusted(format!("doc:{}", p.display())),
        )
    }
}

// ---------------------------------------------------------------------------

fn rows_arg(args: &Value) -> Option<Vec<Vec<Value>>> {
    args.get("rows").and_then(|r| r.as_array()).map(|rows| {
        rows.iter()
            .map(|r| r.as_array().cloned().unwrap_or_else(|| vec![r.clone()]))
            .collect()
    })
}

fn write_xlsx(p: &Path, rows: &[Vec<Value>], sheet: &str) -> Result<()> {
    use rust_xlsxwriter::{Format, Workbook};
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name(sheet)?;
    let bold = Format::new().set_bold();
    for (r, row) in rows.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            let (r, c) = (r as u32, c as u16);
            match v {
                Value::Number(n) => {
                    ws.write_number(r, c, n.as_f64().unwrap_or(0.0))?;
                }
                Value::Bool(b) => {
                    ws.write_boolean(r, c, *b)?;
                }
                Value::Null => {}
                Value::String(s) if s.starts_with('=') => {
                    ws.write_formula(r, c, s.as_str())?;
                }
                Value::String(s) => {
                    if r == 0 {
                        ws.write_string_with_format(r, c, s, &bold)?;
                    } else if let Ok(f) = s.replace(',', ".").parse::<f64>() {
                        if s.chars()
                            .all(|ch| ch.is_ascii_digit() || ch == ',' || ch == '.' || ch == '-')
                        {
                            ws.write_number(r, c, f)?;
                        } else {
                            ws.write_string(r, c, s)?;
                        }
                    } else {
                        ws.write_string(r, c, s)?;
                    }
                }
                other => {
                    ws.write_string(r, c, other.to_string())?;
                }
            }
        }
    }
    ws.autofit();
    wb.save(p)?;
    Ok(())
}

fn write_csv(p: &Path, rows: &[Vec<Value>]) -> Result<()> {
    let mut w = csv::Writer::from_path(p)?;
    for row in rows {
        w.write_record(row.iter().map(|v| match v {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        }))?;
    }
    w.flush()?;
    Ok(())
}

async fn pandoc_convert(input_md: &str, out: &Path, title: Option<&str>) -> Result<()> {
    let tmp = tempfile::Builder::new().suffix(".md").tempfile()?;
    let mut body = String::new();
    if let Some(t) = title {
        body.push_str(&format!("---\ntitle: \"{}\"\n---\n\n", t.replace('"', "'")));
    }
    body.push_str(input_md);
    std::fs::write(tmp.path(), body)?;
    let o = run_quiet(
        "pandoc",
        &[
            &tmp.path().to_string_lossy(),
            "-f",
            "markdown",
            "-o",
            &out.to_string_lossy(),
        ],
        120,
    )
    .await?;
    if !o.status.success() {
        bail!("pandoc falhou: {}", String::from_utf8_lossy(&o.stderr));
    }
    Ok(())
}

pub async fn soffice_convert(input: &Path, fmt: &str, outdir: &Path) -> Result<PathBuf> {
    if which("soffice").is_none() {
        bail!("LibreOffice não instalado (sudo apt install libreoffice)");
    }
    // A private profile avoids clashing with a running LibreOffice instance.
    let profile = tempfile::tempdir()?;
    let profile_url = format!(
        "-env:UserInstallation={}",
        url::Url::from_directory_path(profile.path())
            .map(|u| u.to_string())
            .unwrap_or_default()
    );
    let o = run_quiet(
        "soffice",
        &[
            &profile_url,
            "--headless",
            "--convert-to",
            fmt,
            "--outdir",
            &outdir.to_string_lossy(),
            &input.to_string_lossy(),
        ],
        240,
    )
    .await?;
    let ext = fmt.split(':').next().unwrap_or(fmt);
    let produced = outdir.join(input.with_extension(ext).file_name().unwrap());
    if !produced.exists() {
        bail!(
            "LibreOffice não gerou {}: {}",
            produced.display(),
            String::from_utf8_lossy(&o.stderr)
        );
    }
    Ok(produced)
}

pub struct DocCreate;

#[async_trait]
impl Tool for DocCreate {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "doc_create".into(),
            description: "Cria documento pelo formato da extensão: .docx, .pdf, .pptx, .odt, .md, .txt, .html, .tex (a partir de `content` em Markdown) \
                ou .xlsx/.csv (a partir de `rows`: lista de linhas; a primeira é o cabeçalho; strings com '=' viram fórmulas). \
                Em .pptx cada título '#'/'##' inicia um slide."
                .into(),
            parameters: json!({"type":"object","properties":{
                "path":{"type":"string"},
                "content":{"type":"string","description":"Markdown"},
                "rows":{"type":"array","items":{"type":"array"}},
                "title":{"type":"string"},
                "sheet":{"type":"string"},
                "engine":{"type":"string","enum":["auto","builtin","pandoc"],"description":"auto usa pandoc/LibreOffice se instalados"}
            },"required":["path"]}),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let p = ctx.guard.resolve(arg_str(args, "path").unwrap_or(""));
        let verb = if p.exists() { "sobrescrever" } else { "criar" };
        ctx.guard
            .assess_write(&p, Risk::Write, format!("{verb} documento {}", p.display()))
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let p = ctx.guard.resolve(arg_str(&args, "path").unwrap_or(""));
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let content = arg_str(&args, "content").unwrap_or("");
        let title = arg_str(&args, "title");
        let engine = arg_str(&args, "engine").unwrap_or("auto");
        let has_pandoc = which("pandoc").is_some() && engine != "builtin";
        let method;
        match ext(&p).as_str() {
            "xlsx" => {
                let rows = rows_arg(&args).context("xlsx requer `rows`")?;
                write_xlsx(&p, &rows, arg_str(&args, "sheet").unwrap_or("Planilha1"))?;
                method = "rust_xlsxwriter";
            }
            "csv" => {
                let rows = rows_arg(&args).context("csv requer `rows`")?;
                write_csv(&p, &rows)?;
                method = "csv";
            }
            "docx" | "odt" if has_pandoc => {
                pandoc_convert(content, &p, title).await?;
                method = "pandoc";
            }
            "docx" => {
                crate::ooxml::write_docx(&p, title, content)?;
                method = "builtin";
            }
            "odt" => {
                let tmp = tempfile::tempdir()?;
                let docx = tmp
                    .path()
                    .join(p.with_extension("docx").file_name().unwrap());
                crate::ooxml::write_docx(&docx, title, content)?;
                let produced = soffice_convert(&docx, "odt", tmp.path()).await?;
                std::fs::copy(produced, &p)?;
                method = "builtin+libreoffice";
            }
            "pptx" => {
                if !has_pandoc {
                    bail!("criar .pptx requer pandoc (sudo apt install pandoc)");
                }
                pandoc_convert(content, &p, title).await?;
                method = "pandoc";
            }
            "pdf" => {
                if engine == "auto" && has_pandoc && which("soffice").is_some() {
                    let tmp = tempfile::tempdir()?;
                    let docx = tmp
                        .path()
                        .join(p.with_extension("docx").file_name().unwrap());
                    pandoc_convert(content, &docx, title).await?;
                    let produced = soffice_convert(&docx, "pdf", tmp.path()).await?;
                    std::fs::copy(produced, &p)?;
                    method = "pandoc+libreoffice";
                } else {
                    std::fs::write(&p, crate::pdfgen::render(title, content))?;
                    method = "builtin";
                }
            }
            "md" | "txt" | "tex" | "html" | "htm" | "rst" | "json" | "xml" | "yaml" | "yml" => {
                let mut body = String::new();
                if let (Some(t), "md") = (title, ext(&p).as_str()) {
                    body.push_str(&format!("# {t}\n\n"));
                }
                body.push_str(content);
                std::fs::write(&p, body)?;
                method = "texto";
            }
            other => bail!("formato .{other} não suportado por doc_create"),
        }
        // Verify by reading the result back.
        let size = std::fs::metadata(&p)
            .with_context(|| format!("{} não foi criado", p.display()))?
            .len();
        let check = match ext(&p).as_str() {
            "pdf" | "docx" | "pptx" | "xlsx" | "csv" | "odt" => read_document(&p, None, 5)
                .await
                .map(|t| t.chars().take(300).collect::<String>()),
            _ => Ok(String::new()),
        };
        let check = match check {
            Ok(s) if !s.is_empty() => {
                format!("\nVerificação (início do conteúdo lido de volta):\n{s}")
            }
            Ok(_) => String::new(),
            Err(e) => format!("\nAviso: não foi possível reler o arquivo: {e}"),
        };
        Ok(ToolOutput::ok(format!(
            "Criado {} ({size} bytes, via {method}).{check}",
            p.display()
        ))
        .with_artifact(p))
    }
}

// ---------------------------------------------------------------------------

pub struct DocConvert;

#[async_trait]
impl Tool for DocConvert {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "doc_convert".into(),
            description: "Converte documentos (LibreOffice: docx/odt/xlsx/pptx/doc → pdf, docx, odt, xlsx...; pandoc: md/html/docx/epub). Saída no mesmo diretório ou em output.".into(),
            parameters: json!({"type":"object","properties":{"path":{"type":"string"},"to":{"type":"string","description":"extensão de destino, ex.: pdf"},"output":{"type":"string"}},"required":["path","to"]}),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let input = ctx.guard.resolve(arg_str(args, "path").unwrap_or(""));
        let to = arg_str(args, "to").unwrap_or("pdf");
        let out = arg_str(args, "output")
            .map(|o| ctx.guard.resolve(o))
            .unwrap_or_else(|| input.with_extension(to));
        let r = ctx.guard.assess_read(&input, String::new());
        if r.force.is_some() {
            return r;
        }
        ctx.guard.assess_write(
            &out,
            Risk::Write,
            format!("converter {} → {}", input.display(), out.display()),
        )
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let input = ctx.guard.resolve(arg_str(&args, "path").unwrap_or(""));
        let to = arg_str(&args, "to")
            .unwrap_or("pdf")
            .trim_start_matches('.')
            .to_lowercase();
        let out = arg_str(&args, "output")
            .map(|o| ctx.guard.resolve(o))
            .unwrap_or_else(|| input.with_extension(&to));
        let src = ext(&input);
        let pandoc_formats = ["md", "markdown", "html", "epub", "rst", "tex", "org"];
        if which("pandoc").is_some()
            && (pandoc_formats.contains(&src.as_str()) || pandoc_formats.contains(&to.as_str()))
            && to != "pdf"
        {
            let o = run_quiet(
                "pandoc",
                &[&input.to_string_lossy(), "-o", &out.to_string_lossy()],
                120,
            )
            .await?;
            if !o.status.success() {
                bail!("pandoc falhou: {}", String::from_utf8_lossy(&o.stderr));
            }
        } else {
            let tmp = tempfile::tempdir()?;
            let produced = soffice_convert(&input, &to, tmp.path()).await?;
            std::fs::copy(produced, &out)?;
        }
        let size = std::fs::metadata(&out)
            .context("conversão não gerou o arquivo")?
            .len();
        Ok(
            ToolOutput::ok(format!("Convertido para {} ({size} bytes)", out.display()))
                .with_artifact(out),
        )
    }
}

// ---------------------------------------------------------------------------

pub struct LatexCompile;

#[async_trait]
impl Tool for LatexCompile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "latex_compile".into(),
            description: "Compila um .tex em PDF (tectonic, latexmk, pdflatex ou xelatex; shell-escape desativado). Retorna erros do log.".into(),
            parameters: json!({"type":"object","properties":{"path":{"type":"string"},"engine":{"type":"string","enum":["auto","tectonic","pdflatex","xelatex","lualatex"]}},"required":["path"]}),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let p = ctx.guard.resolve(arg_str(args, "path").unwrap_or(""));
        ctx.guard.assess_write(
            &p.with_extension("pdf"),
            Risk::Execute,
            format!("compilar LaTeX {}", p.display()),
        )
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let p = ctx.guard.resolve(arg_str(&args, "path").unwrap_or(""));
        let dir = p.parent().unwrap_or(Path::new(".")).to_path_buf();
        let want = arg_str(&args, "engine").unwrap_or("auto");
        let engine = ["tectonic", "latexmk", "pdflatex", "xelatex", "lualatex"]
            .into_iter()
            .find(|e| (want == "auto" || *e == want) && which(e).is_some())
            .context("nenhum compilador LaTeX encontrado (sudo apt install texlive-latex-recommended, ou instale o tectonic)")?;
        let file = p.file_name().unwrap().to_string_lossy().to_string();
        let a: Vec<String> = match engine {
            "tectonic" => vec!["--untrusted".into(), file.clone()],
            "latexmk" => vec![
                "-pdf".into(),
                "-interaction=nonstopmode".into(),
                "-halt-on-error".into(),
                "-no-shell-escape".into(),
                file.clone(),
            ],
            e => vec![
                format!("-interaction=nonstopmode"),
                "-halt-on-error".into(),
                "-no-shell-escape".into(),
                if e == "pdflatex" {
                    "-output-format=pdf".into()
                } else {
                    "-recorder".into()
                },
                file.clone(),
            ],
        };
        let cmd = crate::shell::build_command(engine, &a, &dir, ctx, true);
        let res = crate::shell::run_command(cmd, Duration::from_secs(300), ctx).await?;
        let pdf = p.with_extension("pdf");
        if res.ok() && pdf.exists() {
            return Ok(ToolOutput::ok(format!(
                "PDF gerado: {} ({} bytes) com {engine}",
                pdf.display(),
                std::fs::metadata(&pdf)?.len()
            ))
            .with_artifact(pdf));
        }
        let log = std::fs::read_to_string(p.with_extension("log")).unwrap_or_default();
        let errors: Vec<&str> = log
            .lines()
            .filter(|l| l.starts_with('!') || l.contains("Error"))
            .take(20)
            .collect();
        Ok(ToolOutput::err(format!(
            "Falha ao compilar com {engine}.\n{}\n{}",
            errors.join("\n"),
            res.render(4000)
        )))
    }
}
