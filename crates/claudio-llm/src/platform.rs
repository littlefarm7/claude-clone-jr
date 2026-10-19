//! Small cross-platform helpers (Linux, macOS, Windows) shared by all crates.

use std::path::{Path, PathBuf};

/// Executable file name for this OS (`llama-server` → `llama-server.exe` on Windows).
pub fn exe_name(name: &str) -> String {
    if cfg!(windows) && Path::new(name).extension().is_none() {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Look a program up in PATH (honouring PATHEXT on Windows).
pub fn which(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    let exts: Vec<String> = if cfg!(windows) {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into());
        std::iter::once(String::new())
            .chain(
                pathext
                    .split(';')
                    .filter(|e| !e.is_empty())
                    .map(|e| e.to_lowercase()),
            )
            .collect()
    } else {
        vec![String::new()]
    };
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        for ext in &exts {
            let cand = dir.join(format!("{name}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Find a program in PATH or in well-known install locations for this OS.
pub fn find_program(names: &[&str]) -> Option<PathBuf> {
    for n in names {
        if let Some(p) = which(n) {
            return Some(p);
        }
    }
    for n in names {
        for dir in extra_dirs() {
            let cand = dir.join(exe_name(n));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Install locations that are often missing from PATH.
fn extra_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if cfg!(target_os = "macos") {
        for d in [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/Applications/LibreOffice.app/Contents/MacOS",
            "/Applications/Google Chrome.app/Contents/MacOS",
            "/Applications/Chromium.app/Contents/MacOS",
            "/Applications/Microsoft Edge.app/Contents/MacOS",
        ] {
            v.push(PathBuf::from(d));
        }
    }
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                let base = PathBuf::from(base);
                for sub in [
                    "LibreOffice\\program",
                    "Google\\Chrome\\Application",
                    "Microsoft\\Edge\\Application",
                    "Chromium\\Application",
                    "Tesseract-OCR",
                    "Pandoc",
                    "ffmpeg\\bin",
                    "eSpeak NG",
                ] {
                    v.push(base.join(sub));
                }
            }
        }
    }
    if let Some(h) = home_dir() {
        v.push(h.join(".local/bin"));
        v.push(h.join(".local/share/claudio/bin"));
    }
    v
}

pub fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Expand a leading `~` (works with `/` and `\`).
pub fn expand_home(p: &str) -> String {
    let rest = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\"));
    match (rest, home_dir()) {
        (Some(rest), Some(home)) => home.join(rest).to_string_lossy().into_owned(),
        _ if p == "~" => home_dir()
            .map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.into()),
        _ => p.to_string(),
    }
}

/// Canonicalize without Windows `\\?\` prefixes.
pub fn canonicalize(p: &Path) -> std::io::Result<PathBuf> {
    dunce::canonicalize(p)
}

/// Human-readable OS name, e.g. "Linux Mint LMDE 6", "macOS 15.1", "Windows 11".
pub fn os_pretty_name() -> String {
    if let Some(name) = sysinfo::System::long_os_version() {
        return name;
    }
    std::env::consts::OS.to_string()
}

/// Whether a process with this PID exists.
pub fn pid_alive(pid: u32) -> bool {
    let mut sys = sysinfo::System::new();
    let p = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[p]), true);
    sys.process(p).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_and_which() {
        let home = home_dir().unwrap();
        assert_eq!(PathBuf::from(expand_home("~/x")), home.join("x"));
        assert_eq!(expand_home("/abs"), "/abs");
        let prog = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(which(prog).is_some());
        assert!(which("definitely-not-a-program-xyz").is_none());
        assert!(pid_alive(std::process::id()));
    }
}
