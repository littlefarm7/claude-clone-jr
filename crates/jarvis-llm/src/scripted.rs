//! Deterministic provider for tests and offline demos.

use std::collections::VecDeque;
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::{ChatRequest, ChatResponse, Message, Part, Provider, Role, StopReason, Usage};

type Responder = Box<dyn Fn(&ChatRequest) -> Message + Send + Sync>;

pub struct Scripted {
    queue: Mutex<VecDeque<Responder>>,
    pub requests: Mutex<Vec<ChatRequest>>,
}

impl Default for Scripted {
    fn default() -> Self {
        Self::new()
    }
}

impl Scripted {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Queue a fixed reply.
    pub fn push(&self, msg: Message) -> &Self {
        self.queue
            .lock()
            .unwrap()
            .push_back(Box::new(move |_| msg.clone()));
        self
    }

    /// Queue a reply computed from the request (e.g. to inspect tool results).
    pub fn push_fn(&self, f: impl Fn(&ChatRequest) -> Message + Send + Sync + 'static) -> &Self {
        self.queue.lock().unwrap().push_back(Box::new(f));
        self
    }

    pub fn call(name: &str, args: serde_json::Value) -> Message {
        Message {
            role: Role::Assistant,
            parts: vec![Part::ToolCall {
                id: crate::new_call_id(),
                name: name.into(),
                arguments: args,
                provider_meta: None,
            }],
        }
    }

    pub fn remaining(&self) -> usize {
        self.queue.lock().unwrap().len()
    }
}

#[async_trait]
impl Provider for Scripted {
    fn id(&self) -> String {
        "scripted:test".into()
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let f = self
            .queue
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow!("script esgotado"))?;
        let message = f(&req);
        self.requests.lock().unwrap().push(req);
        let stop = if message.tool_calls().is_empty() {
            StopReason::EndTurn
        } else {
            StopReason::ToolUse
        };
        Ok(ChatResponse {
            message,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
            },
            stop,
        })
    }
}
