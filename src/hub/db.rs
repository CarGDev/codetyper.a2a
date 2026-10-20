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

pub const TASK_COLS: &str = "id, context_id, from_agent, to_agent, state, status_message, history, artifacts, created_at, updated_at, requester_seen_at, directory, from_directory, from_session";

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

fn has_column(conn: &Connection, table: &str, col: &str) -> bool {
    let Ok(mut st) = conn.prepare(&format!("PRAGMA table_info({table})")) else { return false };
    st.query_map([], |r| r.get::<_, String>(1))
        .map(|rows| rows.flatten().any(|c| c == col))
        .unwrap_or(false)
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
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS agents (
           name TEXT PRIMARY KEY, card TEXT NOT NULL, updated_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS tasks (
           id TEXT PRIMARY KEY,
           context_id TEXT NOT NULL,
           from_agent TEXT NOT NULL,
           to_agent TEXT NOT NULL,
           state TEXT NOT NULL,
           status_message TEXT,
           history TEXT NOT NULL,
           artifacts TEXT NOT NULL,
           created_at TEXT NOT NULL,
           updated_at TEXT NOT NULL,
           requester_seen_at TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_tasks_to ON tasks(to_agent, state);
         CREATE INDEX IF NOT EXISTS idx_tasks_from ON tasks(from_agent, updated_at);
         CREATE INDEX IF NOT EXISTS idx_tasks_ctx ON tasks(context_id);
         CREATE TABLE IF NOT EXISTS sessions (
           agent TEXT NOT NULL, directory TEXT NOT NULL, session_id TEXT NOT NULL,
           tmux_target TEXT, status TEXT NOT NULL DEFAULT 'live', created_at TEXT NOT NULL,
           PRIMARY KEY (agent, directory, session_id)
         );
         CREATE TABLE IF NOT EXISTS nudges (
           key TEXT PRIMARY KEY, task_id TEXT NOT NULL, context_id TEXT, target TEXT NOT NULL,
           kind TEXT NOT NULL, created_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS hop_allow (
           context_id TEXT PRIMARY KEY, grants INTEGER NOT NULL DEFAULT 0,
           notified_level INTEGER NOT NULL DEFAULT 0, last_continue_msg TEXT
         );
         CREATE TABLE IF NOT EXISTS events (
           id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, task_id TEXT, context_id TEXT,
           kind TEXT NOT NULL, agent TEXT, session TEXT, detail TEXT,
           tokens_in INTEGER, tokens_out INTEGER, cost_usd REAL
         );
         CREATE INDEX IF NOT EXISTS idx_events_task ON events(task_id);",
    )
    .map_err(|e| e.to_string())?;
    for col in ["directory", "from_directory", "from_session"] {
        if !has_column(&conn, "tasks", col) {
            // another process may have added it in the meantime
            let _ = conn.execute(&format!("ALTER TABLE tasks ADD COLUMN {col} TEXT"), []);
        }
    }
    Ok(conn)
}

/// Best-effort trace event: its own errors are ignored so tracing can never break a tick or a tool
/// call. Tokens and cost stay NULL (opencode does not expose usage to us).
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
