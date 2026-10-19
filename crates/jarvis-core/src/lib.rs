//! Jarvis Core: the agent loop, permissions, persistence and memory.

pub mod agent;
pub mod audit;
pub mod config;
pub mod consent;
pub mod events;
pub mod memory;
pub mod permissions;
pub mod plan;
pub mod safety;
pub mod store;
pub mod tool;
pub mod workflow;

pub use agent::{Agent, AgentDeps, RunOutcome};
pub use config::Config;
pub use events::{AgentEvent, ApprovalDecision, ApprovalRequest, Approver, EventSink};
pub use permissions::{Risk, RiskAssessment};
pub use store::{SessionStatus, Store};
pub use tool::{Tool, ToolContext, ToolOutput, ToolRegistry};
