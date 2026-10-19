//! Skills: folders with a SKILL.md (YAML-like front matter + instructions).

use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use jarvis_core::config::{expand, Config};
use jarvis_core::permissions::{Risk, RiskAssessment};
use jarvis_core::tool::{arg_str, Tool, ToolContext, ToolOutput};
use jarvis_llm::ToolSpec;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
}

pub fn parse_front_matter(text: &str) -> (Option<String>, Option<String>) {
    let mut name = None;
    let mut desc = None;
    let mut lines = text.lines();
    if lines.next().map(|l| l.trim()) != Some("---") {
        return (None, None);
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().trim_matches(['"', '\'']).to_string();
            match k.trim() {
                "name" => name = Some(v),
                "description" => desc = Some(v),
                _ => {}
            }
        }
    }
    (name, desc)
}

pub fn discover(cfg: &Config) -> Vec<Skill> {
    let mut out = Vec::new();
    for d in &cfg.skills_dirs {
        let Ok(rd) = std::fs::read_dir(expand(d)) else {
            continue;
        };
        for e in rd.flatten() {
            let file = e.path().join("SKILL.md");
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let (name, desc) = parse_front_matter(&text);
            out.push(Skill {
                name: name.unwrap_or_else(|| e.file_name().to_string_lossy().to_string()),
                description: desc.unwrap_or_default(),
                dir: e.path(),
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn catalog(skills: &[Skill]) -> String {
    let mut s = String::from("Skills disponíveis (carregue com skill_load antes de usar):\n");
    for k in skills {
        s.push_str(&format!("- {}: {}\n", k.name, k.description));
    }
    s
}

pub struct SkillLoad {
    pub skills: Vec<Skill>,
}

#[async_trait]
impl Tool for SkillLoad {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "skill_load".into(),
            description: "Carrega as instruções completas de uma skill instalada pelo usuário."
                .into(),
            parameters: json!({"type":"object","properties":{"name":{"type":"string","enum": self.skills.iter().map(|s| s.name.clone()).collect::<Vec<_>>()}},"required":["name"]}),
        }
    }
    fn assess(&self, args: &Value, _: &ToolContext) -> RiskAssessment {
        RiskAssessment::new(
            Risk::Read,
            format!("carregar skill {}", arg_str(args, "name").unwrap_or("")),
        )
    }
    async fn call(&self, args: Value, _: &ToolContext) -> Result<ToolOutput> {
        let name = arg_str(&args, "name").unwrap_or("");
        let Some(s) = self.skills.iter().find(|s| s.name == name) else {
            return Ok(ToolOutput::err(format!("skill '{name}' não existe")));
        };
        let body = std::fs::read_to_string(s.dir.join("SKILL.md"))?;
        let files: Vec<String> = std::fs::read_dir(&s.dir)?
            .flatten()
            .map(|e| e.path().display().to_string())
            .collect();
        Ok(ToolOutput::ok(format!(
            "{body}\n\nArquivos da skill:\n{}",
            files.join("\n")
        )))
    }
}
