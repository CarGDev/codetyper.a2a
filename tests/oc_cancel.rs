//! `oc cancel` stops open tasks (and the turns working on them) from the terminal.
use std::path::{Path, PathBuf};
use std::process::Command;

fn sandbox(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("a2a-cancel-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // a fake opencode: logs every call; session ses_m1 is running a turn
    let stub = dir.join("opencode");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\necho \"$@\" >> {log}\ncase \"$2 $3\" in\n  \"get /api/session/active\") echo '{{\"data\":{{\"ses_m1\":{{}}}}}}';;\nesac\n",
            log = dir.join("calls.log").display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn oc(dir: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_codetypera2a"))
        .arg("oc")
        .args(args)
        .env("A2A_DB", dir.join("hub.db"))
        .env("A2A_DISPATCHER_PID", dir.join("d.pid"))
        .env("OPENCODE_BIN", dir.join("opencode"))
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.success())
}

fn seed(dir: &Path) -> rusqlite::Connection {
    let _ = oc(dir, &["status"]); // creates the schema
    let c = rusqlite::Connection::open(dir.join("hub.db")).unwrap();
    c.execute_batch(
        "INSERT INTO agents (name, card, created_at, updated_at) VALUES ('orchestrator','{}','t','t'), ('mentor','{}','t','t'), ('any','{}','t','t');
         INSERT INTO contexts (id, created_at, updated_at) VALUES ('c1','t','t'), ('c2','t','t');
         INSERT INTO sessions (agent, directory, session_id, created_at) VALUES ('mentor','/tmp/projx','ses_m1','t');",
    )
    .unwrap();
    for (id, ctx, state) in [
        ("aaaaaaaa-1", "c1", "submitted"),
        ("bbbbbbbb-2", "c1", "working"),
        ("cccccccc-3", "c2", "input-required"),
        ("dddddddd-4", "c2", "auth-required"),
        ("eeeeeeee-5", "c1", "completed"),
    ] {
        c.execute(
            "INSERT INTO tasks (id, context_id, from_agent, to_agent, state, created_at, updated_at, directory)
             VALUES (?, ?, 'orchestrator', 'mentor', ?, 't', 't', '/tmp/projx')",
            rusqlite::params![id, ctx, state],
        )
        .unwrap();
    }
    c.execute_batch(
        "INSERT INTO nudges (key, task_id, context_id, target, kind, status, created_at, updated_at)
           VALUES ('k1','aaaaaaaa-1','c1','mentor','assignee','pending','t','t'),
                  ('k2','bbbbbbbb-2','c1','mentor','assignee','delivered','t','t');",
    )
    .unwrap();
    c
}

fn states(c: &rusqlite::Connection) -> Vec<String> {
    let mut st = c.prepare("SELECT state FROM tasks ORDER BY id").unwrap();
    st.query_map([], |r| r.get::<_, String>(0)).unwrap().flatten().collect()
}

fn calls(dir: &Path, needle: &str) -> usize {
    std::fs::read_to_string(dir.join("calls.log")).unwrap_or_default().lines().filter(|l| l.contains(needle)).count()
}

#[test]
fn cancel_all_cancels_open_tasks_kills_pending_nudges_and_interrupts_the_running_turn() {
    let dir = sandbox("all");
    let c = seed(&dir);
    let (out, ok) = oc(&dir, &["cancel", "--all", "--yes"]);
    assert!(ok, "{out}");
    assert_eq!(states(&c), ["canceled", "canceled", "canceled", "canceled", "completed"], "{out}");
    let hist: String = c.query_row("SELECT history FROM tasks WHERE id='aaaaaaaa-1'", [], |r| r.get(0)).unwrap();
    assert!(hist.contains("canceled by user"), "{hist}");
    let k1: String = c.query_row("SELECT status FROM nudges WHERE key='k1'", [], |r| r.get(0)).unwrap();
    let k2: String = c.query_row("SELECT status FROM nudges WHERE key='k2'", [], |r| r.get(0)).unwrap();
    assert_eq!((k1.as_str(), k2.as_str()), ("dead", "delivered"));
    let ev: i64 = c.query_row("SELECT COUNT(*) FROM events WHERE kind='state_change' AND detail='canceled'", [], |r| r.get(0)).unwrap();
    assert_eq!(ev, 4);
    assert_eq!(calls(&dir, "post /api/session/ses_m1/interrupt"), 1, "the running session is interrupted once");
    // nothing left to do
    assert!(oc(&dir, &["cancel", "--all", "--yes"]).0.contains("no open tasks"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancel_can_be_scoped_and_can_leave_running_turns_alone() {
    let dir = sandbox("scoped");
    let c = seed(&dir);
    oc(&dir, &["cancel", "aaaaaaaa", "--yes", "--no-interrupt"]);
    assert_eq!(states(&c), ["canceled", "working", "input-required", "auth-required", "completed"]);
    oc(&dir, &["cancel", "--context", "c2", "--yes", "--no-interrupt"]);
    assert_eq!(states(&c), ["canceled", "working", "canceled", "canceled", "completed"]);
    assert_eq!(calls(&dir, "/interrupt"), 0, "--no-interrupt never touches a session");
    assert!(!oc(&dir, &["cancel"]).1, "a selector is required");
    let _ = std::fs::remove_dir_all(&dir);
}

fn live_sleeper() -> std::process::Child {
    Command::new("sleep").arg("60").spawn().unwrap()
}

#[test]
fn shutdown_waits_for_the_last_opencode_then_ends_everything() {
    let dir = sandbox("shutdown");
    let c = seed(&dir);
    // two opencodes are running (their pids are registered as clients)
    let (mut a, mut b) = (live_sleeper(), live_sleeper());
    let clients = dir.join("d.pid.clients");
    std::fs::create_dir_all(&clients).unwrap();
    std::fs::write(clients.join(a.id().to_string()), "").unwrap();
    std::fs::write(clients.join(b.id().to_string()), "").unwrap();
    // one of them exits: the other is still running, so nothing is touched
    let (out, _) = oc(&dir, &["shutdown", "--client", &a.id().to_string()]);
    assert!(out.trim().is_empty(), "leaving things alone is silent: {out}");
    assert_eq!(states(&c), ["submitted", "working", "input-required", "auth-required", "completed"]);
    // the last one exits: tasks are canceled, the running turn is interrupted, sessions retired
    let (out, ok) = oc(&dir, &["shutdown", "--client", &b.id().to_string()]);
    assert!(ok, "{out}");
    assert_eq!(states(&c), ["canceled", "canceled", "canceled", "canceled", "completed"], "{out}");
    assert_eq!(calls(&dir, "post /api/session/ses_m1/interrupt"), 1);
    let st: String = c.query_row("SELECT status FROM sessions WHERE session_id='ses_m1'", [], |r| r.get(0)).unwrap();
    assert_eq!(st, "ended");
    let _ = (a.kill(), b.kill(), a.wait(), b.wait());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shutdown_can_be_disabled() {
    let dir = sandbox("keep");
    let c = seed(&dir);
    let out = Command::new(env!("CARGO_BIN_EXE_codetypera2a"))
        .args(["oc", "shutdown", "--force"])
        .env("A2A_DB", dir.join("hub.db"))
        .env("A2A_DISPATCHER_PID", dir.join("d.pid"))
        .env("OPENCODE_BIN", dir.join("opencode"))
        .env("A2A_KEEP_RUNNING", "1")
        .env_remove("TMUX")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(states(&c), ["submitted", "working", "input-required", "auth-required", "completed"]);
    let _ = std::fs::remove_dir_all(&dir);
}
