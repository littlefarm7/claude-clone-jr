//! Execution plan maintained by the model through the `update_plan` tool.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanStep {
    pub title: String,
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub steps: Vec<PlanStep>,
}

impl Plan {
    pub fn unfinished(&self) -> Vec<&PlanStep> {
        self.steps
            .iter()
            .filter(|s| matches!(s.status, StepStatus::Pending | StepStatus::InProgress))
            .collect()
    }

    pub fn progress(&self) -> (usize, usize) {
        let done = self
            .steps
            .iter()
            .filter(|s| matches!(s.status, StepStatus::Completed | StepStatus::Skipped))
            .count();
        (done, self.steps.len())
    }

    pub fn render(&self) -> String {
        self.steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mark = match s.status {
                    StepStatus::Pending => "[ ]",
                    StepStatus::InProgress => "[~]",
                    StepStatus::Completed => "[x]",
                    StepStatus::Skipped => "[-]",
                    StepStatus::Failed => "[!]",
                };
                let note = s.note.as_deref().map(|n| format!(" — {n}")).unwrap_or_default();
                format!("{mark} {}. {}{note}", i + 1, s.title)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn plan_tool_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "steps": {
                "type": "array",
                "description": "Lista completa de passos (substitui o plano anterior).",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": {"type": "string"},
                        "status": {"type": "string", "enum": ["pending", "in_progress", "completed", "skipped", "failed"]},
                        "note": {"type": "string"}
                    },
                    "required": ["title", "status"]
                }
            }
        },
        "required": ["steps"]
    })
}
