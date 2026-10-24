//! `oc statusline`: the one-line footer Claude Code shows (hub tasks in flight, subagents running now)
//! and `oc statusline --hook start|stop`, the SubagentStart / SubagentStop hook that feeds it.
//! `--hook inbox` is the UserPromptSubmit hook: it hands the requester (`A2A_REQUESTER`, default
//! `orchestrator`) the results that finished since the last prompt as additional context, and marks
//! them seen (`tasks.requester_seen_at`, the same mark `a2a_inbox as=requester` uses). It never
//! creates or migrates the hub. `--hook ensure` is the SessionStart hook: inside tmux it starts
//! `oc ensure` (dispatcher + `a2a-status` window) detached and prints nothing, because SessionStart
//! stdout becomes model context. All of them must be fast and exit 0 always.
use crate::{paths, sys};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// A subagent entry older than this is a crashed or missed stop and is ignored.
pub const STALE_SECS: u64 = 600;
const MAX_TASKS_SHOWN: usize = 2;

#[derive(Default, Debug)]
pub struct Snapshot {
    /// `None` when there is no hub database yet.
    pub dispatcher: Option<bool>,
    /// (from, to) of every open task.
    pub tasks: Vec<(String, String)>,
    /// Agent types of the subagents running now.
    pub agents: Vec<String>,
    /// Finished tasks the requester has not been told about yet.
    pub results: usize,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hook {
    Start,
    Stop,
    Inbox,
    Ensure,
}

fn counted(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

pub fn render(s: &Snapshot) -> String {
    let dot = match s.dispatcher {
        Some(true) => " ●",
        Some(false) => " ○",
        None => "",
    };
    let mut line = format!("a2a{dot}");
    if !s.tasks.is_empty() {
        let shown: Vec<String> = s.tasks.iter().take(MAX_TASKS_SHOWN).map(|(a, b)| format!("{a}>{b}")).collect();
        let more = s.tasks.len().saturating_sub(MAX_TASKS_SHOWN);
        let more = if more > 0 { format!(" +{more}") } else { String::new() };
        line += &format!(" · {}: {}{more}", counted(s.tasks.len(), "task"), shown.join(", "));
    }
    if !s.agents.is_empty() {
        line += &format!(" · {}: {}", counted(s.agents.len(), "subagent"), s.agents.join(", "));
    }
    if s.results > 0 {
        line += &format!(" · {} waiting", counted(s.results, "result"));
    }
    line
}

/// A filename-safe agent id; ids come from hook stdin, so they are never used as paths.
fn safe_id(v: &Value) -> Option<&str> {
    v.as_str().filter(|id| !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
}

/// One file per running subagent (`<dir>/<agent_id>`), written atomically, so hooks that fire at the
/// same moment never lose each other's update. Returns whether the input named an agent.
pub fn record(dir: &Path, stdin: &str, hook: Hook, now: u64) -> bool {
    let Ok(input) = serde_json::from_str::<Value>(stdin) else { return false };
    let Some(id) = safe_id(&input["agent_id"]) else { return false };
    let file = dir.join(id);
    if hook == Hook::Stop {
        let _ = std::fs::remove_file(&file);
        return true;
    }
    let kind = input["agent_type"].as_str().filter(|t| !t.is_empty()).unwrap_or("agent");
    let body = serde_json::json!({ "type": kind, "started": now }).to_string();
    let tmp = dir.join(format!(".{id}.{}.tmp", std::process::id()));
    let written = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&tmp, body)).and_then(|_| std::fs::rename(&tmp, &file));
    let _ = written.is_err().then(|| std::fs::remove_file(&tmp));
    true
}

/// Agent types of the entries that are not stale, oldest first.
pub fn running(dir: &Path, now: u64) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut live: Vec<(u64, String)> = entries
        .filter_map(Result::ok)
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| serde_json::from_str::<Value>(&std::fs::read_to_string(e.path()).ok()?).ok())
        .filter_map(|v| Some((v["started"].as_u64()?, v["type"].as_str()?.to_string())))
        .filter(|(started, _)| now.saturating_sub(*started) <= STALE_SECS)
        .collect();
    live.sort();
    live.into_iter().map(|(_, kind)| kind).collect()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Open tasks as (from, to), read-only: a missing or unreadable hub is just "no tasks".
fn open_tasks(db: &Path) -> Option<Vec<(String, String)>> {
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.busy_timeout(std::time::Duration::from_millis(300)).ok()?;
    let states = super::panel::OPEN.map(|s| format!("'{s}'")).join(",");
    let mut st = conn.prepare(&format!("SELECT from_agent, to_agent FROM tasks WHERE state IN ({states}) ORDER BY created_at")).ok()?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).ok()?;
    Some(rows.filter_map(Result::ok).collect())
}

/// Task states that mean the requester has something to read.
const FINISHED: [&str; 5] = ["input-required", "auth-required", "completed", "failed", "rejected"];

