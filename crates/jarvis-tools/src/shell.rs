//! Terminal, background jobs and Linux diagnostics.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use jarvis_core::permissions::{PathVerdict, Risk, RiskAssessment};
use jarvis_core::safety::is_secret_env;
use jarvis_core::tool::{arg_bool, arg_str, arg_u64, Tool, ToolContext, ToolOutput, ToolRegistry};
use jarvis_llm::ToolSpec;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

pub fn register(r: &mut ToolRegistry) {
    r.register(Arc::new(Shell));
    r.register(Arc::new(JobStatus));
    r.register(Arc::new(SystemInfo));
    r.register(Arc::new(ProcessList));
}

// ---------------------------------------------------------------------------
// Command classification
// ---------------------------------------------------------------------------

const ADMIN: &[&str] = &[
    "sudo",
    "su",
    "doas",
    "pkexec",
    "run0",
    "passwd",
    "visudo",
    "mount",
    "umount",
    "modprobe",
    "insmod",
    "rmmod",
    "reboot",
    "shutdown",
    "poweroff",
    "halt",
    "init",
    "telinit",
    "useradd",
    "usermod",
    "userdel",
    "groupadd",
    "groupdel",
    "chpasswd",
    "iptables",
    "ip6tables",
    "nft",
    "ufw",
    "service",
    "update-grub",
    "grub-install",
    "dpkg-reconfigure",
    "chroot",
    "setcap",
    "swapon",
    "swapoff",
    "cryptsetup",
    "lvm",
    "sysctl",
];
const DESTRUCTIVE: &[&str] = &[
    "rm", "rmdir", "shred", "dd", "wipefs", "fdisk", "sfdisk", "parted", "gdisk", "truncate",
    "kill", "killall", "pkill", "srm", "unlink",
];
const NETWORK: &[&str] = &[
    "curl",
    "wget",
    "ssh",
    "scp",
    "sftp",
    "rsync",
    "nc",
    "ncat",
    "netcat",
    "telnet",
    "ftp",
    "aria2c",
    "yt-dlp",
    "youtube-dl",
];
const READ_ONLY: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "wc",
    "grep",
    "rg",
    "egrep",
    "fgrep",
    "pwd",
    "echo",
    "printf",
    "date",
    "cal",
    "uname",
    "whoami",
    "id",
    "groups",
    "df",
    "du",
    "free",
    "uptime",
    "ps",
    "pgrep",
    "lscpu",
    "lsblk",
    "lspci",
    "lsusb",
    "lsmod",
    "hostname",
    "which",
    "whereis",
    "type",
    "file",
    "stat",
    "sort",
    "uniq",
    "cut",
    "tr",
    "diff",
    "cmp",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "b2sum",
    "tree",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "nproc",
    "env",
    "printenv",
    "locale",
    "true",
    "false",
    "test",
    "seq",
    "column",
    "jq",
    "less",
    "more",
    "nl",
    "od",
    "xxd",
    "hexdump",
    "strings",
    "journalctl",
    "dmesg",
    "sensors",
    "inxi",
    "neofetch",
    "fastfetch",
    "glxinfo",
    "vulkaninfo",
    "clinfo",
    "rocminfo",
    "nvidia-smi",
    "ffprobe",
    "pdfinfo",
    "pdftotext",
    "identify",
    "exiftool",
    "mediainfo",
    "top",
    "htop",
    "vmstat",
    "iostat",
    "lsof",
    "ss",
    "getent",
    "tty",
    "sleep",
    "time",
    "man",
    "apropos",
    "fc-list",
    "xdg-mime",
    "zcat",
    "zgrep",
    "awk",
    "sed",
    "find",
];

