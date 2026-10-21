//! SQLite state shared by the hub, the dispatcher and `oc` (one WAL file, many processes).
use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params_from_iter, types::Value as Sql};
use std::path::Path;
use std::time::Duration;

pub const TERMINAL: [&str; 4] = ["completed", "failed", "canceled", "rejected"];
pub const STATES: [&str; 8] = [
    "submitted",
    "working",
    "input-required",
    "auth-required",
    "completed",
    "failed",
    "canceled",
    "rejected",
];

pub const TASK_COLS: &str = "id, context_id, from_agent, to_agent, state, status_message, history, artifacts, created_at, updated_at, requester_seen_at, directory, from_directory, from_session, version, claimed_by, priority";

#[derive(Debug, Clone)]
pub struct TaskRow {
    pub id: String,
    pub context_id: String,
    pub from_agent: String,
    pub to_agent: String,
    pub state: String,
    pub status_message: Option<String>,
    pub history: String,
    pub artifacts: String,
    pub created_at: String,
    pub updated_at: String,
    pub requester_seen_at: Option<String>,
    pub directory: Option<String>,
    pub from_directory: Option<String>,
    /// The opencode session the requester runs in; results are routed back to it.
    pub from_session: Option<String>,
    /// Compare-and-swap counter, bumped by every claim/update.
    pub version: i64,
    /// The agent that claimed the task (set by `claim_task`).
    pub claimed_by: Option<String>,
    pub priority: i64,
    /// Set by the dispatcher: the task named a folder itself (vs. one inferred later).
    pub explicit_dir: bool,
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub agent: String,
    pub directory: String,
    pub session_id: String,
    pub tmux_target: Option<String>,
    pub status: String,
    pub created_at: String,
}

/// ISO-8601 UTC with milliseconds, like JS `toISOString()`.
pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn iso_ago_ms(ms: i64) -> String {
    (Utc::now() - chrono::Duration::milliseconds(ms)).to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn parse_ms(iso: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(iso).map(|d| d.timestamp_millis()).unwrap_or(0)
}

pub fn task_from_row(r: &Row) -> rusqlite::Result<TaskRow> {
    Ok(TaskRow {
        id: r.get(0)?,
        context_id: r.get(1)?,
        from_agent: r.get(2)?,
        to_agent: r.get(3)?,
        state: r.get(4)?,
        status_message: r.get(5)?,
        history: r.get(6)?,
        artifacts: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        requester_seen_at: r.get(10)?,
        directory: r.get(11)?,
        from_directory: r.get(12)?,
        from_session: r.get(13)?,
        version: r.get(14)?,
        claimed_by: r.get(15)?,
        priority: r.get(16)?,
        explicit_dir: false,
    })
}

pub fn session_from_row(r: &Row) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        agent: r.get(0)?,
        directory: r.get(1)?,
        session_id: r.get(2)?,
        tmux_target: r.get(3)?,
        status: r.get(4)?,
        created_at: r.get(5)?,
    })
}

pub const SESSION_COLS: &str = "agent, directory, session_id, tmux_target, status, created_at";

