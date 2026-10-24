//! Data for the opencode panel plugin (`plugins/a2a/panels.tsx`): `oc status --json` and `oc answer`.
use crate::hub::db::{self, TaskRow};
use crate::{paths, sys};
use serde_json::{Map, Value, json};

const OPEN: [&str; 4] = ["submitted", "working", "input-required", "auth-required"];

/// The text of an A2A message (`{"parts":[{"text":..}]}`), parts joined by a newline.
pub fn message_text(raw: &str) -> String {
    let v: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
    let parts = v["parts"].as_array().cloned().unwrap_or_default();
    parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n")
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
    let dir = args.iter().position(|a| a == "--dir").and_then(|i| args.get(i + 1)).cloned();
    let d = db::open(&paths::db_path())?;
    let max_hops = paths::env_u64("DISPATCH_MAX_HOPS", 40) as i64;
    let since = db::iso_ago_ms(3_600_000);
    let mut tasks = db::tasks(&d, "WHERE state IN ('submitted','working','input-required','auth-required') OR updated_at >= ? ORDER BY created_at", &[&since])?;
    if let Some(dir) = &dir {
        tasks.retain(|t| [t.directory.as_deref(), t.from_directory.as_deref()].into_iter().flatten().any(|x| crate::hub::project::same_project(dir, x)));
    }
    let today = db::iso_ago_ms(86_400_000);
    let (tin, tout, cost): (i64, i64, f64) = d
        .query_row(
            "SELECT COALESCE(SUM(tokens_in),0), COALESCE(SUM(tokens_out),0), COALESCE(SUM(cost_usd),0.0) FROM events WHERE kind='usage' AND ts >= ?",
            [&today],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap_or((0, 0, 0.0));
    let done = tasks.iter().filter(|t| t.state == "completed").count();
    Ok(json!({
        "dispatcher": sys::dispatcher_pid(&paths::dispatcher_pid_path()).is_some(),
        "tasks": tasks.iter().map(|t| task_json(&d, t, max_hops)).collect::<Vec<_>>(),
        "progress": { "done": done, "total": tasks.len() },
        "today": { "tokens_in": tin, "tokens_out": tout, "cost_usd": cost },
    }))
}

/// `oc answer <task-id> <text...>`: reply to a task waiting for the requester (what `a2a_send_message {taskId}` does).
pub fn answer(args: &[String]) -> Result<String, String> {
    let (Some(id), text) = (args.first(), args[1.min(args.len())..].join(" ")) else { return Err("usage: oc answer <task-id> <text>".into()) };
    if text.trim().is_empty() {
        return Err("usage: oc answer <task-id> <text>".into());
    }
    let d = db::open(&paths::db_path())?;
    let open = db::tasks(&d, "WHERE state IN ('submitted','working','input-required','auth-required')", &[])?;
    let hits: Vec<&TaskRow> = open.iter().filter(|t| t.id.starts_with(id.as_str())).collect();
    let [t] = hits.as_slice() else { return Err(format!("{} open task(s) match {id}", hits.len())) };
    let hub = crate::hub::server::Hub::open(&paths::db_path(), &paths::agent_dir())?;
    let mut a = Map::new();
    a.insert("from".into(), json!(t.from_agent));
    a.insert("taskId".into(), json!(t.id));
    a.insert("text".into(), json!(text));
    hub.call("send_message", &a).ok_or("send_message missing")??;
    Ok(format!("answered {} ({} -> {})", &t.id[..8.min(t.id.len())], t.from_agent, t.to_agent))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_the_numbered_lines() {
        let q = "Which way?\n1) fix the test\n2. change the code\n- 3: skip it\nnot numbered\n4)";
        assert_eq!(parse_options(q), vec!["fix the test", "change the code", "skip it"]);
        assert!(parse_options("no options here").is_empty());
    }

    #[test]
    fn message_text_joins_the_text_parts() {
        let m = r#"{"role":"agent","parts":[{"text":"a"},{"data":1},{"text":"b"}]}"#;
        assert_eq!(message_text(m), "a\nb");
        assert_eq!(message_text("not json"), "");
    }
}