fn first_word_risk(tokens: &[String]) -> Risk {
    let mut i = 0;
    while i < tokens.len() && tokens[i].contains('=') && !tokens[i].starts_with('-') {
        i += 1; // env assignments
    }
    let Some(cmd) = tokens.get(i) else {
        return Risk::Read;
    };
    let name = Path::new(cmd)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let args = &tokens[i + 1..];
    let has = |a: &str| args.iter().any(|x| x == a);
    let any_starts = |p: &str| args.iter().any(|x| x.starts_with(p));
    if ADMIN.contains(&name.as_str()) {
        if name == "sysctl" && !has("-w") && !any_starts("--write") {
            return Risk::Read;
        }
        return Risk::Admin;
    }
    match name.as_str() {
        "systemctl" => {
            if has("--user") {
                return if args.iter().any(|a| {
                    ["status", "list-units", "list-timers", "is-active", "show"]
                        .contains(&a.as_str())
                }) {
                    Risk::Read
                } else {
                    Risk::Execute
                };
            }
            return if args.iter().any(|a| {
                [
                    "status",
                    "list-units",
                    "list-timers",
                    "is-active",
                    "show",
                    "is-enabled",
                ]
                .contains(&a.as_str())
            }) {
                Risk::Read
            } else {
                Risk::Admin
            };
        }
        "apt" | "apt-get" | "aptitude" | "dnf" | "yum" | "pacman" | "zypper" | "snap" => {
            return if args.iter().any(|a| {
                [
                    "list", "search", "show", "policy", "info", "-Q", "-Ss", "-Si",
                ]
                .contains(&a.as_str())
            }) {
                Risk::Read
            } else {
                Risk::Admin
            };
        }
        "dpkg" => {
            return if has("-l") || has("-L") || has("-s") || has("--list") {
                Risk::Read
            } else {
                Risk::Admin
            }
        }
        "flatpak" => {
            return if has("--system") {
                Risk::Admin
            } else if has("list") || has("info") {
                Risk::Read
            } else {
                Risk::Execute
            }
        }
        "chmod" | "chown" | "chgrp" => {
            return if has("-R")
                || args.iter().any(|a| {
                    a.starts_with('/') && !a.starts_with("/home") && !a.starts_with("/tmp")
                }) {
                Risk::Destructive
            } else {
                Risk::Write
            };
        }
        "find" => {
            return if has("-delete") || has("-exec") || has("-execdir") || has("-ok") {
                Risk::Destructive
            } else {
                Risk::Read
            }
        }
        "sed" => {
            return if args
                .iter()
                .any(|a| a.starts_with("-i") || a == "--in-place")
            {
                Risk::Write
            } else {
                Risk::Read
            }
        }
        "awk" | "gawk" => {
            return if args
                .iter()
                .any(|a| a.contains("system(") || a.contains("| getline") || a.contains("print >"))
            {
                Risk::Execute
            } else {
                Risk::Read
            }
        }
        "git" => {
            let sub = args
                .iter()
                .find(|a| !a.starts_with('-'))
                .map(|s| s.as_str())
                .unwrap_or("");
            return match sub {
                "status" | "log" | "diff" | "show" | "branch" | "remote" | "blame"
                | "rev-parse" | "ls-files" | "describe" | "tag" => Risk::Read,
                "clone" | "fetch" | "pull" | "push" | "ls-remote" | "submodule" => {
                    if sub == "push" && (has("--force") || has("-f") || any_starts("--force")) {
                        Risk::Destructive
                    } else {
                        Risk::Network
                    }
                }
                "reset" if has("--hard") => Risk::Destructive,
                "clean" | "filter-branch" => Risk::Destructive,
                "checkout" | "restore" if has("--") || has(".") => Risk::Destructive,
                _ => Risk::Write,
            };
        }
        "pip" | "pip3" | "npm" | "pnpm" | "yarn" | "cargo" | "gem" | "go" => {
            let sub = args.first().map(|s| s.as_str()).unwrap_or("");
            return if ["install", "add", "update", "upgrade", "publish", "login"].contains(&sub) {
                Risk::Network
            } else {
                Risk::Execute
            };
        }
        "crontab" => {
            return if has("-r") {
                Risk::Destructive
            } else if has("-l") {
                Risk::Read
            } else {
                Risk::Write
            }
        }
        "mv" | "cp" | "mkdir" | "touch" | "ln" | "tee" | "tar" | "zip" | "unzip" | "gzip"
        | "gunzip" | "xz" | "7z" | "patch" => {
            if (name == "mv" || name == "cp") && (has("-f") || has("--force")) {
                return Risk::Destructive;
            }
            return Risk::Write;
        }
        _ => {}
    }
    if DESTRUCTIVE.contains(&name.as_str()) {
        if name == "kill" && !has("-9") && !has("-KILL") && args.len() <= 1 {
            return Risk::Execute;
        }
        return Risk::Destructive;
    }
    if NETWORK.contains(&name.as_str()) {
        return Risk::Network;
    }
    if READ_ONLY.contains(&name.as_str()) {
        return Risk::Read;
    }
    if name.starts_with("mkfs") {
        return Risk::Destructive;
    }
    Risk::Execute
}

