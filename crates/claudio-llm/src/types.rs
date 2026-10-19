use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// One piece of a message. Provider adapters translate these to wire formats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
        /// Opaque provider data that must be echoed back (e.g. Gemini thought signatures).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_meta: Option<Value>,
    },
    ToolResult {
        call_id: String,
        name: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    Image {
        mime: String,
        data_b64: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            parts: vec![Part::Text { text: text.into() }],
        }
    }
    pub fn system(text: impl Into<String>) -> Self {
        Self::text(Role::System, text)
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self::text(Role::User, text)
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::text(Role::Assistant, text)
    }

    /// Concatenated text parts.
    pub fn text_content(&self) -> String {
        let mut out = String::new();
        for p in &self.parts {
            if let Part::Text { text } = p {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }

    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::ToolCall {
                    id,
                    name,
                    arguments,
                    ..
                } => Some(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                }),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub message: Message,
    pub usage: Usage,
    pub stop: StopReason,
}

/// Split system messages from the rest (most APIs want system separately).
pub fn split_system(messages: &[Message]) -> (String, Vec<&Message>) {
    let mut sys = String::new();
    let mut rest = Vec::new();
    for m in messages {
        if m.role == Role::System {
            if !sys.is_empty() {
                sys.push_str("\n\n");
            }
            sys.push_str(&m.text_content());
        } else {
            rest.push(m);
        }
    }
    (sys, rest)
}

pub fn new_call_id() -> String {
    format!("call_{}", uuid::Uuid::new_v4().simple())
}