/// `SELECT <cols> FROM tasks <tail>` with positional text params.
pub fn tasks(conn: &Connection, tail: &str, params: &[&str]) -> Result<Vec<TaskRow>, String> {
    let sql = format!("SELECT {TASK_COLS} FROM tasks {tail}");
    let mut st = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = st
        .query_map(params_from_iter(params.iter().map(|p| Sql::Text(p.to_string()))), task_from_row)
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

pub fn task_by_id(conn: &Connection, id: &str) -> Result<Option<TaskRow>, String> {
    Ok(tasks(conn, "WHERE id = ?", &[id])?.into_iter().next())
}

pub fn sessions(conn: &Connection, tail: &str, params: &[&str]) -> Result<Vec<SessionRow>, String> {
    let sql = format!("SELECT {SESSION_COLS} FROM sessions {tail}");
    let mut st = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = st
        .query_map(params_from_iter(params.iter().map(|p| Sql::Text(p.to_string()))), session_from_row)
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

#[allow(dead_code)]
fn has_column(conn: &Connection, table: &str, col: &str) -> bool {
    let Ok(mut st) = conn.prepare(&format!("PRAGMA table_info({table})")) else { return false };
    st.query_map([], |r| r.get::<_, String>(1))
        .map(|rows| rows.flatten().any(|c| c == col))
        .unwrap_or(false)
}


pub const SCHEMA_VERSION: i64 = 2;

/// The schema of `a2a_hub.dbml`. Timestamps are fixed-width ISO-8601 UTC text, JSON columns are
/// checked with json_valid(). `events` carries no foreign keys on purpose: it is append-only
/// and best-effort, so tracing can never fail a call.
const SCHEMA_V2: &str = "
CREATE TABLE agents (
  name TEXT PRIMARY KEY,
  card TEXT NOT NULL CHECK (json_valid(card)),
  status TEXT NOT NULL DEFAULT 'active',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE contexts (
  id TEXT PRIMARY KEY,
  state TEXT NOT NULL DEFAULT 'active',
  root_task_id TEXT,
  metadata TEXT CHECK (metadata IS NULL OR json_valid(metadata)),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  completed_at TEXT
);
CREATE INDEX idx_contexts_state_updated ON contexts(state, updated_at);
CREATE TABLE tasks (
  id TEXT PRIMARY KEY,
  context_id TEXT NOT NULL REFERENCES contexts(id),
  parent_task_id TEXT REFERENCES tasks(id),
  from_agent TEXT NOT NULL REFERENCES agents(name),
  to_agent TEXT NOT NULL REFERENCES agents(name),
  state TEXT NOT NULL DEFAULT 'submitted',
  status_message TEXT,
  history TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(history)),
  artifacts TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(artifacts)),
  version INTEGER NOT NULL DEFAULT 0,
  priority INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  completed_at TEXT,
  requester_seen_at TEXT,
  directory TEXT,
  from_directory TEXT,
  from_session TEXT,
  from_session_key TEXT REFERENCES sessions(key),
  claimed_by TEXT
);
CREATE INDEX idx_tasks_inbox ON tasks(to_agent, state, priority, created_at);
CREATE INDEX idx_tasks_from ON tasks(from_agent, updated_at);
CREATE INDEX idx_tasks_context ON tasks(context_id, created_at);
CREATE INDEX idx_tasks_parent ON tasks(parent_task_id);
CREATE INDEX idx_tasks_state_updated ON tasks(state, updated_at);
CREATE TABLE sessions (
  key TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
  agent TEXT NOT NULL REFERENCES agents(name),
  directory TEXT NOT NULL,
  session_id TEXT NOT NULL,
  tmux_target TEXT,
  status TEXT NOT NULL DEFAULT 'live',
  created_at TEXT NOT NULL,
  last_seen_at TEXT,
  lease_expires_at TEXT,
  ended_at TEXT
);
CREATE UNIQUE INDEX uq_sessions_runtime ON sessions(agent, directory, session_id);
CREATE INDEX idx_sessions_live ON sessions(agent, status, lease_expires_at);
CREATE TABLE nudges (
  key TEXT PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES tasks(id),
  context_id TEXT NOT NULL REFERENCES contexts(id),
  target TEXT NOT NULL,
  kind TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  attempts INTEGER NOT NULL DEFAULT 0,
  next_attempt_at TEXT,
  claimed_by TEXT,
  claim_expires_at TEXT,
  delivered_at TEXT,
  last_error TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX idx_nudges_ready ON nudges(status, next_attempt_at, created_at);
CREATE INDEX idx_nudges_expired_claims ON nudges(status, claim_expires_at);
CREATE INDEX idx_nudges_task ON nudges(task_id, created_at);
CREATE INDEX idx_nudges_context ON nudges(context_id, created_at);
CREATE TABLE hop_allow (
  context_id TEXT PRIMARY KEY REFERENCES contexts(id),
  grants INTEGER NOT NULL DEFAULT 0,
  notified_level INTEGER NOT NULL DEFAULT 0,
  last_continue_msg TEXT,
  updated_at TEXT NOT NULL
);
CREATE TABLE events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts TEXT NOT NULL,
  task_id TEXT,
  context_id TEXT,
  kind TEXT NOT NULL,
  agent TEXT,
  session_key TEXT,
  correlation_id TEXT,
  causation_id TEXT,
  detail TEXT,
  tokens_in INTEGER,
  tokens_out INTEGER,
  cost_usd REAL
);
CREATE INDEX idx_events_task ON events(task_id, id);
CREATE INDEX idx_events_context ON events(context_id, id);
CREATE INDEX idx_events_kind_ts ON events(kind, ts);
CREATE INDEX idx_events_correlation ON events(correlation_id, id);
";

/// v1 (the original schema, `user_version` 0) -> v2: every table is renamed, recreated from
/// SCHEMA_V2 and refilled, with the placeholder agents/contexts the foreign keys need.
const UPGRADE_V1_TO_V2: &str = "
DROP INDEX IF EXISTS idx_tasks_to;
DROP INDEX IF EXISTS idx_tasks_from;
DROP INDEX IF EXISTS idx_tasks_ctx;
DROP INDEX IF EXISTS idx_events_task;
ALTER TABLE agents RENAME TO agents_v1;
ALTER TABLE tasks RENAME TO tasks_v1;
ALTER TABLE sessions RENAME TO sessions_v1;
ALTER TABLE nudges RENAME TO nudges_v1;
ALTER TABLE hop_allow RENAME TO hop_allow_v1;
ALTER TABLE events RENAME TO events_v1;
";

const COPY_V1_TO_V2: &str = "
INSERT INTO agents (name, card, status, created_at, updated_at)
  SELECT name, CASE WHEN json_valid(card) THEN card ELSE '{}' END, 'active', updated_at, updated_at FROM agents_v1;
INSERT OR IGNORE INTO agents (name, card, status, created_at, updated_at)
  SELECT n, '{}', 'active', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now') FROM (
    SELECT from_agent AS n FROM tasks_v1 UNION SELECT to_agent FROM tasks_v1
    UNION SELECT agent FROM sessions_v1 UNION SELECT 'any');