/// Classify a shell command line by its most dangerous segment.
pub fn classify_command(cmd: &str) -> Risk {
    let mut risk = Risk::Read;
    // Command substitution / eval hide what runs: at least Execute.
    if cmd.contains("$(") || cmd.contains('`') || cmd.contains("eval ") || cmd.contains("<(") {
        risk = Risk::Execute;
    }
    // Output redirection to a file (not /dev/null, not fd dup) writes.
    let redir = regex::Regex::new(r"(^|[^0-9&>])>{1,2}\s*([^&\s][^\s;|&]*)").unwrap();
    for c in redir.captures_iter(cmd) {
        let target = &c[2];
        if target != "/dev/null" && target != "/dev/stderr" && target != "/dev/stdout" {
            risk = risk.max(
                if target.starts_with("/dev/")
                    || target.starts_with("/etc/")
                    || target.starts_with("/boot")
                {
                    Risk::Admin
                } else {
                    Risk::Write
                },
            );
        }
    }
    // Drop redirections (already accounted for) so `2>&1` is not read as a background `&`.
    let strip = regex::Regex::new(r"[0-9]*(>{1,2}|<)&?\s*[^\s;|&]+|[0-9]*>&[0-9-]+").unwrap();
    let without_redirs = strip.replace_all(cmd, " ");
    let normalized = without_redirs
        .replace("&&", ";")
        .replace("||", ";")
        .replace(['|', '\n', '&'], ";")
        .replace("$(", ";")
        .replace('`', ";")
        .replace(['(', ')'], " ");
    for seg in normalized.split(';') {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        let tokens = shell_words::split(seg)
            .unwrap_or_else(|_| seg.split_whitespace().map(String::from).collect());
        let mut r = first_word_risk(&tokens);
        // `xargs rm`, `nohup cmd`, `timeout 5 cmd`, `bash -c`: classify the wrapped command.
        if let Some(first) = tokens.first().map(|s| s.as_str()) {
            if [
                "xargs", "nohup", "timeout", "nice", "ionice", "time", "env", "stdbuf", "watch",
            ]
            .contains(&first)
            {
                let inner: Vec<String> = tokens
                    .iter()
                    .skip(1)
                    .skip_while(|t| {
                        t.starts_with('-') || t.parse::<f64>().is_ok() || t.contains('=')
                    })
                    .cloned()
                    .collect();
                r = r.max(first_word_risk(&inner));
            } else if [
                "bash", "sh", "zsh", "dash", "fish", "python", "python3", "perl", "ruby", "node",
            ]
            .contains(&first)
            {
                r = r.max(Risk::Execute);
                if let Some(pos) = tokens.iter().position(|t| t == "-c") {
                    if let Some(inner) = tokens.get(pos + 1) {
                        r = r.max(classify_command(inner));
                    }
                }
            }
        }
        risk = risk.max(r);
    }
    risk
}

// ---------------------------------------------------------------------------
// Process execution
// ---------------------------------------------------------------------------

pub fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

/// Environment for child processes: inherit, minus secrets.
pub fn scrubbed_env() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(k, _)| !is_secret_env(k))
        .collect()
}

pub struct ExecResult {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
    pub ms: u128,
}

