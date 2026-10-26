//! SQLite state shared by the hub, the dispatcher and `oc` (one WAL file, many processes).
use chrono::{SecondsFormat, Utc};
use rusqlite::{
    params_from_iter, types::Value as Sql, Connection, OpenFlags, OptionalExtension, Row,
};
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
    /// The runtime that runs the session; `None` (NULL) is opencode.
    pub backend: Option<String>,
}

/// ISO-8601 UTC with milliseconds, like JS `toISOString()`.
pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn iso_ago_ms(ms: i64) -> String {
    (Utc::now() - chrono::Duration::milliseconds(ms)).to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn parse_ms(iso: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
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
        backend: r.get(6)?,
    })
}

pub const SESSION_COLS: &str =
    "agent, directory, session_id, tmux_target, status, created_at, backend";

/// `SELECT <cols> FROM tasks <tail>` with positional text params.
pub fn tasks(conn: &Connection, tail: &str, params: &[&str]) -> Result<Vec<TaskRow>, String> {
    let sql = format!("SELECT {TASK_COLS} FROM tasks {tail}");
    let mut st = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = st
        .query_map(
            params_from_iter(params.iter().map(|p| Sql::Text(p.to_string()))),
            task_from_row,
        )
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
        .query_map(
            params_from_iter(params.iter().map(|p| Sql::Text(p.to_string()))),
            session_from_row,
        )
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

#[allow(dead_code)]
fn has_column(conn: &Connection, table: &str, col: &str) -> bool {
    let Ok(mut st) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return false;
    };
    st.query_map([], |r| r.get::<_, String>(1))
        .map(|rows| rows.flatten().any(|c| c == col))
        .unwrap_or(false)
}

pub const SCHEMA_VERSION: i64 = 6;

/// The current schema (v6) of `a2a_hub.dbml`. Timestamps are fixed-width ISO-8601 UTC text, JSON columns are
/// checked with json_valid(). `events` carries no foreign keys on purpose: it is append-only
/// and best-effort, so tracing can never fail a call.
const SCHEMA: &str = "
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
  ended_at TEXT,
  backend TEXT
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
  cost_usd REAL,
  model TEXT,
  source TEXT,
  ref TEXT,
  tokens_cache_read INTEGER,
  tokens_cache_write INTEGER
);
CREATE INDEX idx_events_task ON events(task_id, id);
CREATE INDEX idx_events_context ON events(context_id, id);
CREATE INDEX idx_events_kind_ts ON events(kind, ts);
CREATE INDEX idx_events_correlation ON events(correlation_id, id);
CREATE INDEX idx_events_model ON events(kind, model);
CREATE INDEX idx_events_source_model ON events(kind, source, model);
CREATE UNIQUE INDEX idx_events_source_ref ON events(source, ref) WHERE ref IS NOT NULL;
CREATE TABLE ingest_offsets (
  source TEXT NOT NULL,
  path TEXT NOT NULL,
  byte_offset INTEGER NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (source, path)
);
";

/// v1 (the original schema, `user_version` 0) -> v2: every table is renamed, recreated from
/// SCHEMA and refilled, with the placeholder agents/contexts the foreign keys need.
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

/// v2 -> v3: `events.model`, filled from the `$.model` of the usage events written before the column existed.
const UPGRADE_V2_TO_V3: &str = "
ALTER TABLE events ADD COLUMN model TEXT;
CREATE INDEX idx_events_model ON events(kind, model);
";

/// Idempotent: only usage rows without a model whose `detail` is JSON with a non-empty text model are touched.
/// v3 -> v4: `events.source` (NULL reads as opencode), `events.ref` (the tool's own id, unique per source) and the
/// per-file read positions of the transcript ingester.
const UPGRADE_V3_TO_V4: &str = "
ALTER TABLE events ADD COLUMN source TEXT;
ALTER TABLE events ADD COLUMN ref TEXT;
CREATE INDEX idx_events_source_model ON events(kind, source, model);
CREATE UNIQUE INDEX idx_events_source_ref ON events(source, ref) WHERE ref IS NOT NULL;
CREATE TABLE ingest_offsets (
  source TEXT NOT NULL,
  path TEXT NOT NULL,
  byte_offset INTEGER NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (source, path)
);
";

