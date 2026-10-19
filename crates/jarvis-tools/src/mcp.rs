//! Model Context Protocol client (stdio transport, newline-delimited JSON-RPC).

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use jarvis_core::config::McpServerConfig;
use jarvis_core::permissions::{Risk, RiskAssessment};
use jarvis_core::tool::{Tool, ToolContext, ToolOutput, ToolRegistry};
use jarvis_llm::ToolSpec;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};

pub const PROTOCOL_VERSION: &str = "2025-06-18";

type Pending = Arc<std::sync::Mutex<HashMap<i64, oneshot::Sender<Value>>>>;

pub struct McpClient {
    pub name: String,
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicI64,
    _child: Mutex<Child>,
    pub server_info: Value,
}

impl McpClient {
    pub async fn spawn(cfg: &McpServerConfig) -> Result<Self> {
        // Resolve `npx` → `npx.cmd` on Windows, app-bundle paths on macOS, etc.
        let expanded = jarvis_core::config::expand(&cfg.command);
        let program = crate::shell::which(&expanded.to_string_lossy()).unwrap_or(expanded);
        let mut cmd = Command::new(program);
        cmd.args(&cfg.args)
            .env_clear()
            .envs(crate::shell::scrubbed_env())
            .envs(cfg.env.iter())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("iniciando {}", cfg.command))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let pending: Pending = Arc::default();
        let p2 = pending.clone();
        let (req_tx, mut req_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if v.get("method").is_some() {
                    if v.get("id").is_some() {
                        let _ = req_tx.send(v); // server -> client request
                    }
                    continue;
                }
                if let Some(id) = v["id"].as_i64() {
                    if let Some(tx) = p2.lock().unwrap().remove(&id) {
                        let _ = tx.send(v);
                    }
                }
            }
        });
        let client = Self {
            name: cfg.name.clone(),
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicI64::new(1),
            _child: Mutex::new(child),
            server_info: Value::Null,
        };
        let client = {
            let init = client
                .request(
                    "initialize",
                    json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "jarvis-core", "version": env!("CARGO_PKG_VERSION")}}),
                )
                .await?;
            client
                .notify("notifications/initialized", json!({}))
                .await?;
            Self {
                server_info: init,
                ..client
            }
        };
        // We declare no client capabilities, so server-initiated requests are only logged.
        let name = client.name.clone();
        tokio::spawn(async move {
            while let Some(req) = req_rx.recv().await {
                tracing::debug!("MCP {name}: pedido do servidor ignorado: {}", req["method"]);
            }
        });
        Ok(client)
    }

    async fn send(&self, v: &Value) -> Result<()> {
        let mut s = self.stdin.lock().await;
        s.write_all(format!("{v}\n").as_bytes()).await?;
        s.flush().await?;
        Ok(())
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        let resp = tokio::time::timeout(Duration::from_secs(120), rx)
            .await
            .map_err(|_| anyhow!("MCP {}: tempo esgotado em {method}", self.name))?
            .map_err(|_| anyhow!("MCP {}: servidor encerrou", self.name))?;
        if let Some(err) = resp.get("error") {
            bail!("MCP {} {method}: {}", self.name, err);
        }
        Ok(resp["result"].clone())
    }

    pub async fn list_tools(&self) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let r = self.request("tools/list", params).await?;
            out.extend(r["tools"].as_array().cloned().unwrap_or_default());
            match r["nextCursor"].as_str() {
                Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
                _ => break,
            }
        }
        Ok(out)
    }

    pub async fn call_tool(&self, name: &str, args: Value) -> Result<(String, bool)> {
        let r = self
            .request("tools/call", json!({"name": name, "arguments": args}))
            .await?;
        let mut text = Vec::new();
        for c in r["content"].as_array().cloned().unwrap_or_default() {
            match c["type"].as_str() {
                Some("text") => text.push(c["text"].as_str().unwrap_or("").to_string()),
                Some(t) => text.push(format!("[conteúdo {t} omitido]")),
                None => {}
            }
        }
        if text.is_empty() {
            if let Some(s) = r.get("structuredContent") {
                text.push(s.to_string());
            }
        }
        Ok((text.join("\n"), r["isError"].as_bool().unwrap_or(false)))
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub struct McpTool {
    client: Arc<McpClient>,
    remote_name: String,
    spec: ToolSpec,
}

pub async fn register_tools(client: &Arc<McpClient>, reg: &mut ToolRegistry) -> Result<usize> {
    let tools = client.list_tools().await?;
    for t in &tools {
        let remote = t["name"].as_str().unwrap_or("").to_string();
        let mut name = format!("mcp__{}__{}", sanitize(&client.name), sanitize(&remote));
        name.truncate(64);
        let mut schema = t["inputSchema"].clone();
        if !schema.is_object() {
            schema = json!({"type": "object", "properties": {}});
        }
        reg.register(Arc::new(McpTool {
            client: client.clone(),
            remote_name: remote,
            spec: ToolSpec {
                name,
                description: format!(
                    "[MCP {}] {}",
                    client.name,
                    t["description"].as_str().unwrap_or("")
                ),
                parameters: schema,
            },
        }));
    }
    Ok(tools.len())
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }
    fn assess(&self, args: &Value, _: &ToolContext) -> RiskAssessment {
        let a: String = args.to_string().chars().take(200).collect();
        RiskAssessment::new(
            Risk::Execute,
            format!("MCP {}: {} {a}", self.client.name, self.remote_name),
        )
    }
    async fn call(&self, args: Value, _: &ToolContext) -> Result<ToolOutput> {
        let (text, is_error) = self.client.call_tool(&self.remote_name, args).await?;
        let mut out = ToolOutput::ok(text).untrusted(format!("mcp:{}", self.client.name));
        out.is_error = is_error;
        Ok(out)
    }
}
