//! API keys entered in the settings screen.
//!
//! Stored in `<config>/secrets/api_keys.toml` with 0600 permissions (directory
//! 0700). The directory is on the agent's blocked-path list, so tools can
//! never read it. Environment variables still take precedence.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{config_dir, set_private, Config};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Secrets {
    /// Model name (from `[[models]]`) → API key.
    #[serde(default)]
    pub keys: BTreeMap<String, String>,
}

pub fn secrets_path() -> PathBuf {
    config_dir().join("secrets").join("api_keys.toml")
}

/// Secrets live next to the config file in use (`--config` support).
pub fn secrets_path_for(cfg: &Config) -> PathBuf {
    match cfg.source.as_deref().and_then(Path::parent) {
        Some(dir) => dir.join("secrets").join("api_keys.toml"),
        None => secrets_path(),
    }
}

impl Secrets {
    pub fn load() -> Result<Self> {
        Self::load_from(&secrets_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        if file_is_shared(path) {
            anyhow::bail!(
                "{} pode ser lido por outros usuários; chaves ignoradas (use chmod 600)",
                path.display()
            );
        }
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).context("arquivo de chaves inválido")
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&secrets_path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        let dir = path.parent().context("caminho sem pasta")?;
        std::fs::create_dir_all(dir)?;
        set_private_dir(dir);
        // Create the file private before writing any secret into it.
        std::fs::write(path, "")?;
        set_private(path);
        std::fs::write(path, toml::to_string(self)?)?;
        set_private(path);
        Ok(())
    }

    pub fn set(&mut self, model: &str, key: &str) {
        let key = key.trim();
        if key.is_empty() {
            self.keys.remove(model);
        } else {
            self.keys.insert(model.to_string(), key.to_string());
        }
    }

    /// Fill `api_key` of models that have none yet.
    pub fn apply(&self, cfg: &mut Config) {
        for m in &mut cfg.models {
            if m.api_key.is_none() {
                if let Some(k) = self.keys.get(&m.name) {
                    m.api_key = Some(k.clone());
                }
            }
        }
    }
}

#[cfg(unix)]
fn file_is_shared(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o077 != 0)
        .unwrap_or(false)
}
#[cfg(not(unix))]
fn file_is_shared(_: &Path) -> bool {
    false
}

#[cfg(unix)]
fn set_private_dir(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn set_private_dir(_: &Path) {}

/// Show only the first and last characters of a key.
pub fn mask(key: &str) -> String {
    let n = key.chars().count();
    if n <= 8 {
        return "•".repeat(n);
    }
    let start: String = key.chars().take(4).collect();
    let end: String = key.chars().skip(n - 4).collect();
    format!("{start}…{end}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_private_and_apply() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secrets").join("api_keys.toml");
        let mut s = Secrets::default();
        s.set("claude", "  sk-ant-123456789  ");
        s.save_to(&p).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let loaded = Secrets::load_from(&p).unwrap();
        assert_eq!(loaded.keys["claude"], "sk-ant-123456789");
        let mut cfg = Config::default();
        loaded.apply(&mut cfg);
        assert_eq!(
            cfg.model("claude").unwrap().api_key.as_deref(),
            Some("sk-ant-123456789")
        );
        assert_eq!(mask("sk-ant-123456789"), "sk-a…6789");
        let mut s = loaded;
        s.set("claude", "");
        assert!(s.keys.is_empty());
    }
}
