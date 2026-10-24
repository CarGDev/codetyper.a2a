//! `oc stats --ingest` reads Claude Code transcripts into the hub as `claude-code` usage events, once per message id.
use std::path::{Path, PathBuf};
use std::process::Command;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("a2a-claude-usage-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn assistant(id: &str, model: &str, usage: &str) -> String {
    format!(r#"{{"type":"assistant","uuid":"u-{id}","sessionId":"sess-1","timestamp":"2026-10-24T04:03:47.960Z","message":{{"id":"{id}","model":"{model}","usage":{usage}}}}}"#)
}

fn usage(i: i64, cc: i64, cr: i64, o: i64) -> String {
    format!(r#"{{"input_tokens":{i},"cache_creation_input_tokens":{cc},"cache_read_input_tokens":{cr},"output_tokens":{o}}}"#)
}

fn oc(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_codetypera2a"))
        .arg("oc")
        .args(args)
        .env("A2A_DB", dir.join("hub.db"))
        .env("A2A_DISPATCHER_PID", dir.join("d.pid"))
        .env("CLAUDE_CONFIG_DIR", dir.join("claude"))
        .env("OPENCODE_BIN", "/usr/bin/false")
        .env("NO_COLOR", "1")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned(), out.status.success())
}

fn write_transcript(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join("claude/projects").join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

fn rows(dir: &Path) -> Vec<(String, i64, i64, Option<f64>)> {
    let c = rusqlite::Connection::open(dir.join("hub.db")).unwrap();
    let mut st = c.prepare("SELECT model, tokens_in, tokens_out, cost_usd FROM events WHERE source = 'claude-code' ORDER BY ref").unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().flatten().collect()
}

#[test]
fn ingest_counts_each_message_id_once_and_skips_what_is_not_usage() {
    let dir = tmp("rules");
    let lines = [
        assistant("m1", "claude-sonnet-5-5", &usage(2, 100, 10, 5)),
        assistant("m1", "claude-sonnet-5-5", &usage(2, 100, 10, 7)), // the same message again: the larger output wins
        assistant("m2", "claude-future-1", &usage(1, 0, 0, 1)),
        assistant("m3", "<synthetic>", &usage(9, 9, 9, 9)),
        r#"{"type":"assistant","message":{"id":"m4","model":"claude-sonnet-5-5"}}"#.into(),
        r#"{"type":"user","message":{"content":"hi"}}"#.into(),
        "{not json".into(),
    ];
    write_transcript(&dir, "-proj/sess-1.jsonl", &(lines.join("\n") + "\n"));
    let (out, err, ok) = oc(&dir, &["stats", "--ingest"]);
    assert!(ok && out.contains("2 new usage rows"), "{out}{err}");
    let got = rows(&dir);
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!((got[0].0.as_str(), got[0].1, got[0].2), ("claude-sonnet-5-5", 112, 7), "in = input + cache creation + cache read");
    assert!(got[0].3.is_some(), "claude-sonnet-5-5 is in the price table");
    assert_eq!((got[1].0.as_str(), got[1].3), ("claude-future-1", None), "no invented price");
    let (out, _, _) = oc(&dir, &["stats", "--ingest"]);
    assert!(out.contains("0 new usage rows"), "{out}");
    assert_eq!(rows(&dir).len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_truncated_last_line_waits_for_its_newline_and_nested_transcripts_are_read() {
    let dir = tmp("partial");
    let whole = assistant("m1", "claude-sonnet-5-5", &usage(1, 0, 0, 1));
    let next = assistant("m2", "claude-sonnet-5-5", &usage(2, 0, 0, 2));
    let (head, tail) = next.split_at(next.len() / 2);
    let path = write_transcript(&dir, "-proj/sess-1.jsonl", &format!("{whole}\n{head}"));
    write_transcript(&dir, "-proj/sess-1/subagents/agent-a.jsonl", &(assistant("s1", "claude-sonnet-5-5", &usage(3, 0, 0, 3)) + "\n"));
    let (out, err, ok) = oc(&dir, &["stats", "--ingest"]);
    assert!(ok && out.contains("2 new usage rows"), "{out}{err}");
    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    std::io::Write::write_all(&mut f, format!("{tail}\n").as_bytes()).unwrap();
    let (out, _, _) = oc(&dir, &["stats", "--ingest"]);
    assert!(out.contains("1 new usage rows"), "{out}");
    assert_eq!(rows(&dir).len(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stats_model_shows_one_table_per_source_and_a_broken_claude_dir_never_breaks_it() {
    let dir = tmp("render");
    write_transcript(&dir, "-proj/sess-1.jsonl", &(assistant("m1", "claude-sonnet-5-5", &usage(2, 100, 10, 5)) + "\n"));
    let (out, err, ok) = oc(&dir, &["stats", "--model"]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("claude code") && out.contains("claude-sonnet-5-5") && !out.contains("opencode\n"), "{out}");
    assert!(out.contains("include cache"), "{out}");
    assert_eq!(rows(&dir).len(), 1);
    let (again, _, _) = oc(&dir, &["stats", "--model"]);
    assert_eq!(again, out, "a second run changes nothing");
    // an unreadable transcript root is a warning, not a failure
    std::fs::remove_dir_all(dir.join("claude")).unwrap();
    std::fs::write(dir.join("claude"), "a file, not a dir").unwrap();
    let (out, err, ok) = oc(&dir, &["stats", "--model"]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("claude-sonnet-5-5"), "rows already stored still render: {out}");
    let _ = std::fs::remove_dir_all(&dir);
}
