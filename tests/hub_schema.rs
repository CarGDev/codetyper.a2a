//! The hub database migrates from the original (v1) schema to a2a_hub.dbml (v5) without losing rows.
use std::path::PathBuf;
use std::process::Command;

const V1: &str = "
CREATE TABLE agents (name TEXT PRIMARY KEY, card TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE tasks (id TEXT PRIMARY KEY, context_id TEXT NOT NULL, from_agent TEXT NOT NULL, to_agent TEXT NOT NULL,
  state TEXT NOT NULL, status_message TEXT, history TEXT NOT NULL, artifacts TEXT NOT NULL, created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL, requester_seen_at TEXT, directory TEXT, from_directory TEXT, from_session TEXT);
CREATE INDEX idx_tasks_to ON tasks(to_agent, state);
CREATE INDEX idx_tasks_from ON tasks(from_agent, updated_at);
CREATE INDEX idx_tasks_ctx ON tasks(context_id);
CREATE TABLE sessions (agent TEXT NOT NULL, directory TEXT NOT NULL, session_id TEXT NOT NULL, tmux_target TEXT,
  status TEXT NOT NULL DEFAULT 'live', created_at TEXT NOT NULL, PRIMARY KEY (agent, directory, session_id));
CREATE TABLE nudges (key TEXT PRIMARY KEY, task_id TEXT NOT NULL, context_id TEXT, target TEXT NOT NULL,
  kind TEXT NOT NULL, created_at TEXT NOT NULL);
CREATE TABLE hop_allow (context_id TEXT PRIMARY KEY, grants INTEGER NOT NULL DEFAULT 0,
  notified_level INTEGER NOT NULL DEFAULT 0, last_continue_msg TEXT);
CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, task_id TEXT, context_id TEXT,
  kind TEXT NOT NULL, agent TEXT, session TEXT, detail TEXT, tokens_in INTEGER, tokens_out INTEGER, cost_usd REAL);
CREATE INDEX idx_events_task ON events(task_id);
INSERT INTO agents VALUES ('mentor', '{\"name\":\"mentor\"}', 't1');
INSERT INTO tasks VALUES ('task-open-1','c1','orchestrator','mentor','working',NULL,'[]','[]','t1','t2',NULL,'/p',NULL,'ses1');
INSERT INTO tasks VALUES ('task-done-1','c1','orchestrator','mentor','completed',NULL,'not json','[]','t1','t3',NULL,'/p',NULL,NULL);
INSERT INTO sessions VALUES ('mentor','/p','ses_m','tmux','live','t1');
INSERT INTO nudges VALUES ('k1','task-open-1','c1','mentor','assignee','t1');
INSERT INTO nudges VALUES ('k2','task-open-1','c1','mentor','no-session','t1');
INSERT INTO nudges VALUES ('k3','gone','c1','mentor','assignee','t1');
INSERT INTO hop_allow VALUES ('c1', 2, 1, NULL);
INSERT INTO events (ts, task_id, context_id, kind, agent) VALUES ('t1','task-open-1','c1','task_created','orchestrator');
";

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("a2a-schema-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Any `oc` call opens (and so migrates) the database.
fn open_via_oc(db: &std::path::Path) {
    let st = Command::new(env!("CARGO_BIN_EXE_codetypera2a"))
        .args(["oc", "status"])
        .env("A2A_DB", db)
        .env("A2A_DISPATCHER_PID", db.with_extension("pid"))
        .env("OPENCODE_BIN", "/usr/bin/false")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .unwrap();
    assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
}

fn one(c: &rusqlite::Connection, sql: &str) -> String {
    c.query_row(sql, [], |r| r.get::<_, rusqlite::types::Value>(0))
        .map(|v| match v {
            rusqlite::types::Value::Text(t) => t,
            rusqlite::types::Value::Integer(i) => i.to_string(),
            rusqlite::types::Value::Null => "NULL".into(),
            other => format!("{other:?}"),
        })
        .unwrap()
}

#[test]
fn fresh_database_is_v5_with_every_table() {
    let d = tmp("fresh");
    let db = d.join("hub.db");
    open_via_oc(&db);
    let c = rusqlite::Connection::open(&db).unwrap();
    assert_eq!(one(&c, "PRAGMA user_version"), "5");
    for t in ["agents", "contexts", "tasks", "sessions", "nudges", "hop_allow", "events"] {
        assert_eq!(one(&c, &format!("SELECT COUNT(*) FROM sqlite_master WHERE name='{t}'")), "1", "{t}");
    }
    for col in ["version", "claimed_by", "parent_task_id", "priority"] {
        assert_eq!(one(&c, &format!("SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name='{col}'")), "1", "{col}");
    }
    assert_eq!(one(&c, "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='backend'"), "1");
    assert_eq!(one(&c, "SELECT COUNT(*) FROM pragma_table_info('events') WHERE name='model'"), "1");
    assert_eq!(one(&c, "SELECT COUNT(*) FROM sqlite_master WHERE name='idx_events_model'"), "1");
    for col in ["source", "ref"] {
        assert_eq!(one(&c, &format!("SELECT COUNT(*) FROM pragma_table_info('events') WHERE name='{col}'")), "1", "{col}");
    }
    for name in ["idx_events_source_model", "idx_events_source_ref", "ingest_offsets"] {
        assert_eq!(one(&c, &format!("SELECT COUNT(*) FROM sqlite_master WHERE name='{name}'")), "1", "{name}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn v1_database_is_migrated_and_keeps_its_rows() {
    let d = tmp("v1");
    let db = d.join("hub.db");
    rusqlite::Connection::open(&db).unwrap().execute_batch(V1).unwrap();
    open_via_oc(&db);
    open_via_oc(&db); // idempotent
    let c = rusqlite::Connection::open(&db).unwrap();
    assert_eq!(one(&c, "PRAGMA user_version"), "5");
    assert!(d.join("hub.db.bak-v1").exists(), "a backup of the v1 file is kept");
    assert_eq!(one(&c, "SELECT COUNT(*) FROM tasks"), "2");
    assert_eq!(one(&c, "SELECT history FROM tasks WHERE id='task-done-1'"), "[]", "invalid JSON is repaired");
    assert_eq!(one(&c, "SELECT completed_at FROM tasks WHERE id='task-done-1'"), "t3");
    assert_eq!(one(&c, "SELECT state FROM contexts WHERE id='c1'"), "active");
    assert_eq!(one(&c, "SELECT COUNT(*) FROM agents WHERE name IN ('orchestrator','any','mentor')"), "3");
    assert_eq!(one(&c, "SELECT length(key) FROM sessions"), "32");
    assert_eq!(one(&c, "SELECT status FROM nudges WHERE key='k1'"), "delivered");
    assert_eq!(one(&c, "SELECT status FROM nudges WHERE key='k2'"), "dead");
    assert_eq!(one(&c, "SELECT COUNT(*) FROM nudges WHERE key='k3'"), "0", "orphan nudge dropped");
    assert_eq!(one(&c, "SELECT grants FROM hop_allow WHERE context_id='c1'"), "2");
    assert_eq!(one(&c, "SELECT COUNT(*) FROM events"), "1");
    assert_eq!(one(&c, "SELECT model FROM events"), "NULL", "no usage event, no model");
    assert_eq!(one(&c, "SELECT COUNT(*) FROM pragma_foreign_key_check"), "0");
    let _ = std::fs::remove_dir_all(&d);
}
