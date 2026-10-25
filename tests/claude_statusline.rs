use std::io::Write;
use std::process::{Command, Stdio};

fn tmp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("a2a-cc-statusline-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn oc(dir: &std::path::Path, args: &[&str], stdin: &str) -> (String, bool) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_codetypera2a"))
        .arg("oc")
        .args(args)
        .env("A2A_DB", dir.join("hub.db"))
        .env("A2A_DISPATCHER_PID", dir.join("d.pid"))
        .env("A2A_STATUSLINE_DIR", dir.join("sub"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

#[test]
fn statusline_prints_one_line_and_exits_zero_on_any_stdin() {
    let dir = tmp("line");
    for stdin in [
        "",
        "{}",
        "garbage \u{0} {{{",
        r#"{"model":{"display_name":"x"}}"#,
    ] {
        let (out, ok) = oc(&dir, &["statusline"], stdin);
        assert!(ok, "stdin {stdin:?}");
        assert_eq!(out, "a2a\n", "no hub database yet, so just the name");
    }
    assert!(
        !dir.join("hub.db").exists(),
        "the status line must not create or migrate the hub"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hooks_make_the_line_show_and_forget_a_subagent() {
    let dir = tmp("hooks");
    oc(
        &dir,
        &["statusline", "--hook", "start"],
        r#"{"agent_id":"a1","agent_type":"Explore","session_id":"s"}"#,
    );
    assert_eq!(
        oc(&dir, &["statusline"], "{}").0,
        "a2a · 1 subagent: Explore\n"
    );
    let (_, ok) = oc(
        &dir,
        &["statusline", "--hook", "stop"],
        r#"{"agent_id":"a1"}"#,
    );
    assert!(ok);
    assert_eq!(oc(&dir, &["statusline"], "{}").0, "a2a\n");
    assert!(
        oc(&dir, &["statusline", "--hook", "start"], "not json").1,
        "a bad hook payload still exits 0"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn init_claude_adds_the_status_line_once() {
    let dir = tmp("init");
    let run = || {
        let out = Command::new(env!("CARGO_BIN_EXE_codetypera2a"))
            .args(["init", "--claude", "--yes", "--no-models"])
            .env("CLAUDE_CONFIG_DIR", dir.join("claude"))
            .env("OPENCODE_CONFIG_DIR", dir.join("oc"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::read_to_string(dir.join("claude/settings.json")).unwrap()
    };
    let first = run();
    assert!(
        first.contains("\"statusLine\"")
            && first.contains("--hook start")
            && first.contains("--hook stop"),
        "{first}"
    );
    assert_eq!(run(), first, "a second run changes nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A hub with two finished tasks for the orchestrator (one in this folder, one elsewhere) and one still running.
fn seeded(name: &str) -> std::path::PathBuf {
    let dir = tmp(name);
    let st = Command::new(env!("CARGO_BIN_EXE_codetypera2a"))
        .args(["oc", "continue", "__statusline_seed__"])
        .env("A2A_DB", dir.join("hub.db"))
        .env("A2A_DISPATCHER_PID", dir.join("d.pid"))
        .env("OPENCODE_BIN", "/usr/bin/false")
        .env_remove("TMUX")
        .output()
        .unwrap();
    assert!(st.status.success());
    let c = rusqlite::Connection::open(dir.join("hub.db")).unwrap();
    c.execute(
        "DELETE FROM hop_allow WHERE context_id='__statusline_seed__'",
        [],
    )
    .unwrap();
    c.execute("DELETE FROM contexts WHERE id='__statusline_seed__'", [])
        .unwrap();
    c.execute_batch(
        "INSERT INTO agents (name, card, created_at, updated_at) VALUES ('orchestrator','{}','t','t'), ('mentor','{}','t','t');
         INSERT INTO contexts (id, created_at, updated_at) VALUES ('c1','t','t');",
    )
    .unwrap();
    for (id, state, dir_, msg) in [
        ("aaaaaaaa-1", "completed", "/proj/a", "the review is done"),
        (
            "bbbbbbbb-2",
            "input-required",
            "/elsewhere",
            "which branch?",
        ),
        ("cccccccc-3", "working", "/proj/a", "busy"),
    ] {
        let status = serde_json::json!({"role":"agent","parts":[{"text":msg}]}).to_string();
        c.execute(
            "INSERT INTO tasks (id, context_id, from_agent, to_agent, state, status_message, created_at, updated_at, directory, from_directory)
             VALUES (?, 'c1', 'orchestrator', 'mentor', ?, ?, '2026-10-24T00:00:00.000Z', '2026-10-24T00:00:01.000Z', ?, ?)",
            rusqlite::params![id, state, status, dir_, dir_],
        )
        .unwrap();
    }
    dir
}

#[test]
fn the_inbox_hook_adds_the_finished_results_of_this_folder_once() {
    let dir = seeded("inbox");
    let (out, ok) = oc(
        &dir,
        &["statusline", "--hook", "inbox"],
        r#"{"hook_event_name":"UserPromptSubmit","cwd":"/proj/a","prompt":"hi"}"#,
    );
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    let text = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        text.contains("aaaaaaaa")
            && text.contains("completed")
            && text.contains("the review is done"),
        "{text}"
    );
    assert!(text.contains("mcp__a2a__get_task"), "{text}");
    assert!(
        !text.contains("bbbbbbbb") && !text.contains("cccccccc"),
        "other folders and running tasks stay out: {text}"
    );
    let (again, ok) = oc(
        &dir,
        &["statusline", "--hook", "inbox"],
        r#"{"cwd":"/proj/a"}"#,
    );
    assert!(
        ok && again.trim().is_empty(),
        "delivered once, then silent: {again:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_inbox_hook_is_silent_without_a_hub_or_on_bad_input() {
    let dir = tmp("inbox-none");
    for stdin in ["", "not json", "{}"] {
        let (out, ok) = oc(&dir, &["statusline", "--hook", "inbox"], stdin);
        assert!(ok && out.is_empty(), "{stdin:?} -> {out:?}");
    }
    assert!(!dir.join("hub.db").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_footer_counts_results_waiting_without_consuming_them() {
    let dir = seeded("footer");
    let (line, _) = oc(&dir, &["statusline"], "{}");
    assert!(line.contains("2 results waiting"), "{line}");
    let (line2, _) = oc(&dir, &["statusline"], "{}");
    assert_eq!(line, line2, "reading the footer marks nothing");
    let _ = std::fs::remove_dir_all(&dir);
}