impl ExecResult {
    pub fn render(&self, max: usize) -> String {
        let mut s = String::new();
        if self.timed_out {
            s.push_str("[TEMPO ESGOTADO — processo encerrado]\n");
        }
        if self.cancelled {
            s.push_str("[CANCELADO — processo encerrado]\n");
        }
        s.push_str(&format!(
            "exit code: {}  ({} ms)\n",
            self.code.map(|c| c.to_string()).unwrap_or("-".into()),
            self.ms
        ));
        if !self.stdout.is_empty() {
            s.push_str("--- stdout ---\n");
            s.push_str(&jarvis_core::safety::truncate_middle(
                &self.stdout,
                max * 3 / 4,
            ));
            if !s.ends_with('\n') {
                s.push('\n');
            }
        }
        if !self.stderr.is_empty() {
            s.push_str("--- stderr ---\n");
            s.push_str(&jarvis_core::safety::truncate_middle(&self.stderr, max / 4));
        }
        s
    }
    pub fn ok(&self) -> bool {
        self.code == Some(0) && !self.timed_out && !self.cancelled
    }
}

async fn read_capped<R: AsyncRead + Unpin>(mut r: R, cap: usize) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match r.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if buf.len() < cap {
                    buf.extend_from_slice(&chunk[..n.min(cap - buf.len())]);
                }
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

/// Build the command, wrapped in bubblewrap when enabled and installed.
pub fn build_command(
    program: &str,
    args: &[String],
    cwd: &Path,
    ctx: &ToolContext,
    sandbox: bool,
) -> Command {
    let bwrap = if sandbox && ctx.cfg.sandbox.enabled {
        which("bwrap")
    } else {
        None
    };
    let mut cmd = match &bwrap {
        Some(b) => {
            let mut c = Command::new(b);
            c.args([
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--tmpfs",
                "/tmp",
            ]);
            for w in &ctx.guard.workspace {
                let w = w.to_string_lossy().to_string();
                c.args(["--bind", &w, &w]);
            }
            if !ctx.cfg.sandbox.network {
                c.arg("--unshare-net");
            }
            c.args([
                "--die-with-parent",
                "--chdir",
                &cwd.to_string_lossy(),
                "--",
                program,
            ]);
            c.args(args);
            c
        }
        None => {
            let mut c = Command::new(program);
            c.args(args);
            c
        }
    };
    cmd.current_dir(cwd)
        .env_clear()
        .envs(scrubbed_env())
        .env("JARVIS_AGENT", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    cmd
}

/// Run to completion with timeout, cancellation and output caps.
pub async fn run_command(
    mut cmd: Command,
    timeout: Duration,
    ctx: &ToolContext,
) -> Result<ExecResult> {
    let t0 = Instant::now();
    let mut child = cmd.spawn().context("falha ao iniciar o processo")?;
    let pid = child.id();
    let cap = ctx.max_output.max(4096) * 4;
    let out = tokio::spawn(read_capped(child.stdout.take().unwrap(), cap));
    let err = tokio::spawn(read_capped(child.stderr.take().unwrap(), cap));
    let (mut timed_out, mut cancelled) = (false, false);
    let status = tokio::select! {
        s = child.wait() => s.ok(),
        _ = tokio::time::sleep(timeout) => { timed_out = true; kill_group(pid); child.wait().await.ok() }
        _ = ctx.cancel.cancelled() => { cancelled = true; kill_group(pid); child.wait().await.ok() }
    };
    // Kill leftovers in the group (background children) so pipes close.
    kill_group(pid);
    let stdout = tokio::time::timeout(Duration::from_secs(5), out)
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();
    let stderr = tokio::time::timeout(Duration::from_secs(5), err)
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();
    Ok(ExecResult {
        code: status.and_then(|s| s.code()),
        stdout,
        stderr,
        timed_out,
        cancelled,
        ms: t0.elapsed().as_millis(),
    })
}

// ---------------------------------------------------------------------------
// Background jobs
// ---------------------------------------------------------------------------

struct Job {
    command: String,
    started: Instant,
    output: Arc<Mutex<String>>,
    status: Arc<Mutex<Option<i32>>>,
    pid: Option<u32>,
}

fn jobs() -> &'static Mutex<HashMap<String, Job>> {
    static J: OnceLock<Mutex<HashMap<String, Job>>> = OnceLock::new();
    J.get_or_init(Default::default)
}