/// v4 -> v5: `sessions.backend` names the runtime that runs the session (NULL reads as opencode), so the
/// dispatcher looks an agent's live session up in the right backend.
const UPGRADE_V4_TO_V5: &str = "
ALTER TABLE sessions ADD COLUMN backend TEXT;
";

/// v5 -> v6: prompt-cache token counts in their own columns (NULL where the source has none). Claude Code rows of
/// v4/v5 summed cache into `tokens_in`; they are derived from the transcripts, so they and their read positions are
/// dropped and the next ingest rebuilds them exactly. Rows of other sources are untouched.
const UPGRADE_V5_TO_V6: &str = "
ALTER TABLE events ADD COLUMN tokens_cache_read INTEGER;
ALTER TABLE events ADD COLUMN tokens_cache_write INTEGER;
DELETE FROM events WHERE source = 'claude-code';
DELETE FROM ingest_offsets WHERE source = 'claude-code';
";

const BACKFILL_MODEL: &str = "
UPDATE events SET model = json_extract(detail, '$.model')
WHERE kind = 'usage' AND model IS NULL AND json_valid(detail)
  AND json_type(detail, '$.model') = 'text' AND json_extract(detail, '$.model') != '';
";

fn user_version(conn: &Connection) -> Result<i64, String> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    exists(
        conn,
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name = ?",
        &[name],
    )
}

/// `<db><suffix>` next to the file, once: a later migration never overwrites an older backup. Best
/// effort; the transaction is the real safety net.
fn backup(path: &Path, suffix: &str) {
    let mut bak = path.as_os_str().to_owned();
    bak.push(suffix);
    let bak = std::path::PathBuf::from(bak);
    if bak.exists() {
        return;
    }
    let _ = std::fs::copy(path, &bak);
}

/// The stepwise upgrades of a file that is already v2 or newer; each step first copies the file to `<db>.bak-v<from>`.
fn upgrade_from(conn: &Connection, path: &Path, from: i64) -> Result<(), String> {
    let run = |sql: &str| conn.execute_batch(sql).map_err(|e| e.to_string());
    if from == 2 {
        backup(path, ".bak-v2");
        run(UPGRADE_V2_TO_V3)?;
        run(BACKFILL_MODEL)?;
    }
    if from == 3 {
        backup(path, ".bak-v3");
    }
    if from <= 3 {
        run(UPGRADE_V3_TO_V4)?;
    }
    if from == 4 {
        backup(path, ".bak-v4");
    }
    if from <= 4 {
        run(UPGRADE_V4_TO_V5)?;
    }
    if from == 5 {
        backup(path, ".bak-v5");
    }
    run(UPGRADE_V5_TO_V6)?;
    run(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
}

/// Bring the file to SCHEMA_VERSION, one step per version. Safe with several processes starting at
/// once: the version is re-read inside the write transaction, so only one of them migrates. A v1
/// file is copied to `<db>.bak-v1` first, a v2 file to `<db>.bak-v2`, a v3 file to `<db>.bak-v3`, a v4 file to `<db>.bak-v4`, a v5 file to `<db>.bak-v5`. Foreign keys are off while
/// tables are rebuilt.
fn migrate(conn: &Connection, path: &Path) -> Result<(), String> {
    if user_version(conn)? >= SCHEMA_VERSION {
        return Ok(());
    }
    conn.execute_batch("PRAGMA foreign_keys = OFF")
        .map_err(|e| e.to_string())?;
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| e.to_string())?;
    let run = || -> Result<(), String> {
        let v = user_version(conn)?;
        if v >= SCHEMA_VERSION {
            return Ok(());
        }
        if v >= 2 {
            return upgrade_from(conn, path, v);
        }
        let legacy = table_exists(conn, "tasks");
        if legacy {
            backup(path, ".bak-v1");
            conn.execute_batch(UPGRADE_V1_TO_V2)
                .map_err(|e| e.to_string())?;
        }
        conn.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        if legacy {
            conn.execute_batch(COPY_V1_TO_V2)
                .map_err(|e| e.to_string())?;
            conn.execute_batch(BACKFILL_MODEL)
                .map_err(|e| e.to_string())?;
        }
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
            .map_err(|e| e.to_string())
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
    open_inner(path, true)
}

/// Open after a caller has already acquired the migration lock and handled legacy state.
pub fn open_after_migration(path: &Path) -> Result<Connection, String> {
    open_inner(path, false)
}

fn open_inner(path: &Path, migrate_legacy: bool) -> Result<Connection, String> {
    if migrate_legacy
        && std::env::var_os("A2A_DB").is_none()
        && path == crate::paths::runtime_home().join("hub.db")
    {
        crate::paths::migrate_legacy(&crate::paths::runtime_home(), &crate::paths::legacy_home())?;
    }
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        if path.starts_with(crate::paths::runtime_home()) {
            crate::paths::ensure_private_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        } else {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
    }
    let conn = Connection::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    crate::paths::ensure_private_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
    conn.busy_timeout(Duration::from_millis(8000))
        .map_err(|e| e.to_string())?;
    for attempt in 0.. {
        match conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0)) {
            Ok(_) => break,
            Err(e) if attempt >= 5 => return Err(e.to_string()),
            Err(_) => std::thread::sleep(Duration::from_millis(150)),
        }
    }
    migrate(&conn, path)?;
    conn.execute_batch("PRAGMA foreign_keys = ON")
        .map_err(|e| e.to_string())?;
    Ok(conn)
}