INSERT INTO contexts (id, state, created_at, updated_at, completed_at)
  SELECT c, CASE WHEN SUM(open) > 0 THEN 'active' ELSE 'completed' END, MIN(ca), MAX(ua),
         CASE WHEN SUM(open) > 0 THEN NULL ELSE MAX(ua) END
  FROM (
    SELECT context_id AS c, CASE WHEN state IN ('completed','failed','canceled','rejected') THEN 0 ELSE 1 END AS open,
           created_at AS ca, updated_at AS ua FROM tasks_v1
    UNION ALL SELECT context_id, 0, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')
      FROM hop_allow_v1
    UNION ALL SELECT context_id, 0, created_at, created_at FROM nudges_v1 WHERE context_id IS NOT NULL
  ) GROUP BY c;
INSERT INTO tasks (id, context_id, from_agent, to_agent, state, status_message, history, artifacts, created_at,
                   updated_at, completed_at, requester_seen_at, directory, from_directory, from_session)
  SELECT id, context_id, from_agent, to_agent, state, status_message,
         CASE WHEN json_valid(history) THEN history ELSE '[]' END,
         CASE WHEN json_valid(artifacts) THEN artifacts ELSE '[]' END,
         created_at, updated_at,
         CASE WHEN state IN ('completed','failed','canceled','rejected') THEN updated_at END,
         requester_seen_at, directory, from_directory, from_session FROM tasks_v1;
INSERT INTO sessions (agent, directory, session_id, tmux_target, status, created_at, last_seen_at)
  SELECT agent, directory, session_id, tmux_target, status, created_at, created_at FROM sessions_v1;
INSERT INTO nudges (key, task_id, context_id, target, kind, status, attempts, delivered_at, created_at, updated_at)
  SELECT key, task_id, context_id, target, kind,
         CASE WHEN kind IN ('no-agent','no-session') THEN 'dead' ELSE 'delivered' END, 0,
         CASE WHEN kind IN ('no-agent','no-session') THEN NULL ELSE created_at END, created_at, created_at
  FROM nudges_v1 WHERE context_id IS NOT NULL AND task_id IN (SELECT id FROM tasks);
INSERT INTO hop_allow (context_id, grants, notified_level, last_continue_msg, updated_at)
  SELECT context_id, grants, notified_level, last_continue_msg, strftime('%Y-%m-%dT%H:%M:%fZ','now') FROM hop_allow_v1;
INSERT INTO events (id, ts, task_id, context_id, kind, agent, detail, tokens_in, tokens_out, cost_usd)
  SELECT id, ts, task_id, context_id, kind, agent, detail, tokens_in, tokens_out, cost_usd FROM events_v1;
DROP TABLE agents_v1; DROP TABLE tasks_v1; DROP TABLE sessions_v1; DROP TABLE nudges_v1;
DROP TABLE hop_allow_v1; DROP TABLE events_v1;
";

fn user_version(conn: &Connection) -> Result<i64, String> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(|e| e.to_string())
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    exists(conn, "SELECT 1 FROM sqlite_master WHERE type='table' AND name = ?", &[name])
}