fn requester() -> String {
    std::env::var("A2A_REQUESTER").ok().filter(|r| !r.is_empty()).unwrap_or_else(|| "orchestrator".into())
}

/// A finished task the requester has not been told about.
struct Pending {
    id: String,
    to: String,
    state: String,
    text: String,
}

fn status_text(status_message: Option<&str>) -> String {
    let parts = status_message.and_then(|m| serde_json::from_str::<Value>(m).ok()).map(|m| m["parts"].clone()).unwrap_or(Value::Null);
    let text: Vec<&str> = parts.as_array().into_iter().flatten().filter_map(|p| p["text"].as_str()).collect();
    text.join(" ").chars().take(400).collect()
}

/// Finished tasks `agent` sent that it has not seen (never seen, or changed since), newest last. With
/// `cwd` only those of that project.
fn pending(conn: &rusqlite::Connection, agent: &str, cwd: Option<&str>) -> Option<Vec<Pending>> {
    let states = FINISHED.map(|s| format!("'{s}'")).join(",");
    let mut st = conn
        .prepare(&format!(
            "SELECT id, to_agent, state, status_message, directory, from_directory FROM tasks
             WHERE from_agent = ? AND to_agent != from_agent AND state IN ({states})
               AND (requester_seen_at IS NULL OR updated_at > requester_seen_at) ORDER BY updated_at"
        ))
        .ok()?;
    let rows = st
        .query_map([agent], |r| {
            let (own, dir): (Option<String>, Option<String>) = (r.get(5)?, r.get(4)?);
            Ok((Pending { id: r.get(0)?, to: r.get(1)?, state: r.get(2)?, text: status_text(r.get::<_, Option<String>>(3)?.as_deref()) }, own.or(dir.clone()), dir))
        })
        .ok()?;
    let here = |own: &Option<String>, dir: &Option<String>| match cwd {
        None => true,
        Some(c) => [own, dir].into_iter().flatten().any(|d| d == c || crate::hub::project::same_project(c, d)),
    };
    Some(rows.filter_map(Result::ok).filter(|(_, own, dir)| here(own, dir)).map(|(p, _, _)| p).collect())
}

fn results_context(list: &[Pending]) -> String {
    let lines: Vec<String> = list.iter().map(|p| format!("- task {} ({}) is {}: {}", &p.id[..8.min(p.id.len())], p.to, p.state, p.text)).collect();
    format!(
        "A2A results waiting for you ({}):\n{}\nRead one in full with mcp__a2a__get_task {{id}}; answer a question with mcp__a2a__send_message {{taskId}}.",
        list.len(),
        lines.join("\n")
    )
}

/// The UserPromptSubmit hook: prints the context JSON and marks the results seen. Silent when there is
/// nothing to say, no hub, or the input is not a hook payload.
fn inbox(stdin: &str, db: &Path) -> Option<String> {
    let cwd = serde_json::from_str::<Value>(stdin).ok()?["cwd"].as_str().map(String::from)?;
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE).ok()?;
    conn.busy_timeout(std::time::Duration::from_millis(300)).ok()?;
    let list = pending(&conn, &requester(), Some(&cwd)).filter(|l| !l.is_empty())?;
    let out = serde_json::json!({ "hookSpecificOutput": { "hookEventName": "UserPromptSubmit", "additionalContext": results_context(&list) } });
    let ts = crate::hub::db::now();
    list.iter().for_each(|p| {
        let _ = conn.execute("UPDATE tasks SET requester_seen_at = ? WHERE id = ?", rusqlite::params![ts, p.id]);
    });
    Some(out.to_string())
}

fn results_waiting(db: &Path) -> usize {
    let open = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok();
    open.and_then(|c| pending(&c, &requester(), None)).map_or(0, |l| l.len())
}

fn snapshot(now: u64) -> Snapshot {
    let db: PathBuf = paths::db_path();
    let tasks = db.is_file().then(|| open_tasks(&db)).flatten();
    Snapshot {
        results: if tasks.is_some() { results_waiting(&db) } else { 0 },
        dispatcher: tasks.as_ref().map(|_| sys::dispatcher_pid(&paths::dispatcher_pid_path()).is_some()),
        tasks: tasks.unwrap_or_default(),
        agents: running(&paths::statusline_dir(), now),
    }
}

fn read_stdin() -> String {
    use std::io::{IsTerminal, Read};
    let mut text = String::new();
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return text;
    }
    let _ = stdin.lock().take(1 << 20).read_to_string(&mut text);
    text
}

/// The arguments that make this binary run `oc ensure`: only inside tmux, and without the `oc` word when
/// the binary itself is called `oc` (that name already is the subcommand).
fn ensure_args(exe_name: &str, tmux: Option<&str>) -> Option<Vec<&'static str>> {
    tmux.filter(|t| !t.is_empty())?;
    Some(match exe_name {
        "oc" => vec!["ensure"],
        _ => vec!["oc", "ensure"],
    })
}

