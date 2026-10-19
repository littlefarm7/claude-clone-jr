//! Workflows (TOML) and the cron scheduler.
//!
//! ```toml
//! name = "relatorio-semanal"
//! allowed_tools = ["read_file", "doc_create"]   # pre-approved when unattended
//! [vars]
//! pasta = "~/jarvis-work/vendas"
//! [[steps]]
//! tool = "glob"
//! args = { pattern = "*.csv", path = "{{pasta}}" }
//! save_as = "arquivos"
//! [[steps]]
//! task = "Resuma os CSV listados abaixo em relatorio.pdf:\n{{arquivos}}"
//! ```

use std::collections::BTreeMap;
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Run a single tool directly.
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub args: Option<toml::Value>,
    /// Or give a task to the agent.
    #[serde(default)]
    pub task: Option<String>,
    /// Store the step output in this variable.
    #[serde(default)]
    pub save_as: Option<String>,
    /// Continue even if this step fails.
    #[serde(default)]
    pub continue_on_error: bool,
}

impl Workflow {
    pub fn from_toml(text: &str) -> Result<Self> {
        let w: Workflow = toml::from_str(text).context("workflow TOML inválido")?;
        for (i, s) in w.steps.iter().enumerate() {
            match (&s.tool, &s.task) {
                (Some(_), None) | (None, Some(_)) => {}
                _ => bail!("passo {}: defina exatamente um de `tool` ou `task`", i + 1),
            }
        }
        Ok(w)
    }

    pub fn load(path: &std::path::Path) -> Result<Self> {
        Self::from_toml(
            &std::fs::read_to_string(path).with_context(|| format!("lendo {}", path.display()))?,
        )
    }
}

#[derive(Debug, Clone)]
pub struct WorkflowOutcome {
    pub session_id: String,
    pub ok: bool,
    pub log: Vec<String>,
}

/// Execute a workflow step by step. Tool steps go through the agent's
/// permission system; task steps run as child agent sessions.
pub async fn run_workflow(
    agent: &crate::agent::Agent,
    wf: &Workflow,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<WorkflowOutcome> {
    use crate::store::SessionStatus;
    let store = agent.deps().store.clone();
    let sid = store.create_session(
        &format!("workflow: {}", wf.name),
        &wf.description,
        "workflow",
        None,
    )?;
    let mut vars = wf.vars.clone();
    for v in vars.values_mut() {
        *v = crate::config::expand(v).to_string_lossy().to_string();
    }
    let mut log = Vec::new();
    let mut ok = true;
    for (i, step) in wf.steps.iter().enumerate() {
        if cancel.is_cancelled() {
            log.push("cancelado".into());
            ok = false;
            break;
        }
        let (output, failed) = if let Some(tool) = &step.tool {
            let args = render_args(&step.args, &vars);
            let (out, err) = agent.call_tool(&sid, tool, args, cancel.clone()).await;
            (out, err)
        } else {
            let task = render(step.task.as_deref().unwrap_or(""), &vars);
            let out = agent.start_child(&task, &sid, cancel.clone()).await?;
            let failed = !matches!(
                out.status,
                SessionStatus::Completed | SessionStatus::CompletedUnverified
            );
            (out.summary, failed)
        };
        let label = step.tool.clone().unwrap_or_else(|| "tarefa".into());
        log.push(format!(
            "passo {} ({label}): {}",
            i + 1,
            if failed { "falhou" } else { "ok" }
        ));
        store.append_message(
            &sid,
            &jarvis_llm::Message::assistant(format!("[passo {} — {label}]\n{output}", i + 1)),
        )?;
        if let Some(name) = &step.save_as {
            vars.insert(name.clone(), output.clone());
        }
        if failed && !step.continue_on_error {
            ok = false;
            log.push(format!(
                "interrompido no passo {}: {}",
                i + 1,
                output.chars().take(300).collect::<String>()
            ));
            break;
        }
    }
    let status = if ok {
        SessionStatus::Completed
    } else {
        SessionStatus::Failed
    };
    store.update_session(
        &sid,
        status,
        &Default::default(),
        &Default::default(),
        Some(&log.join("\n")),
    )?;
    Ok(WorkflowOutcome {
        session_id: sid,
        ok,
        log,
    })
}

/// Replace `{{var}}` placeholders.
pub fn render(template: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = template.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

/// Render placeholders inside every string of a TOML value, returning JSON.
pub fn render_args(args: &Option<toml::Value>, vars: &BTreeMap<String, String>) -> Value {
    fn conv(v: &toml::Value, vars: &BTreeMap<String, String>) -> Value {
        match v {
            toml::Value::String(s) => Value::String(render(s, vars)),
            toml::Value::Integer(i) => Value::from(*i),
            toml::Value::Float(f) => Value::from(*f),
            toml::Value::Boolean(b) => Value::Bool(*b),
            toml::Value::Datetime(d) => Value::String(d.to_string()),
            toml::Value::Array(a) => Value::Array(a.iter().map(|x| conv(x, vars)).collect()),
            toml::Value::Table(t) => {
                Value::Object(t.iter().map(|(k, x)| (k.clone(), conv(x, vars))).collect())
            }
        }
    }
    args.as_ref()
        .map(|a| conv(a, vars))
        .unwrap_or_else(|| Value::Object(Default::default()))
}

/// Parse a cron expression. Accepts the classic 5-field form (min hour dom mon dow)
/// by prepending a seconds field.
pub fn parse_cron(expr: &str) -> Result<cron::Schedule> {
    let fields = expr.split_whitespace().count();
    let full = if fields == 5 {
        format!("0 {expr}")
    } else {
        expr.to_string()
    };
    cron::Schedule::from_str(&full)
        .map_err(|e| anyhow::anyhow!("expressão cron inválida '{expr}': {e}"))
}

/// Whether a schedule is due: some fire time lies in (last_run, now].
pub fn is_due(
    expr: &str,
    last_run: Option<&str>,
    created_or_now: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let sched = parse_cron(expr)?;
    let since = last_run
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&chrono::Utc))
        .unwrap_or(created_or_now);
    // Evaluate in local time: users write cron in their wall clock.
    let since_local = since.with_timezone(&chrono::Local);
    Ok(sched
        .after(&since_local)
        .next()
        .map(|t| t.with_timezone(&chrono::Utc) <= now)
        .unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_render() {
        let w = Workflow::from_toml(
            r#"
name = "t"
[vars]
dir = "/tmp/x"
[[steps]]
tool = "glob"
args = { pattern = "*.csv", path = "{{dir}}", n = 3 }
save_as = "files"
[[steps]]
task = "resuma {{files}}"
"#,
        )
        .unwrap();
        let mut vars = w.vars.clone();
        let a = render_args(&w.steps[0].args, &vars);
        assert_eq!(a["path"], "/tmp/x");
        assert_eq!(a["n"], 3);
        vars.insert("files".into(), "a.csv".into());
        assert_eq!(
            render(w.steps[1].task.as_ref().unwrap(), &vars),
            "resuma a.csv"
        );
        assert!(Workflow::from_toml("name='x'\n[[steps]]\n").is_err());
    }

    #[test]
    fn cron_due() {
        use chrono::TimeZone;
        let now = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 30).unwrap();
        let last = (now - chrono::Duration::minutes(2)).to_rfc3339();
        assert!(is_due("* * * * *", Some(&last), now, now).unwrap());
        let last = (now - chrono::Duration::seconds(10)).to_rfc3339();
        assert!(!is_due("* * * * *", Some(&last), now, now).unwrap());
        assert!(parse_cron("nope").is_err());
    }
}
