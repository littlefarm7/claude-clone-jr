//! Permission policy: tool policy ⊕ risk class ⊕ mode ⊕ taint.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{expand, PermissionsConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Read,
    Network,
    Write,
    Execute,
    Destructive,
    Admin,
}

impl Risk {
    pub fn label(&self) -> &'static str {
        match self {
            Risk::Read => "leitura",
            Risk::Network => "rede",
            Risk::Write => "escrita",
            Risk::Execute => "execução",
            Risk::Destructive => "destrutiva",
            Risk::Admin => "administrativa",
        }
    }

    pub fn changes_state(&self) -> bool {
        matches!(
            self,
            Risk::Write | Risk::Execute | Risk::Destructive | Risk::Admin
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Strict,
    #[default]
    Ask,
    AutoSafe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolPolicy {
    Allow,
    Ask,
    Deny,
}

/// A tool's own judgement about one concrete call.
#[derive(Debug, Clone)]
pub struct RiskAssessment {
    pub risk: Risk,
    /// Human-readable description shown in approval prompts.
    pub summary: String,
    /// Tool-level override (e.g. path outside workspace).
    pub force: Option<Forced>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forced {
    Ask(String),
    Deny(String),
}

impl RiskAssessment {
    pub fn new(risk: Risk, summary: impl Into<String>) -> Self {
        Self {
            risk,
            summary: summary.into(),
            force: None,
        }
    }
    pub fn deny(mut self, why: impl Into<String>) -> Self {
        self.force = Some(Forced::Deny(why.into()));
        self
    }
    pub fn ask(mut self, why: impl Into<String>) -> Self {
        if !matches!(self.force, Some(Forced::Deny(_))) {
            self.force = Some(Forced::Ask(why.into()));
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask { reason: String, allow_session: bool },
    Deny { reason: String },
}

/// Grants given with "allow for this session", keyed by tool + risk.
/// Grants given while the session was tainted are tracked separately: a
/// grant made before reading untrusted content does not survive the taint.
#[derive(Debug, Default, Clone)]
pub struct SessionGrants {
    pub clean: HashSet<(String, Risk)>,
    pub tainted: HashSet<(String, Risk)>,
}

impl SessionGrants {
    pub fn grant(&mut self, tool: &str, risk: Risk, tainted: bool) {
        let key = (tool.to_string(), risk);
        if tainted {
            self.tainted.insert(key);
        } else {
            self.clean.insert(key);
        }
    }

    fn covers(&self, tool: &str, risk: Risk, tainted: bool) -> bool {
        let key = (tool.to_string(), risk);
        self.tainted.contains(&key) || (!tainted && self.clean.contains(&key))
    }
}

pub fn decide(
    cfg: &PermissionsConfig,
    tool: &str,
    ra: &RiskAssessment,
    tainted: bool,
    grants: &SessionGrants,
) -> Decision {
    let policy = cfg.tools.get(tool).copied();
    if policy == Some(ToolPolicy::Deny) {
        return Decision::Deny {
            reason: format!("ferramenta '{tool}' negada na configuração"),
        };
    }
    if let Some(Forced::Deny(r)) = &ra.force {
        return Decision::Deny { reason: r.clone() };
    }
    if ra.risk == Risk::Admin {
        if cfg.mode == Mode::Strict {
            return Decision::Deny {
                reason: "ação administrativa bloqueada no modo strict".into(),
            };
        }
        // Never auto-approved and never granted for the whole session.
        return Decision::Ask {
            reason: "ação administrativa (privilégios elevados)".into(),
            allow_session: false,
        };
    }
    let base_ask = |reason: String| Decision::Ask {
        reason,
        allow_session: true,
    };
    if let Some(Forced::Ask(r)) = &ra.force {
        return base_ask(r.clone());
    }
    if ra.risk == Risk::Destructive {
        return Decision::Ask {
            reason: "ação destrutiva".into(),
            allow_session: false,
        };
    }
    if grants.covers(tool, ra.risk, tainted) {
        return Decision::Allow;
    }
    if tainted && ra.risk != Risk::Read {
        return base_ask(
            "a sessão leu conteúdo externo não confiável (proteção contra injeção de prompt)"
                .into(),
        );
    }
    if policy == Some(ToolPolicy::Allow) {
        return Decision::Allow;
    }
    if policy == Some(ToolPolicy::Ask) {
        return base_ask(format!("política de '{tool}' é 'ask'"));
    }
    match (cfg.mode, ra.risk) {
        (_, Risk::Read) => Decision::Allow,
        (Mode::Strict, _) => base_ask(format!("modo strict: ação de {}", ra.risk.label())),
        (_, Risk::Network) => Decision::Allow,
        (Mode::AutoSafe, Risk::Write) => Decision::Allow,
        (_, r) => base_ask(format!("ação de {}", r.label())),
    }
}

// ---------------------------------------------------------------------------
// Path guard
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PathGuard {
    pub workspace: Vec<PathBuf>,
    pub read_roots: Vec<PathBuf>,
    pub blocked: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathVerdict {
    Inside,
    ReadableOutside,
    Outside,
    Blocked(String),
}

impl PathGuard {
    pub fn from_config(cfg: &PermissionsConfig) -> Self {
        let canon =
            |p: &PathBuf| jarvis_llm::platform::canonicalize(p).unwrap_or_else(|_| normalize(p));
        let workspace: Vec<PathBuf> = cfg
            .workspace_roots
            .iter()
            .map(|r| canon(&expand(r)))
            .collect();
        let read_roots = cfg.read_roots.iter().map(|r| canon(&expand(r))).collect();
        let mut blocked: Vec<PathBuf> = Vec::new();
        if let Some(home) = dirs::home_dir() {
            for p in [
                ".ssh",
                ".gnupg",
                ".password-store",
                ".local/share/keyrings",
                ".aws",
                ".netrc",
                ".docker/config.json",
                ".kube",
                // macOS
                "Library/Keychains",
                "Library/Cookies",
                // Windows
                "AppData/Roaming/Microsoft/Credentials",
                "AppData/Roaming/Microsoft/Protect",
                "AppData/Local/Microsoft/Credentials",
            ] {
                blocked.push(home.join(p));
            }
        }
        if cfg!(windows) {
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
            blocked.push(PathBuf::from(root).join("System32").join("config"));
        }
        blocked.push(crate::config::config_dir().join("secrets"));
        blocked.push(crate::config::data_dir().join("audit.jsonl"));
        blocked.push(crate::config::data_dir().join("jarvis.db"));
        for p in ["/etc/shadow", "/etc/gshadow", "/etc/sudoers", "/root"] {
            blocked.push(PathBuf::from(p));
        }
        blocked.extend(cfg.blocked_paths.iter().map(|p| expand(p)));
        // Compare against canonical forms too (/var → /private/var on macOS, symlinked homes).
        let canonical: Vec<PathBuf> = blocked
            .iter()
            .filter_map(|b| jarvis_llm::platform::canonicalize(b).ok())
            .filter(|c| !blocked.contains(c))
            .collect();
        blocked.extend(canonical);
        Self {
            workspace,
            read_roots,
            blocked,
        }
    }

    /// Main working directory (first workspace root).
    pub fn cwd(&self) -> PathBuf {
        self.workspace
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Resolve a user/model-supplied path to an absolute, canonical path.
    /// Non-existent tails are normalized lexically after canonicalizing the
    /// longest existing ancestor, so symlinks cannot escape.
    pub fn resolve(&self, input: &str) -> PathBuf {
        let p = expand(input);
        let abs = if p.is_absolute() {
            p
        } else {
            self.cwd().join(p)
        };
        let abs = normalize(&abs);
        let mut existing = abs.clone();
        let mut tail = Vec::new();
        while !existing.exists() {
            match (existing.file_name(), existing.parent()) {
                (Some(name), Some(parent)) => {
                    tail.push(name.to_os_string());
                    existing = parent.to_path_buf();
                }
                _ => break,
            }
        }
        let mut out = jarvis_llm::platform::canonicalize(&existing).unwrap_or(existing);
        for t in tail.iter().rev() {
            out.push(t);
        }
        out
    }

    pub fn classify(&self, path: &Path) -> PathVerdict {
        for b in &self.blocked {
            if path.starts_with(b) {
                return PathVerdict::Blocked(format!(
                    "caminho sensível bloqueado: {}",
                    b.display()
                ));
            }
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if self.workspace.iter().any(|w| path.starts_with(w)) {
            return PathVerdict::Inside;
        }
        if name == ".env"
            || name.ends_with(".pem")
            || name.starts_with("id_rsa")
            || name.starts_with("id_ed25519")
        {
            return PathVerdict::Blocked("arquivo de credenciais fora do workspace".into());
        }
        if self.read_roots.iter().any(|w| path.starts_with(w)) {
            return PathVerdict::ReadableOutside;
        }
        PathVerdict::Outside
    }

    /// Risk assessment helper for read access.
    pub fn assess_read(&self, path: &Path, summary: String) -> RiskAssessment {
        let ra = RiskAssessment::new(Risk::Read, summary);
        match self.classify(path) {
            PathVerdict::Inside | PathVerdict::ReadableOutside => ra,
            PathVerdict::Outside => {
                ra.ask(format!("leitura fora do workspace: {}", path.display()))
            }
            PathVerdict::Blocked(r) => ra.deny(r),
        }
    }

    /// Risk assessment helper for write access (writes outside are denied).
    pub fn assess_write(&self, path: &Path, risk: Risk, summary: String) -> RiskAssessment {
        let ra = RiskAssessment::new(risk, summary);
        match self.classify(path) {
            PathVerdict::Inside => ra,
            PathVerdict::Blocked(r) => ra.deny(r),
            _ => ra.deny(format!(
                "escrita fora do workspace não é permitida: {} (adicione a pasta em permissions.workspace_roots)",
                path.display()
            )),
        }
    }
}

/// Lexical normalization (resolves `.` and `..` without touching the disk).
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(mode: Mode) -> PermissionsConfig {
        PermissionsConfig {
            mode,
            ..Default::default()
        }
    }

    #[test]
    fn matrix() {
        let g = SessionGrants::default();
        let c = cfg(Mode::Ask);
        let ra = |r| RiskAssessment::new(r, "x");
        assert_eq!(decide(&c, "t", &ra(Risk::Read), false, &g), Decision::Allow);
        assert_eq!(
            decide(&c, "t", &ra(Risk::Network), false, &g),
            Decision::Allow
        );
        assert!(matches!(
            decide(&c, "t", &ra(Risk::Network), true, &g),
            Decision::Ask { .. }
        ));
        assert!(matches!(
            decide(&c, "t", &ra(Risk::Write), false, &g),
            Decision::Ask { .. }
        ));
        assert!(matches!(
            decide(&c, "t", &ra(Risk::Admin), false, &g),
            Decision::Ask {
                allow_session: false,
                ..
            }
        ));
        let auto = cfg(Mode::AutoSafe);
        assert_eq!(
            decide(&auto, "t", &ra(Risk::Write), false, &g),
            Decision::Allow
        );
        assert!(matches!(
            decide(&auto, "t", &ra(Risk::Write), true, &g),
            Decision::Ask { .. }
        ));
        assert!(matches!(
            decide(&auto, "t", &ra(Risk::Execute), false, &g),
            Decision::Ask { .. }
        ));
        assert!(matches!(
            decide(&cfg(Mode::Strict), "t", &ra(Risk::Admin), false, &g),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn explicit_policy_and_grants() {
        let mut c = cfg(Mode::Ask);
        c.tools.insert("shell".into(), ToolPolicy::Deny);
        let ra = RiskAssessment::new(Risk::Read, "x");
        assert!(matches!(
            decide(&c, "shell", &ra, false, &SessionGrants::default()),
            Decision::Deny { .. }
        ));
        let mut g = SessionGrants::default();
        g.grant("write_file", Risk::Write, false);
        let w = RiskAssessment::new(Risk::Write, "x");
        assert_eq!(decide(&c, "write_file", &w, false, &g), Decision::Allow);
        // A clean grant does not survive taint; a grant made while tainted does.
        assert!(matches!(
            decide(&c, "write_file", &w, true, &g),
            Decision::Ask { .. }
        ));
        g.grant("write_file", Risk::Write, true);
        assert_eq!(decide(&c, "write_file", &w, true, &g), Decision::Allow);
        // Admin is never covered by a session grant.
        g.grant("shell2", Risk::Admin, false);
        let a = RiskAssessment::new(Risk::Admin, "x");
        assert!(matches!(
            decide(&c, "shell2", &a, false, &g),
            Decision::Ask { .. }
        ));
    }

    #[test]
    fn guard_blocks_escape() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let c = PermissionsConfig {
            workspace_roots: vec![ws.to_string_lossy().into()],
            ..Default::default()
        };
        let g = PathGuard::from_config(&c);
        let inside = g.resolve("sub/new.txt");
        assert_eq!(g.classify(&inside), PathVerdict::Inside);
        let esc = g.resolve("../outside.txt");
        assert_eq!(g.classify(&esc), PathVerdict::Outside);
        // symlink inside workspace pointing outside must not count as inside
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path(), ws.join("link")).unwrap();
            let via = g.resolve("link/secret.txt");
            assert_eq!(g.classify(&via), PathVerdict::Outside);
        }
        let ssh = g.resolve("~/.ssh/id_rsa");
        assert!(matches!(g.classify(&ssh), PathVerdict::Blocked(_)));
        assert!(matches!(
            g.assess_write(&esc, Risk::Write, "w".into()).force,
            Some(Forced::Deny(_))
        ));
    }
}