/// Start `oc ensure` and return at once; its output goes nowhere.
fn spawn_ensure() {
    let Ok(exe) = std::env::current_exe() else { return };
    let name = exe.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
    let Some(args) = ensure_args(&name, std::env::var("TMUX").ok().as_deref()) else { return };
    let _ = sys::spawn_detached(&exe, &args, Path::new("/dev/null"));
}

/// `oc statusline [--hook start|stop]`; always exits 0 so it can never break Claude Code.
pub fn run(args: &[String]) -> i32 {
    let hook = match args.iter().position(|a| a == "--hook").and_then(|i| args.get(i + 1)).map(String::as_str) {
        Some("start") => Some(Hook::Start),
        Some("stop") => Some(Hook::Stop),
        Some("inbox") => Some(Hook::Inbox),
        Some("ensure") => Some(Hook::Ensure),
        _ => None,
    };
    match hook {
        Some(Hook::Inbox) => {
            let db = paths::db_path();
            if let Some(out) = db.is_file().then(|| inbox(&read_stdin(), &db)).flatten() {
                println!("{out}");
            }
        }
        Some(Hook::Ensure) => spawn_ensure(),
        Some(h) => {
            record(&paths::statusline_dir(), &read_stdin(), h, now_secs());
        }
        None => println!("{}", render(&snapshot(now_secs()))),
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("a2a-statusline-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn tasks(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn render_is_a_single_short_line() {
        let idle = Snapshot { dispatcher: Some(true), ..Snapshot::default() };
        assert_eq!(render(&idle), "a2a ●");
        assert_eq!(render(&Snapshot::default()), "a2a");
        let down = Snapshot { dispatcher: Some(false), ..Snapshot::default() };
        assert_eq!(render(&down), "a2a ○");
        let busy = Snapshot {
            dispatcher: Some(true),
            tasks: tasks(&[("orchestrator", "coder"), ("coder", "tester"), ("a", "b")]),
            agents: vec!["Explore".into(), "Plan".into()],
            results: 0,
        };
        let line = render(&busy);
        assert_eq!(line, "a2a ● · 3 tasks: orchestrator>coder, coder>tester +1 · 2 subagents: Explore, Plan");
        assert!(!line.contains('\n'));
    }

    #[test]
    fn ensure_runs_only_inside_tmux_and_never_doubles_the_oc_word() {
        assert_eq!(ensure_args("codetypera2a", Some("/tmp/tmux-1/default,123,0")), Some(vec!["oc", "ensure"]));
        assert_eq!(ensure_args("oc", Some("/tmp/tmux-1/default,123,0")), Some(vec!["ensure"]));
        assert_eq!(ensure_args("codetypera2a", None), None);
        assert_eq!(ensure_args("codetypera2a", Some("")), None);
    }

    #[test]
    fn render_pluralises_and_names_one_subagent() {
        let s = Snapshot { dispatcher: None, tasks: tasks(&[("a", "b")]), agents: vec!["Explore".into()], results: 1 };
        assert_eq!(render(&s), "a2a · 1 task: a>b · 1 subagent: Explore · 1 result waiting");
    }

    #[test]
    fn hook_start_then_stop_records_and_clears_a_subagent() {
        let dir = tmp("hook");
        assert!(record(&dir, r#"{"agent_id":"ag-1","agent_type":"Explore","session_id":"s"}"#, Hook::Start, 1000));
        assert!(record(&dir, r#"{"agent_id":"ag-2","agent_type":"Plan"}"#, Hook::Start, 1001));
        assert_eq!(running(&dir, 1002), vec!["Explore", "Plan"]);
        assert!(record(&dir, r#"{"agent_id":"ag-1"}"#, Hook::Stop, 1003));
        assert_eq!(running(&dir, 1004), vec!["Plan"]);
        assert!(!dir.read_dir().unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains(".tmp")), "no temp files left");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_entries_expire_and_bad_input_is_ignored() {
        let dir = tmp("stale");
        record(&dir, r#"{"agent_id":"old","agent_type":"Explore"}"#, Hook::Start, 1000);
        assert_eq!(running(&dir, 1000 + STALE_SECS), vec!["Explore"]);
        assert!(running(&dir, 1000 + STALE_SECS + 1).is_empty(), "older than the limit is ignored");
        assert!(!record(&dir, "not json", Hook::Start, 1), "garbage stdin records nothing");
        assert!(!record(&dir, r#"{"agent_type":"x"}"#, Hook::Start, 1), "no agent id, nothing to key on");
        assert!(!record(&dir, r#"{"agent_id":"../../etc"}"#, Hook::Start, 1), "ids are not paths");
        assert!(running(&tmp("missing"), 5).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