fn spawn_background(mut cmd: Command, command: &str) -> Result<String> {
    let mut child = cmd
        .kill_on_drop(false)
        .spawn()
        .context("falha ao iniciar o processo")?;
    let id = format!("job-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let output = Arc::new(Mutex::new(String::new()));
    let status = Arc::new(Mutex::new(None));
    let pid = child.id();
    for stream in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn AsyncRead + Unpin + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn AsyncRead + Unpin + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let o = output.clone();
        tokio::spawn(async move {
            let mut s = stream;
            let mut chunk = [0u8; 4096];
            while let Ok(n) = s.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                let mut g = o.lock().unwrap();
                g.push_str(&String::from_utf8_lossy(&chunk[..n]));
                if g.len() > 200_000 {
                    let cut = g.len() - 100_000;
                    let mut c = cut;
                    while !g.is_char_boundary(c) {
                        c += 1;
                    }
                    g.drain(..c);
                }
            }
        });
    }
    let st = status.clone();
    tokio::spawn(async move {
        let code = child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1);
        *st.lock().unwrap() = Some(code);
    });
    jobs().lock().unwrap().insert(
        id.clone(),
        Job {
            command: command.into(),
            started: Instant::now(),
            output,
            status,
            pid,
        },
    );
    Ok(id)
}

// ---------------------------------------------------------------------------

pub struct Shell;

#[async_trait]
impl Tool for Shell {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "shell".into(),
            description: "Executa um comando bash no workspace (sem sudo). Retorna exit code, stdout e stderr. \
                Use background=true para processos longos e consulte com job_status."
                .into(),
            parameters: json!({"type":"object","properties":{
                "command":{"type":"string"},
                "cwd":{"type":"string","description":"diretório (padrão: workspace)"},
                "timeout_secs":{"type":"integer","description":"padrão 120, máximo 3600"},
                "background":{"type":"boolean"}
            },"required":["command"]}),
        }
    }
    fn assess(&self, args: &Value, ctx: &ToolContext) -> RiskAssessment {
        let cmd = arg_str(args, "command").unwrap_or("");
        let mut risk = classify_command(cmd);
        if arg_bool(args, "background").unwrap_or(false) {
            risk = risk.max(Risk::Execute);
        }
        let mut ra = RiskAssessment::new(risk, format!("$ {cmd}"));
        if let Some(cwd) = arg_str(args, "cwd") {
            let p = ctx.guard.resolve(cwd);
            match ctx.guard.classify(&p) {
                PathVerdict::Blocked(r) => ra = ra.deny(r),
                PathVerdict::Outside => {
                    ra = ra.ask(format!("executar fora do workspace ({})", p.display()))
                }
                _ => {}
            }
        }
        ra
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let command = arg_str(&args, "command").unwrap_or("").to_string();
        let cwd = arg_str(&args, "cwd")
            .map(|c| ctx.guard.resolve(c))
            .unwrap_or_else(|| ctx.guard.cwd());
        std::fs::create_dir_all(&cwd).ok();
        let timeout = Duration::from_secs(
            arg_u64(&args, "timeout_secs")
                .unwrap_or(ctx.timeout.as_secs())
                .clamp(1, 3600),
        );
        let cmd = build_command("bash", &["-c".into(), command.clone()], &cwd, ctx, true);
        if arg_bool(&args, "background").unwrap_or(false) {
            let id = spawn_background(cmd, &command)?;
            return Ok(ToolOutput::ok(format!(
                "Iniciado em segundo plano: {id}. Use job_status com job_id=\"{id}\"."
            )));
        }
        let res = run_command(cmd, timeout, ctx).await?;
        let mut out = ToolOutput::ok(res.render(ctx.max_output));
        out.is_error = !res.ok();
        Ok(out)
    }
}

pub struct JobStatus;