/// Bring the file to SCHEMA_VERSION. Safe with several processes starting at once: the version is
/// re-read inside the write transaction, so only one of them migrates. A v1 file is copied to
/// `<db>.bak-v1` first. Foreign keys are off while tables are rebuilt.
fn migrate(conn: &Connection, path: &Path) -> Result<(), String> {
    if user_version(conn)? >= SCHEMA_VERSION {
        return Ok(());
    }
    conn.execute_batch("PRAGMA foreign_keys = OFF").map_err(|e| e.to_string())?;
    conn.execute_batch("BEGIN IMMEDIATE").map_err(|e| e.to_string())?;
    let run = || -> Result<(), String> {
        let v = user_version(conn)?;
        if v >= SCHEMA_VERSION {
            return Ok(());
        }
        let legacy = table_exists(conn, "tasks");
        if legacy {
            let mut bak = path.as_os_str().to_owned();
            bak.push(".bak-v1");
            let bak = std::path::PathBuf::from(bak);
            if !bak.exists() {
                let _ = std::fs::copy(path, &bak); // best effort; the transaction is the real safety net
            }
            conn.execute_batch(UPGRADE_V1_TO_V2).map_err(|e| e.to_string())?;
        }
        conn.execute_batch(SCHEMA_V2).map_err(|e| e.to_string())?;
        if legacy {
            conn.execute_batch(COPY_V1_TO_V2).map_err(|e| e.to_string())?;
        }
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}")).map_err(|e| e.to_string())
    };
    match run() {
        Ok(()) => conn.execute_batch("COMMIT").map_err(|e| e.to_string()),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(format!("database migration failed: {e}"))
        }
    }
}

/// Open (creating if needed) the shared database. `busy_timeout` is set BEFORE anything that
/// takes a lock, or two processes starting at once fail with "database is locked".
pub fn open(path: &Path) -> Result<Connection, String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let conn = Connection::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    conn.busy_timeout(Duration::from_millis(8000)).map_err(|e| e.to_string())?;
    for attempt in 0.. {
        match conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0)) {
            Ok(_) => break,
            Err(e) if attempt >= 5 => return Err(e.to_string()),
            Err(_) => std::thread::sleep(Duration::from_millis(150)),
        }
    }
    migrate(&conn, path)?;
    conn.execute_batch("PRAGMA foreign_keys = ON").map_err(|e| e.to_string())?;
    Ok(conn)
}

/// Foreign keys need the rows a task points at; create placeholders (`INSERT OR IGNORE`, so a real
/// card or context is never overwritten).
pub fn ensure_agent(conn: &Connection, name: &str) -> rusqlite::Result<()> {
    let t = now();
    conn.execute(
        "INSERT OR IGNORE INTO agents (name, card, status, created_at, updated_at) VALUES (?, '{}', 'active', ?, ?)",
        rusqlite::params![name, t, t],
    )
    .map(|_| ())
}

pub fn ensure_context(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    let t = now();
    conn.execute(
        "INSERT OR IGNORE INTO contexts (id, state, created_at, updated_at) VALUES (?, 'active', ?, ?)",
        rusqlite::params![id, t, t],
    )
    .map(|_| ())
}

/// Best-effort trace event: its own errors are ignored so tracing can never break a tick or a tool
/// call. Tokens and cost are only set by `record_usage`.
pub fn record_event(
    conn: &Connection,
    task_id: Option<&str>,
    context_id: Option<&str>,
    kind: &str,
    agent: Option<&str>,
    detail: Option<&str>,
) {
    let _ = conn.execute(
        "INSERT INTO events (ts, task_id, context_id, kind, agent, detail) VALUES (?,?,?,?,?,?)",
        rusqlite::params![now(), task_id, context_id, kind, agent, detail],
    );
}

/// A usage event: tokens in/out and cost for one task, with the details (cache, reasoning,
/// cumulative session totals) as JSON in `detail`. Best effort like `record_event`.
pub fn record_usage(
    conn: &Connection,
    task_id: &str,
    context_id: &str,
    agent: &str,
    tokens_in: i64,
    tokens_out: i64,
    cost_usd: Option<f64>,
    detail: &str,
) {
    let _ = conn.execute(
        "INSERT INTO events (ts, task_id, context_id, kind, agent, detail, tokens_in, tokens_out, cost_usd) VALUES (?,?,?,'usage',?,?,?,?,?)",
        rusqlite::params![now(), task_id, context_id, agent, detail, tokens_in, tokens_out, cost_usd],
    );
}

pub fn exists(conn: &Connection, sql: &str, params: &[&str]) -> bool {
    conn.query_row(sql, params_from_iter(params.iter().map(|p| Sql::Text(p.to_string()))), |_| Ok(()))
        .optional()
        .ok()
        .flatten()
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_event_stores_a_row_and_never_panics_without_a_table() {
        let dir = std::env::temp_dir().join(format!("a2a-db-ev-{}", std::process::id()));
        let conn = open(&dir.join("t.db")).unwrap();
        record_event(&conn, Some("t1"), Some("c1"), "task_created", Some("a"), Some("b"));
        let (k, d, tin): (String, String, Option<i64>) = conn
            .query_row("SELECT kind, detail, tokens_in FROM events WHERE task_id='t1'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        assert_eq!((k.as_str(), d.as_str(), tin), ("task_created", "b", None));
        conn.execute("DROP TABLE events", []).unwrap();
        record_event(&conn, None, None, "x", None, None); // swallowed
        let _ = std::fs::remove_dir_all(&dir);
    }
}