pub fn open_read_compat(path: &Path) -> Result<(Connection, Option<String>), String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    conn.busy_timeout(Duration::from_millis(8000))
        .map_err(|e| e.to_string())?;
    Ok((conn, None))
}

/// Record an interactive session (one that lives in a tmux pane): older live rows of the same agent in
/// the same folder and runtime are retired so the newest session is the one results are sent to.
pub fn register_interactive(
    conn: &Connection,
    agent: &str,
    directory: &str,
    session: &str,
    pane: &str,
    backend: &str,
) -> rusqlite::Result<()> {
    ensure_agent(conn, agent)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE sessions SET status='stale' WHERE agent=? AND directory=? AND backend=? AND status='live'",
        rusqlite::params![agent, directory, backend],
    )?;
    let t = now();
    tx.execute(
        "INSERT INTO sessions (agent, directory, session_id, tmux_target, backend, created_at, last_seen_at) VALUES (?,?,?,?,?,?,?)",
        rusqlite::params![agent, directory, session, pane, backend, t, t],
    )?;
    tx.commit()
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

/// A usage event: tokens in/out, cost and model for one task, with the details (cache, reasoning,
/// cumulative session totals) as JSON in `detail`. Best effort like `record_event`.
#[allow(clippy::too_many_arguments)]
pub fn record_usage(
    conn: &Connection,
    task_id: &str,
    context_id: &str,
    agent: &str,
    tokens_in: i64,
    tokens_out: i64,
    cost_usd: Option<f64>,
    model: Option<&str>,
    detail: &str,
) {
    let _ = conn.execute(
        "INSERT INTO events (ts, task_id, context_id, kind, agent, detail, tokens_in, tokens_out, cost_usd, model) VALUES (?,?,?,'usage',?,?,?,?,?,?)",
        rusqlite::params![now(), task_id, context_id, agent, detail, tokens_in, tokens_out, cost_usd, model],
    );
}

/// One usage event read from another tool's own log (not written by the dispatcher).
pub struct SourcedUsage<'a> {
    pub source: &'a str,
    /// The tool's own id for the turn; (source, reference) is unique, so a rerun inserts nothing.
    pub reference: &'a str,
    pub ts: &'a str,
    pub session: &'a str,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cache_read: Option<i64>,
    pub cache_write: Option<i64>,
    pub cost_usd: Option<f64>,
    pub model: &'a str,
    pub detail: &'a str,
}

/// Insert a usage event of another source; false when that (source, reference) is already stored.
pub fn record_usage_from(conn: &Connection, u: &SourcedUsage) -> bool {
    let agent: Option<String> = conn
        .query_row(
            "SELECT CASE WHEN COUNT(DISTINCT NULLIF(agent, '')) = 1 THEN MAX(NULLIF(agent, '')) END
             FROM sessions
             WHERE session_id = ? AND backend = 'claude'",
            [u.session],
            |row| row.get(0),
        )
        .ok()
        .flatten();
    conn.execute(
        "INSERT OR IGNORE INTO events (ts, task_id, kind, agent, detail, tokens_in, tokens_out, tokens_cache_read, tokens_cache_write, cost_usd, model, source, ref) VALUES (?,?,'usage',?,?,?,?,?,?,?,?,?,?)",
        rusqlite::params![u.ts, u.session, agent, u.detail, u.tokens_in, u.tokens_out, u.cache_read, u.cache_write, u.cost_usd, u.model, u.source, u.reference],
    )
    .is_ok_and(|n| n > 0)
}

