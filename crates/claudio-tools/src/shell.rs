//! Terminal, background jobs and system diagnostics (Linux, macOS, Windows).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use claudio_core::permissions::{PathVerdict, Risk, RiskAssessment};
use claudio_core::safety::is_secret_env;
use claudio_core::tool::{arg_bool, arg_str, arg_u64, Tool, ToolContext, ToolOutput, ToolRegistry};
use claudio_llm::ToolSpec;
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

/// Windows (cmd / PowerShell) commands, lower-case, matched case-insensitively.
const WIN_ADMIN: &[&str] = &[
    "runas",
    "set-executionpolicy",
    "reg",
    "regedit",
    "sc",
    "bcdedit",
    "diskpart",
    "format",
    "stop-computer",
    "restart-computer",
    "enable-windowsoptionalfeature",
    "disable-windowsoptionalfeature",
    "dism",
    "netsh",
    "icacls",
    "takeown",
    "new-localuser",
    "remove-localuser",
    "add-localgroupmember",
    "set-service",
    "new-service",
    "winget",
    "choco",
    "msiexec",
    "wmic",
];
const WIN_DESTRUCTIVE: &[&str] = &[
    "del",
    "erase",
    "rd",
    "remove-item",
    "ri",
    "clear-content",
    "format-volume",
    "clear-disk",
    "stop-process",
    "taskkill",
    "cipher",
    "clear-recyclebin",
];
const WIN_NETWORK: &[&str] = &[
    "invoke-webrequest",
    "iwr",
    "invoke-restmethod",
    "irm",
    "start-bitstransfer",
    "curl.exe",
    "bitsadmin",
];
const WIN_READ: &[&str] = &[
    "dir",
    "get-childitem",
    "gci",
    "get-content",
    "gc",
    "get-process",
    "gps",
    "get-location",
    "gl",
    "select-string",
    "sls",
    "get-item",
    "gi",
    "get-computerinfo",
    "systeminfo",
    "tasklist",
    "where",
    "where-object",
    "test-path",
    "measure-object",
    "get-date",
    "write-output",
    "write-host",
    "get-command",
    "get-volume",
    "get-psdrive",
    "get-ciminstance",
    "get-service",
    "format-table",
    "format-list",
    "select-object",
    "sort-object",
    "ver",
    "findstr",
    "get-filehash",
    "resolve-path",
    "split-path",
    "join-path",
];

fn windows_word_risk(lname: &str, args: &[String]) -> Option<Risk> {
    let has = |a: &str| args.iter().any(|x| x.eq_ignore_ascii_case(a));
    if lname == "start-process" && (has("runas") || has("-verb")) {
        return Some(Risk::Admin);
    }
    if WIN_ADMIN.contains(&lname) {
        if (lname == "reg" && (has("query") || has("export"))) || (lname == "sc" && has("query")) {
            return Some(Risk::Read);
        }
        return Some(Risk::Admin);
    }
    if WIN_DESTRUCTIVE.contains(&lname) {
        return Some(Risk::Destructive);
    }
    if WIN_NETWORK.contains(&lname) {
        return Some(Risk::Network);
    }
    if WIN_READ.contains(&lname) {
        return Some(Risk::Read);
    }
    match lname {
        "copy" | "copy-item" | "cp" | "move" | "move-item" | "mi" | "mkdir" | "md" | "new-item"
        | "ni" | "set-content" | "add-content" | "out-file" | "rename-item" | "ren"
        | "expand-archive" | "compress-archive" => Some(Risk::Write),
        _ => None,
    }
}

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
    let lname = name.to_lowercase();
    let lname = lname.trim_end_matches(".exe");
    if let Some(r) = windows_word_risk(lname, args) {
        return r;
    }
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
        let tokens = if cfg!(windows) {
            // Backslashes are path separators on Windows, not escapes.
            seg.split_whitespace()
                .map(|t| t.trim_matches(['"', '\'']).to_string())
                .collect()
        } else {
            shell_words::split(seg)
                .unwrap_or_else(|_| seg.split_whitespace().map(String::from).collect())
        };
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