#[async_trait]
impl Tool for JobStatus {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "job_status".into(),
            description: "Mostra status e saída recente de tarefas em segundo plano (sem job_id lista todas). kill=true encerra.".into(),
            parameters: json!({"type":"object","properties":{"job_id":{"type":"string"},"kill":{"type":"boolean"},"tail_bytes":{"type":"integer"}}}),
        }
    }
    fn assess(&self, args: &Value, _: &ToolContext) -> RiskAssessment {
        if arg_bool(args, "kill").unwrap_or(false) {
            RiskAssessment::new(
                Risk::Execute,
                format!("encerrar {}", arg_str(args, "job_id").unwrap_or("?")),
            )
        } else {
            RiskAssessment::new(Risk::Read, "consultar tarefas em segundo plano")
        }
    }
    async fn call(&self, args: Value, _: &ToolContext) -> Result<ToolOutput> {
        let jobs = jobs().lock().unwrap();
        let Some(id) = arg_str(&args, "job_id") else {
            if jobs.is_empty() {
                return Ok(ToolOutput::ok("Nenhuma tarefa em segundo plano."));
            }
            let list: Vec<String> = jobs
                .iter()
                .map(|(id, j)| {
                    format!(
                        "{id}: {} — {} — {}s",
                        j.command,
                        j.status
                            .lock()
                            .unwrap()
                            .map(|c| format!("terminou ({c})"))
                            .unwrap_or("rodando".into()),
                        j.started.elapsed().as_secs()
                    )
                })
                .collect();
            return Ok(ToolOutput::ok(list.join("\n")));
        };
        let Some(job) = jobs.get(id) else {
            return Ok(ToolOutput::err(format!("job {id} não existe")));
        };
        if arg_bool(&args, "kill").unwrap_or(false) {
            kill_group(job.pid);
        }
        let tail = arg_u64(&args, "tail_bytes").unwrap_or(4000) as usize;
        let out = job.output.lock().unwrap().clone();
        let mut start = out.len().saturating_sub(tail);
        while !out.is_char_boundary(start) {
            start += 1;
        }
        let status = job
            .status
            .lock()
            .unwrap()
            .map(|c| format!("terminou com código {c}"))
            .unwrap_or("rodando".into());
        Ok(ToolOutput::ok(format!(
            "{id} ({}): {status}, {}s\n--- saída ---\n{}",
            job.command,
            job.started.elapsed().as_secs(),
            &out[start..]
        )))
    }
}

// ---------------------------------------------------------------------------

pub struct SystemInfo;

fn read_trim(p: &str) -> String {
    std::fs::read_to_string(p)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

pub async fn quick(cmd: &str, args: &[&str]) -> Option<String> {
    let out = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(cmd).args(args).stdin(Stdio::null()).output(),
    )
    .await
    .ok()?
    .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[async_trait]
impl Tool for SystemInfo {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "system_info".into(),
            description: "Diagnóstico do Linux: distribuição, kernel, CPU, memória, disco, GPU e programas úteis instalados.".into(),
            parameters: json!({"type":"object","properties":{}}),
        }
    }
    fn assess(&self, _: &Value, _: &ToolContext) -> RiskAssessment {
        RiskAssessment::new(Risk::Read, "coletar informações do sistema")
    }
    async fn call(&self, _: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let os = read_trim("/etc/os-release")
            .lines()
            .find(|l| l.starts_with("PRETTY_NAME="))
            .map(|l| l[12..].trim_matches('"').to_string())
            .unwrap_or_default();
        let kernel = read_trim("/proc/sys/kernel/osrelease");
        let cpu = read_trim("/proc/cpuinfo")
            .lines()
            .find(|l| l.starts_with("model name"))
            .and_then(|l| l.split(':').nth(1))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let res = jarvis_llm::router::detect_resources();
        let gpu = quick(
            "sh",
            &["-c", "lspci 2>/dev/null | grep -Ei 'vga|3d|display'"],
        )
        .await
        .unwrap_or_else(|| "(lspci indisponível)".into());
        let disk = quick("df", &["-h", &ctx.guard.cwd().to_string_lossy()])
            .await
            .unwrap_or_default();
        let load = read_trim("/proc/loadavg");
        let progs = [
            "ffmpeg",
            "ffprobe",
            "pandoc",
            "soffice",
            "pdftotext",
            "pdflatex",
            "xelatex",
            "tectonic",
            "latexmk",
            "tesseract",
            "convert",
            "magick",
            "chromium",
            "chromium-browser",
            "google-chrome",
            "firefox",
            "llama-server",
            "whisper-cli",
            "piper",
            "espeak-ng",
            "bwrap",
            "git",
            "python3",
            "docker",
            "podman",
            "vulkaninfo",
        ];
        let found: Vec<&str> = progs
            .iter()
            .copied()
            .filter(|p| which(p).is_some())
            .collect();
        let missing: Vec<&str> = progs
            .iter()
            .copied()
            .filter(|p| which(p).is_none())
            .collect();
        Ok(ToolOutput::ok(format!(
            "Sistema: {os}\nKernel: {kernel}\nCPU: {cpu} ({} threads)\nMemória: {} MB livres de {} MB\nCarga: {load}\nGPU: {gpu}\nDisco (workspace):\n{disk}\nProgramas encontrados: {}\nNão encontrados: {}",
            res.cpu_threads,
            res.mem_available_mb,
            res.mem_total_mb,
            found.join(", "),
            missing.join(", ")
        )))
    }
}