pub fn exists(conn: &Connection, sql: &str, params: &[&str]) -> bool {
    conn.query_row(
        sql,
        params_from_iter(params.iter().map(|p| Sql::Text(p.to_string()))),
        |_| Ok(()),
    )
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
        record_event(
            &conn,
            Some("t1"),
            Some("c1"),
            "task_created",
            Some("a"),
            Some("b"),
        );
        let (k, d, tin): (String, String, Option<i64>) = conn
            .query_row(
                "SELECT kind, detail, tokens_in FROM events WHERE task_id='t1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((k.as_str(), d.as_str(), tin), ("task_created", "b", None));
        conn.execute("DROP TABLE events", []).unwrap();
        record_event(&conn, None, None, "x", None, None); // swallowed
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn db_overrides_do_not_restrict_external_parents_or_reject_relative_paths() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("a2a-db-perms-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let external = root.join("external.db");
        open(&external).unwrap();
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let relative = format!("a2a-relative-{}.db", std::process::id());
        drop(open(Path::new(&relative)).unwrap());
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{relative}{suffix}"));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_compat_opener_rejects_writes() {
        let dir = std::env::temp_dir().join(format!("a2a-db-read-only-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("hub.db");
        drop(open(&path).unwrap());
        let (conn, warning) = open_read_compat(&path).unwrap();
        assert!(warning.is_none());
        assert!(conn
            .execute("CREATE TABLE should_not_write (id INTEGER)", [])
            .is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn temp_db(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("a2a-db-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("t.db");
        (dir, path)
    }

    /// What a binary of schema 2 writes: no `model` column in the INSERT.
    fn insert_old_style(conn: &Connection, kind: &str, detail: Option<&str>) {
        conn.execute(
            "INSERT INTO events (ts, task_id, context_id, kind, agent, detail, tokens_in, tokens_out, cost_usd) VALUES (?,?,?,?,?,?,?,?,?)",
            rusqlite::params![now(), "t1", "c1", kind, "build", detail, 10, 2, 0.5],
        )
        .unwrap();
    }

    fn model_of(conn: &Connection, detail: &str) -> Option<String> {
        conn.query_row("SELECT model FROM events WHERE detail = ?", [detail], |r| {
            r.get(0)
        })
        .unwrap()
    }

    fn event_columns(conn: &Connection) -> Vec<String> {
        let mut st = conn.prepare("PRAGMA table_info(events)").unwrap();
        st.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .flatten()
            .collect()
    }

    /// Turn a freshly created database into the schema-4 shape: no cache columns, no `sessions.backend`, version 4.
    fn downgrade_to_v4(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("ALTER TABLE events DROP COLUMN tokens_cache_read; ALTER TABLE events DROP COLUMN tokens_cache_write; ALTER TABLE sessions DROP COLUMN backend; PRAGMA user_version = 4;").unwrap();
    }

    /// Turn a freshly created database into the schema-3 shape: no `source`, `ref`, ingest table, version 3.
    fn downgrade_to_v3(path: &Path) {
        downgrade_to_v4(path);
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "DROP INDEX idx_events_source_model; DROP INDEX idx_events_source_ref; DROP TABLE ingest_offsets;
             ALTER TABLE events DROP COLUMN source; ALTER TABLE events DROP COLUMN ref; PRAGMA user_version = 3;",
        )
        .unwrap();
    }

    /// Turn a freshly created database into the schema-2 shape: no `model`, no index, version 2.
    fn downgrade_to_v2(path: &Path) {
        downgrade_to_v3(path);
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("DROP INDEX idx_events_model; ALTER TABLE events DROP COLUMN model; PRAGMA user_version = 2;").unwrap();
    }

    #[test]
    fn a_fresh_database_has_the_model_column_index_and_version() {
        let (dir, path) = temp_db("fresh");
        let conn = open(&path).unwrap();
        let version = user_version(&conn).unwrap();
        let columns = event_columns(&conn);
        let index = exists(
            &conn,
            "SELECT 1 FROM sqlite_master WHERE type='index' AND name = ?",
            &["idx_events_model"],
        );
        insert_old_style(&conn, "usage", Some("old")); // an older binary's INSERT still works
        let old_model = model_of(&conn, "old");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(version, 6);
        assert!(columns.contains(&"model".to_string()), "{columns:?}");
        assert!(index);
        assert_eq!(old_model, None);
    }

    #[test]
    fn record_usage_stores_the_model_in_its_column() {
        let (dir, path) = temp_db("usage");
        let conn = open(&path).unwrap();
        record_usage(
            &conn,
            "t1",
            "c1",
            "build",
            10,
            2,
            Some(0.5),
            Some("openai/gpt-5.6-luna"),
            "{}",
        );
        record_usage(&conn, "t2", "c1", "build", 10, 2, None, None, "{}");
        let models: Vec<Option<String>> = conn
            .prepare("SELECT model FROM events WHERE kind = 'usage' ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(models, vec![Some("openai/gpt-5.6-luna".to_string()), None]);
    }

    #[test]
    fn a_v2_database_gets_the_column_backfilled_from_detail_and_a_backup() {
        let (dir, path) = temp_db("v2");
        drop(open(&path).unwrap());
        downgrade_to_v2(&path);
        let raw = Connection::open(&path).unwrap();
        insert_old_style(&raw, "usage", Some(r#"{"model":"a/known"}"#));
        insert_old_style(&raw, "usage", Some(r#"{"cost":1}"#));
        insert_old_style(&raw, "usage", Some("not json"));
        insert_old_style(&raw, "usage", Some(r#"{"model":""}"#));
        insert_old_style(&raw, "usage", Some(r#"{"model":7}"#));
        insert_old_style(&raw, "nudge_delivered", Some(r#"{"model":"a/not-usage"}"#));
        drop(raw);

        let conn = open(&path).unwrap();
        let got = [
            model_of(&conn, r#"{"model":"a/known"}"#),
            model_of(&conn, r#"{"cost":1}"#),
            model_of(&conn, "not json"),
            model_of(&conn, r#"{"model":""}"#),
            model_of(&conn, r#"{"model":7}"#),
            model_of(&conn, r#"{"model":"a/not-usage"}"#),
        ];
        let index = exists(
            &conn,
            "SELECT 1 FROM sqlite_master WHERE type='index' AND name = ?",
            &["idx_events_model"],
        );
        let (version, rows) = (
            user_version(&conn).unwrap(),
            conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
                .unwrap(),
        );
        let backup = path.with_extension("db.bak-v2").exists();
        drop(conn);
        let again = open(&path).unwrap(); // idempotent
        let (version2, rows2) = (
            user_version(&again).unwrap(),
            again
                .query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
                .unwrap(),
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            got,
            [Some("a/known".to_string()), None, None, None, None, None]
        );
        assert!(index && backup);
        assert_eq!((version, rows, version2, rows2), (6, 6, 6, 6));
    }

    #[test]
    fn a_v3_database_gets_source_and_ref_keeps_its_rows_and_is_backed_up_once() {
        let (dir, path) = temp_db("v3");
        drop(open(&path).unwrap());
        downgrade_to_v3(&path);
        let raw = Connection::open(&path).unwrap();
        insert_old_style(&raw, "usage", Some("kept"));
        drop(raw);
        let conn = open(&path).unwrap();
        let (version, columns) = (user_version(&conn).unwrap(), event_columns(&conn));
        let source: Option<String> = conn
            .query_row("SELECT source FROM events WHERE detail = 'kept'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let indexes = ["idx_events_source_model", "idx_events_source_ref"].map(|i| {
            exists(
                &conn,
                "SELECT 1 FROM sqlite_master WHERE type='index' AND name = ?",
                &[i],
            )
        });
        let offsets = table_exists(&conn, "ingest_offsets");
        let backups = (
            path.with_extension("db.bak-v3").exists(),
            path.with_extension("db.bak-v2").exists(),
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(version, 6);
        assert!(
            columns.contains(&"source".to_string()) && columns.contains(&"ref".to_string()),
            "{columns:?}"
        );
        assert_eq!(
            source, None,
            "NULL source is read as opencode, not backfilled"
        );
        assert_eq!(indexes, [true, true]);
        assert!(offsets);
        assert_eq!(backups, (true, false));
    }

    #[test]
    fn a_v4_database_gets_the_session_backend_column_keeps_its_rows_and_is_backed_up_once() {
        let (dir, path) = temp_db("v4");
        drop(open(&path).unwrap());
        downgrade_to_v4(&path);
        let raw = Connection::open(&path).unwrap();
        raw.execute_batch(
            "INSERT INTO agents (name, card, created_at, updated_at) VALUES ('mentor','{}','t','t');
             INSERT INTO sessions (agent, directory, session_id, created_at) VALUES ('mentor','/p','ses_old','t');",
        )
        .unwrap();
        drop(raw);
        let conn = open(&path).unwrap();
        let version = user_version(&conn).unwrap();
        let backend: Option<String> = conn
            .query_row(
                "SELECT backend FROM sessions WHERE session_id = 'ses_old'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let backups = (
            path.with_extension("db.bak-v4").exists(),
            path.with_extension("db.bak-v3").exists(),
        );
        drop(conn);
        let again = open(&path).unwrap(); // second open: no-op
        let version2 = user_version(&again).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!((version, version2), (6, 6));
        assert_eq!(
            backend, None,
            "NULL backend is read as opencode, not backfilled"
        );
        assert_eq!(backups, (true, false));
    }

    #[test]
    fn a_v2_database_goes_through_v3_to_v4_in_one_open() {
        let (dir, path) = temp_db("v2to4");
        drop(open(&path).unwrap());
        downgrade_to_v2(&path);
        let conn = open(&path).unwrap();
        let (version, columns) = (user_version(&conn).unwrap(), event_columns(&conn));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(version, 6);
        assert!(
            ["model", "source", "ref"]
                .iter()
                .all(|c| columns.contains(&c.to_string())),
            "{columns:?}"
        );
    }

    fn sourced<'a>(reference: &'a str, tokens_in: i64) -> SourcedUsage<'a> {
        SourcedUsage {
            source: "claude-code",
            reference,
            ts: "2026-10-24T00:00:00.000Z",
            session: "s1",
            tokens_in,
            tokens_out: 1,
            cache_read: None,
            cache_write: None,
            cost_usd: None,
            model: "claude-sonnet-5-5",
            detail: "{}",
        }
    }

    #[test]
    fn record_usage_from_is_unique_per_source_and_reference() {
        let (dir, path) = temp_db("sourced");
        let conn = open(&path).unwrap();
        let (first, again, other) = (
            record_usage_from(&conn, &sourced("m1", 5)),
            record_usage_from(&conn, &sourced("m1", 9)),
            record_usage_from(
                &conn,
                &SourcedUsage {
                    source: "other",
                    ..sourced("m1", 5)
                },
            ),
        );
        record_usage(&conn, "t", "c", "build", 1, 1, None, None, "{}"); // NULL ref rows never collide
        record_usage(&conn, "t", "c", "build", 1, 1, None, None, "{}");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!((first, again, other, rows), (true, false, true, 4));
    }

    #[test]
    fn sourced_usage_attributes_only_one_trusted_claude_session() {
        let (dir, path) = temp_db("sourced-agent");
        let conn = open(&path).unwrap();
        conn.execute_batch(
            "INSERT INTO agents (name, card, created_at, updated_at) VALUES
                ('alice', '{}', 'now', 'now'), ('bob', '{}', 'now', 'now'), ('', '{}', 'now', 'now');
             INSERT INTO sessions (agent, directory, session_id, created_at, backend)
                VALUES ('alice', '/tmp/project', 'unique', 'now', 'claude'),
                       ('alice', '/tmp/project', 'duplicate-same', 'now', 'claude'),
                       ('alice', '/tmp/project-alt', 'duplicate-same', 'now', 'claude'),
                       ('', '/tmp/project', 'duplicate-same', 'now', 'claude'),
                       ('alice', '/tmp/project', 'ambiguous', 'now', 'claude'),
                       ('bob', '/tmp/project', 'ambiguous', 'now', 'claude'),
                       ('bob', '/tmp/project', 'opencode', 'now', 'opencode'),
                       ('', '/tmp/project', 'empty-agent', 'now', 'claude');",
        )
        .unwrap();
        for (reference, session) in [
            ("unique", "unique"),
            ("duplicate-same", "duplicate-same"),
            ("missing", "missing"),
            ("ambiguous", "ambiguous"),
            ("non-claude", "opencode"),
            ("empty-agent", "empty-agent"),
        ] {
            let inserted = record_usage_from(
                &conn,
                &SourcedUsage {
                    session,
                    reference,
                    ..sourced(reference, 1)
                },
            );
            assert!(inserted);
        }
        let mut rows = conn
            .prepare("SELECT ref, agent FROM events WHERE source = 'claude-code' ORDER BY ref")
            .unwrap();
        let rows: Vec<(String, Option<String>)> = rows
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(rows.len(), 6);
        assert_eq!(
            rows.iter().find(|(reference, _)| reference == "unique"),
            Some(&("unique".into(), Some("alice".into())))
        );
        assert_eq!(
            rows.iter()
                .find(|(reference, _)| reference == "duplicate-same"),
            Some(&("duplicate-same".into(), Some("alice".into())))
        );
        assert!(rows
            .iter()
            .filter(|(reference, _)| reference != "unique" && reference != "duplicate-same")
            .all(|(_, agent)| agent.is_none()));
    }

    #[test]
    fn several_processes_opening_a_v2_file_at_once_migrate_it_once() {
        let (dir, path) = temp_db("race");
        drop(open(&path).unwrap());
        downgrade_to_v2(&path);
        let raw = Connection::open(&path).unwrap();
        insert_old_style(&raw, "usage", Some(r#"{"model":"a/known"}"#));
        drop(raw);
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let p = path.clone();
                std::thread::spawn(move || open(&p).map(|c| user_version(&c).unwrap()))
            })
            .collect();
        let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        let conn = open(&path).unwrap();
        let model = model_of(&conn, r#"{"model":"a/known"}"#);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(results.iter().all(|r| matches!(r, Ok(6))), "{results:?}");
        assert_eq!(model.as_deref(), Some("a/known"));
    }

    #[test]
    fn a_v1_database_is_upgraded_through_v2_to_v3_with_its_model_backfilled() {
        let (dir, path) = temp_db("v1");
        std::fs::create_dir_all(&dir).unwrap();
        let raw = Connection::open(&path).unwrap();
        raw.execute_batch(
            "CREATE TABLE agents (name TEXT PRIMARY KEY, card TEXT, updated_at TEXT);
             CREATE TABLE tasks (id TEXT PRIMARY KEY, context_id TEXT, from_agent TEXT, to_agent TEXT, state TEXT, status_message TEXT,
               history TEXT, artifacts TEXT, created_at TEXT, updated_at TEXT, requester_seen_at TEXT, directory TEXT, from_directory TEXT, from_session TEXT);
             CREATE TABLE sessions (agent TEXT, directory TEXT, session_id TEXT, tmux_target TEXT, status TEXT, created_at TEXT);
             CREATE TABLE nudges (key TEXT, task_id TEXT, context_id TEXT, target TEXT, kind TEXT, created_at TEXT);
             CREATE TABLE hop_allow (context_id TEXT, grants INTEGER, notified_level INTEGER, last_continue_msg TEXT);
             CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, task_id TEXT, context_id TEXT, kind TEXT, agent TEXT, detail TEXT,
               tokens_in INTEGER, tokens_out INTEGER, cost_usd REAL);
             INSERT INTO agents VALUES ('build', '{}', '2026-01-01T00:00:00.000Z');
             INSERT INTO tasks VALUES ('t1', 'c1', 'build', 'build', 'completed', NULL, '[]', '[]', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:01.000Z', NULL, NULL, NULL, NULL);
             INSERT INTO events (ts, task_id, context_id, kind, agent, detail, tokens_in, tokens_out, cost_usd)
               VALUES ('2026-01-01T00:00:01.000Z', 't1', 'c1', 'usage', 'build', '{\"model\":\"a/legacy\"}', 5, 1, NULL);",
        )
        .unwrap();
        drop(raw);
        let conn = open(&path).unwrap();
        let (version, model, tasks) = (
            user_version(&conn).unwrap(),
            model_of(&conn, r#"{"model":"a/legacy"}"#),
            conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get::<_, i64>(0))
                .unwrap(),
        );
        let (bak1, bak2) = (
            path.with_extension("db.bak-v1").exists(),
            path.with_extension("db.bak-v2").exists(),
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!((version, tasks, bak1, bak2), (6, 1, true, false));
        assert_eq!(model.as_deref(), Some("a/legacy"));
    }
}
