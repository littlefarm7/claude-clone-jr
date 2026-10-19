//! SQLite persistence: sessions, messages, memories, schedules, consents.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use jarvis_llm::Message;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::plan::Plan;

pub struct Store {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Running,
    Interrupted,
    Completed,
    CompletedUnverified,
    Failed,
    Cancelled,
    LimitReached,
}

impl SessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Interrupted => "interrupted",
            Self::Completed => "completed",
            Self::CompletedUnverified => "completed_unverified",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::LimitReached => "limit_reached",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "completed" => Self::Completed,
            "completed_unverified" => Self::CompletedUnverified,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "limit_reached" => Self::LimitReached,
            _ => Self::Interrupted,
        }
    }
    pub fn resumable(&self) -> bool {
        matches!(
            self,
            Self::Running | Self::Interrupted | Self::LimitReached | Self::Failed | Self::Cancelled
        )
    }
}

/// Counters and phase persisted with the session so a resume continues the budget.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionState {
    pub iterations: u32,
    pub model_calls: u32,
    pub tool_calls: u32,
    pub tokens: u64,
    pub elapsed_secs: u64,
    pub verify_rounds: u32,
    pub plan_nags: u32,
    pub verifying: bool,
    pub verify_tool_calls: u32,
    pub state_changed: bool,
    pub tainted: bool,
    #[serde(default)]
    pub pass_rejected: bool,
    #[serde(default)]
    pub verdict_nags: u32,
    /// Final answer given before verification started.
    #[serde(default)]
    pub pending_answer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub title: String,
    pub task: String,
    pub status: SessionStatus,
    pub model: String,
    pub parent_id: Option<String>,
    pub plan: Plan,
    pub state: SessionState,
    pub summary: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: i64,
    pub kind: String,
    pub content: String,
    pub tags: String,
    pub source: String,
    pub created_at: String,
    #[serde(skip)]
    pub embedding: Option<Vec<f32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRecord {
    pub id: i64,
    pub name: String,
    pub cron: String,
    /// Either a task prompt or a workflow file path.
    pub kind: String,
    pub payload: String,
    pub enabled: bool,
    pub last_run: Option<String>,
    pub allowed_tools: String,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let conn = Connection::open(path).with_context(|| format!("abrindo {}", path.display()))?;
        crate::config::set_private(path);
        Self::init(conn)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            r#"
            PRAGMA journal_mode=WAL;
            PRAGMA foreign_keys=ON;
            PRAGMA busy_timeout=5000;
            CREATE TABLE IF NOT EXISTS sessions(
                id TEXT PRIMARY KEY, title TEXT NOT NULL, task TEXT NOT NULL, status TEXT NOT NULL,
                model TEXT NOT NULL, parent_id TEXT, plan TEXT NOT NULL DEFAULT '{}',
                state TEXT NOT NULL DEFAULT '{}', summary TEXT,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS messages(
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq INTEGER NOT NULL, body TEXT NOT NULL, created_at TEXT NOT NULL,
                PRIMARY KEY(session_id, seq));
            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(session_id UNINDEXED, text);
            CREATE TABLE IF NOT EXISTS memories(
                id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, content TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '', source TEXT NOT NULL DEFAULT 'user',
                embedding BLOB, created_at TEXT NOT NULL);
            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(content, tags, content='memories', content_rowid='id');
            CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, content, tags) VALUES (new.id, new.content, new.tags); END;
            CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, content, tags) VALUES('delete', old.id, old.content, old.tags); END;
            CREATE TABLE IF NOT EXISTS schedules(
                id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, cron TEXT NOT NULL,
                kind TEXT NOT NULL, payload TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
                last_run TEXT, allowed_tools TEXT NOT NULL DEFAULT '');
            CREATE TABLE IF NOT EXISTS consents(
                key TEXT PRIMARY KEY, granted_at TEXT NOT NULL);
            "#,
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // ---- sessions ---------------------------------------------------------

    pub fn create_session(
        &self,
        title: &str,
        task: &str,
        model: &str,
        parent: Option<&str>,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let t = now();
        self.conn.lock().unwrap().execute(
            "INSERT INTO sessions(id,title,task,status,model,parent_id,plan,state,created_at,updated_at)
             VALUES(?1,?2,?3,'running',?4,?5,?6,?7,?8,?8)",
            params![
                id,
                title,
                task,
                model,
                parent,
                serde_json::to_string(&Plan::default())?,
                serde_json::to_string(&SessionState::default())?,
                t
            ],
        )?;
        Ok(id)
    }

    pub fn update_session(
        &self,
        id: &str,
        status: SessionStatus,
        plan: &Plan,
        state: &SessionState,
        summary: Option<&str>,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE sessions SET status=?2, plan=?3, state=?4, summary=COALESCE(?5, summary), updated_at=?6 WHERE id=?1",
            params![id, status.as_str(), serde_json::to_string(plan)?, serde_json::to_string(state)?, summary, now()],
        )?;
        Ok(())
    }

    pub fn set_session_model(&self, id: &str, model: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE sessions SET model=?2 WHERE id=?1",
            params![id, model],
        )?;
        Ok(())
    }

    fn row_to_session(r: &rusqlite::Row) -> rusqlite::Result<SessionRecord> {
        let plan: String = r.get(6)?;
        let state: String = r.get(7)?;
        Ok(SessionRecord {
            id: r.get(0)?,
            title: r.get(1)?,
            task: r.get(2)?,
            status: SessionStatus::parse(&r.get::<_, String>(3)?),
            model: r.get(4)?,
            parent_id: r.get(5)?,
            plan: serde_json::from_str(&plan).unwrap_or_default(),
            state: serde_json::from_str(&state).unwrap_or_default(),
            summary: r.get(8)?,
            created_at: r.get(9)?,
            updated_at: r.get(10)?,
        })
    }

    pub fn get_session(&self, id: &str) -> Result<Option<SessionRecord>> {
        let c = self.conn.lock().unwrap();
        // Accept unique id prefixes for convenience.
        let pattern = format!("{id}%");
        let mut stmt = c.prepare(
            "SELECT id,title,task,status,model,parent_id,plan,state,summary,created_at,updated_at
             FROM sessions WHERE id LIKE ?1 LIMIT 2",
        )?;
        let rows: Vec<SessionRecord> = stmt
            .query_map([pattern], Self::row_to_session)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(if rows.len() == 1 {
            rows.into_iter().next()
        } else {
            None
        })
    }

    pub fn list_sessions(&self, limit: usize) -> Result<Vec<SessionRecord>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT id,title,task,status,model,parent_id,plan,state,summary,created_at,updated_at
             FROM sessions ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([limit as i64], Self::row_to_session)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn delete_session(&self, id: &str) -> Result<bool> {
        let c = self.conn.lock().unwrap();
        c.execute("DELETE FROM messages_fts WHERE session_id=?1", [id])?;
        Ok(c.execute("DELETE FROM sessions WHERE id=?1", [id])? > 0)
    }

    /// Mark sessions left `running` by a crashed process as interrupted.
    pub fn mark_stale_running(&self) -> Result<usize> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE sessions SET status='interrupted' WHERE status='running'",
            [],
        )?)
    }

    // ---- messages ---------------------------------------------------------

    pub fn append_message(&self, session: &str, msg: &Message) -> Result<()> {
        let c = self.conn.lock().unwrap();
        let seq: i64 = c.query_row(
            "SELECT COALESCE(MAX(seq),-1)+1 FROM messages WHERE session_id=?1",
            [session],
            |r| r.get(0),
        )?;
        c.execute(
            "INSERT INTO messages(session_id,seq,body,created_at) VALUES(?1,?2,?3,?4)",
            params![session, seq, serde_json::to_string(msg)?, now()],
        )?;
        let text = msg.text_content();
        if !text.trim().is_empty() {
            c.execute(
                "INSERT INTO messages_fts(session_id,text) VALUES(?1,?2)",
                params![session, text],
            )?;
        }
        c.execute(
            "UPDATE sessions SET updated_at=?2 WHERE id=?1",
            params![session, now()],
        )?;
        Ok(())
    }

    pub fn messages(&self, session: &str) -> Result<Vec<Message>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare("SELECT body FROM messages WHERE session_id=?1 ORDER BY seq")?;
        let rows = stmt
            .query_map([session], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .filter_map(|b| serde_json::from_str(&b).ok())
            .collect();
        Ok(rows)
    }

    /// Full-text search across conversation history.
    pub fn search_history(&self, query: &str, limit: usize) -> Result<Vec<(String, String)>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT session_id, snippet(messages_fts, 1, '[', ']', '…', 12) FROM messages_fts
             WHERE messages_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![fts_query(query), limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    // ---- memories ---------------------------------------------------------

    pub fn add_memory(
        &self,
        kind: &str,
        content: &str,
        tags: &str,
        source: &str,
        embedding: Option<&[f32]>,
    ) -> Result<i64> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO memories(kind,content,tags,source,embedding,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![kind, content, tags, source, embedding.map(f32_to_bytes), now()],
        )?;
        Ok(c.last_insert_rowid())
    }

    fn row_to_memory(r: &rusqlite::Row) -> rusqlite::Result<MemoryRecord> {
        let emb: Option<Vec<u8>> = r.get(5)?;
        Ok(MemoryRecord {
            id: r.get(0)?,
            kind: r.get(1)?,
            content: r.get(2)?,
            tags: r.get(3)?,
            source: r.get(4)?,
            embedding: emb.map(|b| bytes_to_f32(&b)),
            created_at: r.get(6)?,
        })
    }

    pub fn list_memories(&self) -> Result<Vec<MemoryRecord>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT id,kind,content,tags,source,embedding,created_at FROM memories ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], Self::row_to_memory)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn search_memories_fts(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(MemoryRecord, f64)>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT m.id,m.kind,m.content,m.tags,m.source,m.embedding,m.created_at, bm25(memories_fts)
             FROM memories_fts JOIN memories m ON m.id = memories_fts.rowid
             WHERE memories_fts MATCH ?1 ORDER BY bm25(memories_fts) LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![fts_query(query), limit as i64], |r| {
                Ok((Self::row_to_memory(r)?, r.get::<_, f64>(7)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn delete_memory(&self, id: i64) -> Result<bool> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .execute("DELETE FROM memories WHERE id=?1", [id])?
            > 0)
    }

    pub fn clear_memories(&self) -> Result<usize> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .execute("DELETE FROM memories", [])?)
    }

    // ---- schedules --------------------------------------------------------

    pub fn add_schedule(
        &self,
        name: &str,
        cron: &str,
        kind: &str,
        payload: &str,
        allowed_tools: &str,
    ) -> Result<i64> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO schedules(name,cron,kind,payload,allowed_tools) VALUES(?1,?2,?3,?4,?5)",
            params![name, cron, kind, payload, allowed_tools],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn list_schedules(&self) -> Result<Vec<ScheduleRecord>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare("SELECT id,name,cron,kind,payload,enabled,last_run,allowed_tools FROM schedules ORDER BY id")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ScheduleRecord {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    cron: r.get(2)?,
                    kind: r.get(3)?,
                    payload: r.get(4)?,
                    enabled: r.get::<_, i64>(5)? != 0,
                    last_run: r.get(6)?,
                    allowed_tools: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn set_schedule_last_run(&self, id: i64, when: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE schedules SET last_run=?2 WHERE id=?1",
            params![id, when],
        )?;
        Ok(())
    }

    pub fn delete_schedule(&self, id: i64) -> Result<bool> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .execute("DELETE FROM schedules WHERE id=?1", [id])?
            > 0)
    }

    // ---- consents ---------------------------------------------------------

    pub fn has_consent(&self, key: &str) -> Result<bool> {
        let c = self.conn.lock().unwrap();
        Ok(
            c.query_row("SELECT 1 FROM consents WHERE key=?1", [key], |_| Ok(()))
                .optional()?
                .is_some(),
        )
    }

    pub fn grant_consent(&self, key: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT OR REPLACE INTO consents(key,granted_at) VALUES(?1,?2)",
            params![key, now()],
        )?;
        Ok(())
    }

    pub fn revoke_consent(&self, key: &str) -> Result<bool> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .execute("DELETE FROM consents WHERE key=?1", [key])?
            > 0)
    }
}