/// Find a program in PATH or in the usual install folders of this OS.
pub fn which(name: &str) -> Option<PathBuf> {
    claudio_llm::platform::find_program(&[name])
}

/// The shell used by the `shell` tool: bash on Linux/macOS, PowerShell on Windows.
pub fn shell_invocation(command: &str) -> (String, Vec<String>) {
    if cfg!(windows) {
        let ps = which("pwsh").map(|_| "pwsh").unwrap_or("powershell");
        let script = format!(
            "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $ErrorActionPreference = 'Continue'; {command}; if ($LASTEXITCODE) {{ exit $LASTEXITCODE }}"
        );
        (
            ps.to_string(),
            [
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
            ]
            .iter()
            .map(|s| s.to_string())
            .chain([script])
            .collect(),
        )
    } else {
        let sh = if which("bash").is_some() {
            "bash"
        } else {
            "sh"
        };
        (sh.to_string(), vec!["-c".to_string(), command.to_string()])
    }
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
            s.push_str(&claudio_core::safety::truncate_middle(
                &self.stdout,
                max * 3 / 4,
            ));
            if !s.ends_with('\n') {
                s.push('\n');
            }
        }
        if !self.stderr.is_empty() {
            s.push_str("--- stderr ---\n");
            s.push_str(&claudio_core::safety::truncate_middle(
                &self.stderr,
                max / 4,
            ));
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

/// Kill the process and everything it started.
fn kill_group(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// bubblewrap path, only if it can actually create namespaces here
/// (Ubuntu 24.04+ AppArmor may forbid unprivileged user namespaces).
#[cfg(target_os = "linux")]
fn bwrap_usable() -> Option<PathBuf> {
    static B: OnceLock<Option<PathBuf>> = OnceLock::new();
    B.get_or_init(|| {
        let b = which("bwrap")?;
        let ok = std::process::Command::new(&b)
            .args([
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "true",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            tracing::warn!(
                "bwrap instalado mas sem permissão para namespaces; terminal sem isolamento"
            );
        }
        ok.then_some(b)
    })
    .clone()
}

/// Whether commands run isolated on this machine (for `doctor` and the UI).
pub fn sandbox_available() -> bool {
    #[cfg(target_os = "linux")]
    return bwrap_usable().is_some();
    #[cfg(target_os = "macos")]
    return which("sandbox-exec").is_some();
    #[allow(unreachable_code)]
    false
}

/// Seatbelt profile for macOS: writes only to the workspace and a private temp dir.
#[cfg(target_os = "macos")]
fn macos_profile(ctx: &ToolContext, tmp: &Path) -> String {
    let q = |p: &Path| {
        format!(
            "\"{}\"",
            p.to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        )
    };
    let mut allowed: Vec<String> = ctx
        .guard
        .workspace
        .iter()
        .map(|w| format!("(subpath {})", q(w)))
        .collect();
    allowed.push(format!("(subpath {})", q(tmp)));
    let mut p = format!(
        "(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write* {} (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/stdout\") (literal \"/dev/stderr\") (regex #\"^/dev/tty\") (literal \"/dev/dtracehelper\"))\n",
        allowed.join(" ")
    );
    if !ctx.cfg.sandbox.network {
        p.push_str("(deny network*)\n");
    }
    p
}

/// Build the command, isolated when possible: bubblewrap on Linux,
/// `sandbox-exec` on macOS. Windows has no sandbox (see SECURITY.md).
pub fn build_command(
    program: &str,
    args: &[String],
    cwd: &Path,
    ctx: &ToolContext,
    sandbox: bool,
) -> Command {
    let program = which(program)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string());
    let program = program.as_str();
    #[cfg_attr(windows, allow(unused_variables))]
    let sandbox = sandbox && ctx.cfg.sandbox.enabled;
    #[allow(unused_mut)]
    let mut wrapped: Option<Command> = None;
    #[allow(unused_mut)]
    let mut sandbox_tmp: Option<PathBuf> = None;
    #[cfg(target_os = "linux")]
    if sandbox {
        if let Some(b) = bwrap_usable() {
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
            wrapped = Some(c);
        }
    }
    #[cfg(target_os = "macos")]
    if sandbox {
        if let Some(sb) = which("sandbox-exec") {
            let tmp = std::env::temp_dir().join(format!("claudio-sbx-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&tmp);
            let tmp = claudio_llm::platform::canonicalize(&tmp).unwrap_or(tmp);
            let mut c = Command::new(sb);
            c.arg("-p")
                .arg(macos_profile(ctx, &tmp))
                .arg(program)
                .args(args);
            sandbox_tmp = Some(tmp);
            wrapped = Some(c);
        }
    }
    let mut cmd = wrapped.unwrap_or_else(|| {
        let mut c = Command::new(program);
        c.args(args);
        c
    });
    let mut env = scrubbed_env();
    if let Some(t) = sandbox_tmp {
        env.retain(|(k, _)| k != "TMPDIR");
        env.push(("TMPDIR".into(), t.to_string_lossy().into_owned()));
    }
    cmd.current_dir(cwd)
        .env_clear()
        .envs(env)
        .env("CLAUDIO_AGENT", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

/// Run to completion with timeout, cancellation and output caps.
pub async fn run_command(cmd: Command, timeout: Duration, ctx: &ToolContext) -> Result<ExecResult> {
    run_command_with_input(cmd, None, timeout, ctx).await
}

/// Like [`run_command`], writing `input` to stdin first (e.g. the sudo password).
pub async fn run_command_with_input(
    mut cmd: Command,
    input: Option<zeroize::Zeroizing<Vec<u8>>>,
    timeout: Duration,
    ctx: &ToolContext,
) -> Result<ExecResult> {
    let t0 = Instant::now();
    if input.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().context("falha ao iniciar o processo")?;
    if let Some(input) = input {
        crate::elevate::feed_stdin(&mut child, input).await;
    }
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
            description: format!(
                "Executa um comando {} no workspace. Retorna exit code, stdout e stderr. \
                 Use background=true para processos longos e consulte com job_status. \
                 Para algo que exige administrador use a ferramenta admin_command (o usuário digita a senha).",
                if cfg!(windows) { "PowerShell" } else { "bash" }
            ),
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
        if crate::elevate::uses_sudo(cmd) {
            // The sudo password screen shows the command and asks for consent.
            ra = ra.elevated();
        }
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
        if crate::elevate::uses_sudo(&command) {
            if arg_bool(&args, "background").unwrap_or(false) {
                return Ok(ToolOutput::err(
                    "Comandos com sudo não podem rodar em segundo plano.",
                ));
            }
            return crate::elevate::run_elevated(
                &command,
                "comando do terminal que usa sudo",
                false,
                &cwd,
                timeout,
                ctx,
            )
            .await;
        }
        let (prog, sh_args) = shell_invocation(&command);
        let cmd = build_command(&prog, &sh_args, &cwd, ctx, true);
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

pub async fn quick(cmd: &str, args: &[&str]) -> Option<String> {
    let program = which(cmd)?;
    let mut c = Command::new(program);
    c.args(args).stdin(Stdio::null());
    #[cfg(windows)]
    c.creation_flags(0x0800_0000);
    let out = tokio::time::timeout(Duration::from_secs(10), c.output())
        .await
        .ok()?
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn gpu_names() -> String {
    let out = if cfg!(target_os = "linux") {
        quick(
            "sh",
            &["-c", "lspci 2>/dev/null | grep -Ei 'vga|3d|display'"],
        )
        .await
    } else if cfg!(target_os = "macos") {
        quick("system_profiler", &["SPDisplaysDataType"])
            .await
            .map(|s| {
                s.lines()
                    .filter(|l| l.contains("Chipset Model") || l.contains("VRAM"))
                    .map(str::trim)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
    } else {
        quick(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_VideoController).Name",
            ],
        )
        .await
    };
    out.filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(não detectada)".into())
}

#[async_trait]
impl Tool for SystemInfo {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "system_info".into(),
            description: "Diagnóstico do sistema: SO, kernel, CPU, memória, discos, GPU e programas úteis instalados.".into(),
            parameters: json!({"type":"object","properties":{}}),
        }
    }
    fn assess(&self, _: &Value, _: &ToolContext) -> RiskAssessment {
        RiskAssessment::new(Risk::Read, "coletar informações do sistema")
    }
    async fn call(&self, _: Value, _ctx: &ToolContext) -> Result<ToolOutput> {
        use sysinfo::{Disks, System};
        let mut sys = System::new();
        sys.refresh_memory();
        sys.refresh_cpu_all();
        let cpu = sys
            .cpus()
            .first()
            .map(|c| c.brand().trim().to_string())
            .unwrap_or_default();
        let load = System::load_average();
        let disks = Disks::new_with_refreshed_list();
        let disk_lines: Vec<String> = disks
            .list()
            .iter()
            .filter(|d| d.total_space() > 1 << 30)
            .take(8)
            .map(|d| {
                format!(
                    "  {} — {} livres de {}",
                    d.mount_point().display(),
                    crate::fs::human_size(d.available_space()),
                    crate::fs::human_size(d.total_space())
                )
            })
            .collect();
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
            "magick",
            "convert",
            "chromium",
            "google-chrome",
            "chrome",
            "msedge",
            "firefox",
            "llama-server",
            "whisper-cli",
            "piper",
            "espeak-ng",
            "say",
            "bwrap",
            "sandbox-exec",
            "git",
            "python3",
            "python",
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
            "Sistema: {}\nKernel: {}\nArquitetura: {}\nCPU: {cpu} ({} threads)\nMemória: {} MB livres de {} MB\nCarga: {:.2} {:.2} {:.2}\nGPU: {}\nDiscos:\n{}\nProgramas encontrados: {}\nNão encontrados: {}",
            claudio_llm::platform::os_pretty_name(),
            System::kernel_version().unwrap_or_default(),
            std::env::consts::ARCH,
            sys.cpus().len(),
            sys.available_memory() / (1024 * 1024),
            sys.total_memory() / (1024 * 1024),
            load.one,
            load.five,
            load.fifteen,
            gpu_names().await,
            disk_lines.join("\n"),
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
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
        let by_mem = arg_str(&args, "sort") == Some("mem");
        let limit = arg_u64(&args, "limit").unwrap_or(25) as usize;
        let rows = tokio::task::spawn_blocking(move || {
            let mut sys = System::new();
            let kind = ProcessRefreshKind::nothing().with_cpu().with_memory();
            sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
            std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
            sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
            let mut procs: Vec<_> = sys.processes().values().collect();
            if by_mem {
                procs.sort_by_key(|p| std::cmp::Reverse(p.memory()));
            } else {
                procs.sort_by(|a, b| {
                    b.cpu_usage()
                        .partial_cmp(&a.cpu_usage())
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }
            let mut out = vec![format!(
                "{:>8} {:>6} {:>10}  {}",
                "PID", "%CPU", "MEM", "NOME"
            )];
            for p in procs.into_iter().take(limit) {
                out.push(format!(
                    "{:>8} {:>6.1} {:>10}  {}",
                    p.pid().as_u32(),
                    p.cpu_usage(),
                    crate::fs::human_size(p.memory()),
                    p.name().to_string_lossy()
                ));
            }
            out.join("\n")
        })
        .await?;
        Ok(ToolOutput::ok(rows))
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
