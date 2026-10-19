//! Append-only, hash-chained audit log (JSON Lines).

use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::safety::{redact, truncate_middle};

pub struct Audit {
    path: PathBuf,
    last_hash: Mutex<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AuditEntry {
    pub session: String,
    pub event: String,
    pub tool: Option<String>,
    pub risk: Option<String>,
    pub decision: Option<String>,
    pub args: Option<Value>,
    pub ok: Option<bool>,
    pub ms: Option<u64>,
    pub detail: Option<String>,
}

impl Audit {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut last = "genesis".to_string();
        if path.exists() {
            let f = std::fs::File::open(&path)?;
            for line in std::io::BufReader::new(f).lines() {
                let line = line?;
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if let Some(h) = v["hash"].as_str() {
                        last = h.to_string();
                    }
                }
            }
        } else {
            std::fs::File::create(&path)?;
            crate::config::set_private(&path);
        }
        Ok(Self { path, last_hash: Mutex::new(last) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn record(&self, e: AuditEntry) -> Result<()> {
        let args = e.args.map(|a| {
            let s = truncate_middle(&redact(&a.to_string()), 2000);
            Value::String(s)
        });
        let mut last = self.last_hash.lock().unwrap();
        let mut v = json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "session": e.session,
            "event": e.event,
            "tool": e.tool,
            "risk": e.risk,
            "decision": e.decision,
            "args": args,
            "ok": e.ok,
            "ms": e.ms,
            "detail": e.detail.map(|d| truncate_middle(&redact(&d), 1000)),
            "prev_hash": *last,
        });
        let hash = hex::encode(Sha256::digest(v.to_string().as_bytes()));
        v["hash"] = Value::String(hash.clone());
        let mut f = OpenOptions::new().append(true).create(true).open(&self.path)?;
        writeln!(f, "{v}")?;
        *last = hash;
        Ok(())
    }

    /// Recompute the chain. Returns the number of verified entries.
    pub fn verify(path: &Path) -> Result<usize> {
        let f = std::fs::File::open(path)?;
        let mut prev = "genesis".to_string();
        let mut n = 0;
        for (i, line) in std::io::BufReader::new(f).lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let mut v: Value = serde_json::from_str(&line)?;
            let hash = v["hash"].as_str().unwrap_or("").to_string();
            if v["prev_hash"].as_str() != Some(prev.as_str()) {
                bail!("linha {}: encadeamento quebrado (linha removida ou reordenada)", i + 1);
            }
            v.as_object_mut().unwrap().remove("hash");
            let calc = hex::encode(Sha256::digest(v.to_string().as_bytes()));
            if calc != hash {
                bail!("linha {}: conteúdo alterado", i + 1);
            }
            prev = hash;
            n += 1;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_detects_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jsonl");
        let a = Audit::open(&p).unwrap();
        for i in 0..3 {
            a.record(AuditEntry { session: "s".into(), event: format!("e{i}"), args: Some(json!({"k": "sk-ant-aaaaaaaaaaaaaaaaaaaa"})), ..Default::default() })
                .unwrap();
        }
        assert_eq!(Audit::verify(&p).unwrap(), 3);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("sk-ant-aaaa"));
        // Reopen continues the chain.
        let a = Audit::open(&p).unwrap();
        a.record(AuditEntry { session: "s".into(), event: "e3".into(), ..Default::default() }).unwrap();
        assert_eq!(Audit::verify(&p).unwrap(), 4);
        std::fs::write(&p, text.replacen("e1", "eX", 1)).unwrap();
        assert!(Audit::verify(&p).is_err());
    }
}