/// Turn free text into a safe FTS5 query (OR of quoted terms).
pub fn fts_query(q: &str) -> String {
    let terms: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 2)
        .take(16)
        .map(|t| format!("\"{}\"", t.to_lowercase()))
        .collect();
    if terms.is_empty() {
        "\"\"".into()
    } else {
        terms.join(" OR ")
    }
}

pub fn f32_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn bytes_to_f32(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_messages_and_search() {
        let s = Store::in_memory().unwrap();
        let id = s.create_session("t", "task", "m", None).unwrap();
        s.append_message(&id, &Message::user("procure o relatório financeiro"))
            .unwrap();
        s.append_message(&id, &Message::assistant("ok")).unwrap();
        assert_eq!(s.messages(&id).unwrap().len(), 2);
        assert_eq!(s.search_history("financeiro", 5).unwrap().len(), 1);
        let rec = s.get_session(&id[..8]).unwrap().unwrap();
        assert_eq!(rec.status, SessionStatus::Running);
        assert_eq!(s.mark_stale_running().unwrap(), 1);
        assert_eq!(
            s.get_session(&id).unwrap().unwrap().status,
            SessionStatus::Interrupted
        );
        assert!(s.delete_session(&id).unwrap());
        assert!(s.messages(&id).unwrap().is_empty());
    }

    #[test]
    fn memories_fts_and_delete() {
        let s = Store::in_memory().unwrap();
        let a = s
            .add_memory(
                "preference",
                "O usuário prefere respostas em português",
                "idioma",
                "user",
                Some(&[1.0, 0.0]),
            )
            .unwrap();
        s.add_memory(
            "fact",
            "O servidor de casa usa Debian",
            "infra",
            "user",
            None,
        )
        .unwrap();
        let r = s.search_memories_fts("português idioma", 5).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0.embedding.as_deref(), Some(&[1.0f32, 0.0][..]));
        assert!(s.delete_memory(a).unwrap());
        assert!(s.search_memories_fts("português", 5).unwrap().is_empty());
        assert_eq!(s.clear_memories().unwrap(), 1);
    }

    #[test]
    fn consents() {
        let s = Store::in_memory().unwrap();
        assert!(!s.has_consent("remote:claude").unwrap());
        s.grant_consent("remote:claude").unwrap();
        assert!(s.has_consent("remote:claude").unwrap());
        assert!(s.revoke_consent("remote:claude").unwrap());
    }

    #[test]
    fn fts_query_is_safe() {
        assert_eq!(fts_query("a\" OR 1=1; DROP"), "\"or\" OR \"drop\"");
    }
}
