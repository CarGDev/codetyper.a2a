//! Data for the opencode panel plugin (`plugins/a2a/panels.tsx`): `oc status --json` and `oc answer`.
use crate::hub::db::{self, TaskRow};
use crate::{paths, sys};
use serde_json::{Map, Value, json};

pub(crate) const OPEN: [&str; 4] = ["submitted", "working", "input-required", "auth-required"];

/// The text of an A2A message (`{"parts":[{"text":..}]}`), parts joined by a newline.
pub fn message_text(raw: &str) -> String {
    let v: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
    let parts = v["parts"].as_array().cloned().unwrap_or_default();
    parts
        .iter()
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Numbered options in a question: lines such as `1) fix it`, `2. leave it` or `3: skip`.
pub fn parse_options(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim().trim_start_matches(['-', '*', ' ']);
            let digits = l.chars().take_while(char::is_ascii_digit).count();
            let rest = l[digits..].strip_prefix([')', '.', ':'])?;
            (digits > 0 && !rest.trim().is_empty()).then(|| rest.trim().to_string())
        })
        .collect()
}

fn task_json(d: &rusqlite::Connection, t: &TaskRow, max_hops: i64) -> Value {
    let question = (t.state == "input-required" || t.state == "auth-required").then(|| {
        let text = message_text(t.status_message.as_deref().unwrap_or(""));
        json!({ "text": text, "options": parse_options(&text) })
    });
    json!({
        "id": t.id, "short": &t.id[..8.min(t.id.len())], "context": t.context_id,
        "from": t.from_agent, "to": t.to_agent, "state": t.state, "updated_at": t.updated_at,
        "directory": t.directory, "question": question,
        "hop_capped": OPEN.contains(&t.state.as_str()) && super::oc::hop_capped(d, &t.context_id, max_hops),
    })
}

/// `oc status --json [--dir <folder>]`: dispatcher state, the tasks of the last hour (open ones first)
/// and today's token/cost totals, for the plugin to draw.
pub fn status_json(args: &[String]) -> Result<Value, String> {
    let dir = args
        .iter()
        .position(|a| a == "--dir")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let (d, _) = db::open_read_compat(&paths::db_path())?;
    let owner = paths::resolve_runtime_home(&paths::runtime_home(), &paths::legacy_home());
    let max_hops = paths::env_u64("DISPATCH_MAX_HOPS", 40) as i64;
    let since = db::iso_ago_ms(3_600_000);
    let mut tasks = db::tasks(
        &d,
        "WHERE state IN ('submitted','working','input-required','auth-required') OR updated_at >= ? ORDER BY created_at",
        &[&since],
    )?;
    if let Some(dir) = &dir {
        tasks.retain(|t| {
            [t.directory.as_deref(), t.from_directory.as_deref()]
                .into_iter()
                .flatten()
                .any(|x| crate::hub::project::same_project(dir, x))
        });
    }
    let today = db::iso_ago_ms(86_400_000);
    let (tin, tout, cost): (i64, i64, f64) = d
        .query_row(
            "SELECT COALESCE(SUM(tokens_in),0), COALESCE(SUM(tokens_out),0), COALESCE(SUM(cost_usd),0.0) FROM events WHERE kind='usage' AND COALESCE(source,'opencode')='opencode' AND ts >= ?",
            [&today],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap_or((0, 0, 0.0));
    let done = tasks.iter().filter(|t| t.state == "completed").count();
    Ok(json!({
        "dispatcher": sys::dispatcher_pid(&paths::runtime_paths(&paths::resolve_runtime_home(
            &paths::runtime_home(),
            &paths::legacy_home(),
        )).pid).is_some(),
        "tasks": tasks.iter().map(|t| task_json(&d, t, max_hops)).collect::<Vec<_>>(),
        "progress": { "done": done, "total": tasks.len() },
        "today": { "tokens_in": tin, "tokens_out": tout, "cost_usd": cost },
        "diagnostics": diagnostics(&d, owner.legacy),
    }))
}

/// Read-only, bounded health snapshot (additive to `status_json`). Unavailable values are `null`.
fn diagnostics(d: &rusqlite::Connection, legacy_owner: bool) -> Value {
    let count = |sql: &str| -> Value {
        d.query_row(sql, [], |r| r.get::<_, i64>(0))
            .map_or(Value::Null, |n| json!(n))
    };
    let oldest: Option<String> = d
        .query_row(
            "SELECT MIN(created_at) FROM tasks WHERE state IN ('submitted','working','input-required','auth-required')",
            [],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let age =
        oldest.map(|t| ((chrono::Utc::now().timestamp_millis() - db::parse_ms(&t)) / 1000).max(0));
    let since = db::iso_ago_ms(86_400_000);
    let events = |kind: &str| -> Value {
        d.query_row(
            "SELECT COUNT(*) FROM events WHERE kind = ? AND ts >= ?",
            [kind, &since],
            |r| r.get::<_, i64>(0),
        )
        .map_or(Value::Null, |n| json!(n))
    };
    json!({
        "owner": if legacy_owner { "legacy" } else { "canonical" },
        "guard_enforced": paths::runtime_home().join("auth").join("trusted-keys").exists(),
        "open": {
            "submitted": count("SELECT COUNT(*) FROM tasks WHERE state='submitted'"),
            "working": count("SELECT COUNT(*) FROM tasks WHERE state='working'"),
            "input_required": count("SELECT COUNT(*) FROM tasks WHERE state='input-required'"),
        },
        "oldest_open_age_secs": age,
        "last_24h": { "stall_retry": events("stall_retry"), "stall_failed": events("stall_failed") },
    })
}

/// Longest message text `oc messages --json` returns; the pane draws a few lines, not whole reports.
const MESSAGE_TEXT_MAX: usize = 600;
const MESSAGES_DEFAULT_LIMIT: usize = 50;

fn trimmed(text: &str) -> String {
    let t = text.trim();
    if t.chars().count() <= MESSAGE_TEXT_MAX {
        return t.to_string();
    }
    let cut: String = t.chars().take(MESSAGE_TEXT_MAX).collect();
    format!("{cut}...")
}

/// The other end of a message: the requester wrote it, so it goes to the assignee, and the reverse.
fn counterpart<'a>(t: &'a TaskRow, from: &str) -> &'a str {
    if from == t.from_agent {
        &t.to_agent
    } else {
        &t.from_agent
    }
}

