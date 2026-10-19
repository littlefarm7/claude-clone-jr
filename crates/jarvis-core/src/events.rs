//! Events emitted by the agent and the approval interface used by front-ends.

use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::permissions::Risk;
use crate::plan::Plan;
use crate::store::SessionStatus;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    SessionStarted { session_id: String, model: String, resumed: bool },
    ModelSelected { model: String, reason: String },
    ModelCall { n: u32, phase: String },
    AssistantText { text: String },
    PlanUpdated { plan: Plan },
    ToolCall { id: String, name: String, args: Value, risk: Risk, summary: String },
    ToolDecision { id: String, name: String, decision: String },
    ToolResult { id: String, name: String, ok: bool, preview: String, ms: u64 },
    Warning { message: String },
    VerificationStarted { round: u32 },
    VerificationResult { passed: bool, notes: String },
    LimitReached { limit: String },
    Finished { status: SessionStatus, summary: String },
    Log { message: String },
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: AgentEvent);
}

/// Discards events.
pub struct NullSink;
impl EventSink for NullSink {
    fn emit(&self, _: AgentEvent) {}
}

/// Collects events (tests, sub-agents).
#[derive(Default)]
pub struct VecSink(pub Mutex<Vec<AgentEvent>>);
impl EventSink for VecSink {
    fn emit(&self, e: AgentEvent) {
        self.0.lock().unwrap().push(e);
    }
}

/// Forwards events to an unbounded channel (GUI).
pub struct ChannelSink(pub tokio::sync::mpsc::UnboundedSender<AgentEvent>);
impl EventSink for ChannelSink {
    fn emit(&self, e: AgentEvent) {
        let _ = self.0.send(e);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub session_id: String,
    pub tool: String,
    pub risk: Risk,
    pub summary: String,
    pub args: Value,
    pub reason: String,
    /// Whether "allow for the rest of this session" may be offered.
    pub allow_session: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalDecision {
    AllowOnce,
    AllowSession,
    Deny,
}

#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, req: ApprovalRequest) -> ApprovalDecision;
}

/// Non-interactive: denies everything that needs approval, except tools
/// explicitly pre-approved (e.g. for a scheduled job). Admin is never pre-approved.
pub struct NonInteractive {
    pub preapproved: Vec<String>,
}

#[async_trait]
impl Approver for NonInteractive {
    async fn approve(&self, req: ApprovalRequest) -> ApprovalDecision {
        if req.risk != Risk::Admin && req.risk != Risk::Destructive && self.preapproved.iter().any(|t| t == &req.tool) {
            ApprovalDecision::AllowOnce
        } else {
            ApprovalDecision::Deny
        }
    }
}

/// Approves everything except admin actions (tests / explicit `--yes`).
pub struct AutoApprove;

#[async_trait]
impl Approver for AutoApprove {
    async fn approve(&self, req: ApprovalRequest) -> ApprovalDecision {
        if req.risk == Risk::Admin {
            ApprovalDecision::Deny
        } else {
            ApprovalDecision::AllowOnce
        }
    }
}