pub struct ProcessList;

#[async_trait]
impl Tool for ProcessList {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "process_list".into(),
            description: "Lista processos ordenados por uso de CPU ou memória.".into(),
            parameters: json!({"type":"object","properties":{"sort":{"type":"string","enum":["cpu","mem"]},"limit":{"type":"integer"}}}),
        }
    }
    fn assess(&self, _: &Value, _: &ToolContext) -> RiskAssessment {
        RiskAssessment::new(Risk::Read, "listar processos")
    }
    async fn call(&self, args: Value, _: &ToolContext) -> Result<ToolOutput> {
        let key = if arg_str(&args, "sort") == Some("mem") {
            "-%mem"
        } else {
            "-%cpu"
        };
        let limit = arg_u64(&args, "limit").unwrap_or(25) as usize;
        let out = quick(
            "ps",
            &[
                "-eo",
                "pid,user,%cpu,%mem,rss,etime,comm",
                &format!("--sort={key}"),
            ],
        )
        .await
        .context("ps falhou")?;
        Ok(ToolOutput::ok(
            out.lines().take(limit + 1).collect::<Vec<_>>().join("\n"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(classify_command("ls -la && df -h | grep /"), Risk::Read);
        assert_eq!(classify_command("cat a > b.txt"), Risk::Write);
        assert_eq!(classify_command("ls 2>&1 >/dev/null"), Risk::Read);
        assert_eq!(classify_command("rm -rf build"), Risk::Destructive);
        assert_eq!(classify_command("echo hi; sudo apt install x"), Risk::Admin);
        assert_eq!(classify_command("FOO=1 sudo ls"), Risk::Admin);
        assert_eq!(classify_command("apt list --installed"), Risk::Read);
        assert_eq!(classify_command("apt install vlc"), Risk::Admin);
        assert_eq!(classify_command("curl https://x"), Risk::Network);
        assert_eq!(
            classify_command("find . -name '*.o' -delete"),
            Risk::Destructive
        );
        assert_eq!(classify_command("python3 script.py"), Risk::Execute);
        assert_eq!(classify_command("bash -c 'rm -rf /'"), Risk::Destructive);
        assert_eq!(classify_command("ls | xargs rm"), Risk::Destructive);
        assert_eq!(classify_command("echo $(whoami)"), Risk::Execute);
        assert_eq!(classify_command("systemctl --user status x"), Risk::Read);
        assert_eq!(classify_command("systemctl restart nginx"), Risk::Admin);
        assert_eq!(classify_command("git status"), Risk::Read);
        assert_eq!(classify_command("git push --force"), Risk::Destructive);
        assert_eq!(classify_command("echo x > /etc/hosts"), Risk::Admin);
        assert_eq!(classify_command("sed -i s/a/b/ f"), Risk::Write);
        assert_eq!(classify_command("/usr/bin/sudo ls"), Risk::Admin);
        assert_eq!(classify_command("timeout 5 rm x"), Risk::Destructive);
    }
}