/// Every history message of the most recently touched tasks, newest first, `since` (ISO) and `limit` applied.
pub fn messages_of(
    d: &rusqlite::Connection,
    since: Option<&str>,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let tasks = db::tasks(d, "ORDER BY updated_at DESC LIMIT 200", &[])?;
    let mut out: Vec<(String, Value)> = Vec::new();
    for t in &tasks {
        let history: Value = serde_json::from_str(&t.history).unwrap_or(Value::Null);
        for m in history.as_array().into_iter().flatten() {
            let ts = m["timestamp"].as_str().unwrap_or(&t.updated_at).to_string();
            if since.is_some_and(|s| ts.as_str() < s) {
                continue;
            }
            let from = m["from"].as_str().unwrap_or(&t.from_agent);
            let text = trimmed(&message_text(&m.to_string()));
            let msg = json!({
                "task": t.id, "short": &t.id[..8.min(t.id.len())], "context": t.context_id,
                "from": from, "to": counterpart(t, from), "state": t.state, "text": text, "ts": ts,
            });
            out.push((ts, msg));
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(out.into_iter().take(limit).map(|(_, m)| m).collect())
}

/// `oc messages --json [--since <iso>] [--limit N]`: the latest task messages, newest first, read-only.
pub fn messages_json(args: &[String]) -> Result<Value, String> {
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
    };
    let limit = flag("--limit").map_or(Ok(MESSAGES_DEFAULT_LIMIT), |v| {
        v.parse()
            .map_err(|_| format!("--limit needs a number, got {v}"))
    })?;
    let (d, _) = db::open_read_compat(&paths::db_path())?;
    Ok(json!({ "messages": messages_of(&d, flag("--since").map(String::as_str), limit)? }))
}

/// `oc answer <task-id> <text...>`: reply to a task waiting for the requester (what `a2a_send_message {taskId}` does).
pub fn answer(args: &[String]) -> Result<String, String> {
    let (Some(id), text) = (args.first(), args[1.min(args.len())..].join(" ")) else {
        return Err("usage: oc answer <task-id> <text>".into());
    };
    if text.trim().is_empty() {
        return Err("usage: oc answer <task-id> <text>".into());
    }
    let d = db::open(&paths::db_path())?;
    let open = db::tasks(
        &d,
        "WHERE state IN ('submitted','working','input-required','auth-required')",
        &[],
    )?;
    let hits: Vec<&TaskRow> = open
        .iter()
        .filter(|t| t.id.starts_with(id.as_str()))
        .collect();
    let [t] = hits.as_slice() else {
        return Err(format!("{} open task(s) match {id}", hits.len()));
    };
    let hub = crate::hub::server::Hub::open(&paths::db_path(), &paths::agent_dir())?;
    let mut a = Map::new();
    a.insert("from".into(), json!(t.from_agent));
    a.insert("taskId".into(), json!(t.id));
    a.insert("text".into(), json!(text));
    hub.call("send_message", &a)
        .ok_or("send_message missing")??;
    Ok(format!(
        "answered {} ({} -> {})",
        &t.id[..8.min(t.id.len())],
        t.from_agent,
        t.to_agent
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_the_numbered_lines() {
        let q = "Which way?\n1) fix the test\n2. change the code\n- 3: skip it\nnot numbered\n4)";
        assert_eq!(
            parse_options(q),
            vec!["fix the test", "change the code", "skip it"]
        );
        assert!(parse_options("no options here").is_empty());
    }

    #[test]
    fn message_text_joins_the_text_parts() {
        let m = r#"{"role":"agent","parts":[{"text":"a"},{"data":1},{"text":"b"}]}"#;
        assert_eq!(message_text(m), "a\nb");
        assert_eq!(message_text("not json"), "");
    }

    fn seeded() -> rusqlite::Connection {
        let path = std::env::temp_dir().join(format!(
            "a2a-messages-{}-{:?}.db",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let d = db::open(&path).unwrap();
        let ts = "2026-01-01T00:00:00.000Z";
        for a in ["orchestrator", "code-reviewer"] {
            d.execute(
                "INSERT INTO agents (name, card, created_at, updated_at) VALUES (?, '{}', ?, ?)",
                [a, ts, ts],
            )
            .unwrap();
        }
        d.execute("INSERT INTO contexts (id, state, created_at, updated_at) VALUES ('c1', 'working', ?, ?)", [ts, ts]).unwrap();
        let history = r#"[{"role":"user","from":"orchestrator","parts":[{"text":"review src"}],"timestamp":"2026-01-01T00:00:01.000Z"},
            {"role":"agent","from":"code-reviewer","parts":[{"text":"  which branch?  "}],"timestamp":"2026-01-01T00:00:02.000Z"}]"#;
        d.execute(
            "INSERT INTO tasks (id, context_id, from_agent, to_agent, state, history, created_at, updated_at) VALUES ('abcdef123456', 'c1', 'orchestrator', 'code-reviewer', 'input-required', ?, ?, ?)",
            [history, ts, ts],
        )
        .unwrap();
        d
    }

    #[test]
    fn messages_come_newest_first_with_both_ends_and_trimmed_text() {
        let m = messages_of(&seeded(), None, 10).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0]["text"], "which branch?");
        assert_eq!(
            (m[0]["from"].as_str(), m[0]["to"].as_str()),
            (Some("code-reviewer"), Some("orchestrator"))
        );
        assert_eq!(
            (m[0]["short"].as_str(), m[0]["state"].as_str()),
            (Some("abcdef12"), Some("input-required"))
        );
        assert_eq!(m[1]["to"], "code-reviewer");
    }

    #[test]
    fn messages_honour_since_and_limit_and_cap_long_text() {
        let d = seeded();
        assert_eq!(
            messages_of(&d, Some("2026-01-01T00:00:02.000Z"), 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(messages_of(&d, None, 1).unwrap().len(), 1);
        assert!(trimmed(&"x".repeat(MESSAGE_TEXT_MAX + 50)).ends_with("..."));
    }
}
