use std::process::Command;

fn run(args: &[&str], dir: &std::path::Path) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_codetypera2a"))
        .args(args)
        .env("A2A_DB", dir.join("hub.db"))
        .env("A2A_DISPATCHER_PID", dir.join("d.pid"))
        // never reach a real tmux server or opencode from a test
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("OPENCODE_BIN", "/usr/bin/false")
        .output()
        .expect("run");
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.success())
}

#[test]
fn oc_help_is_oc_help_and_has_no_side_effects() {
    let dir = std::env::temp_dir().join(format!("a2a-oc-help-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for flag in ["--help", "-h", "help"] {
        let (text, ok) = run(&["oc", flag], &dir);
        assert!(ok, "oc {flag} failed");
        assert!(text.contains("oc - run opencode with the A2A dispatcher"), "oc {flag}: {text}");
        assert!(text.contains("oc status"), "{text}");
        assert!(text.contains("oc trace") && text.contains("oc stats"), "{text}");
        assert!(!text.contains("OpenCode command line interface"), "must not be opencode's help: {text}");
    }
    // help started nothing: no dispatcher pidfile, no database
    assert!(!dir.join("d.pid").exists(), "oc --help started a dispatcher");
    assert!(!dir.join("hub.db").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn oc_status_shows_context_and_flags_hop_capped_contexts() {
    let dir = std::env::temp_dir().join(format!("a2a-oc-status-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // first run creates the schema
    let _ = run(&["oc", "status"], &dir);
    let db = rusqlite::Connection::open(dir.join("hub.db")).unwrap();
    db.execute(
        "INSERT INTO tasks (id, context_id, from_agent, to_agent, state, history, artifacts, created_at, updated_at)
         VALUES ('abcdef1234567890', 'ctx-x', 'orchestrator', 'build', 'input-required', '[]', '[]', 'now', 'now')",
        [],
    )
    .unwrap();
    let (text, ok) = run(&["oc", "status"], &dir);
    assert!(ok, "{text}");
    assert!(text.contains("ctx=ctx-x") && !text.contains("HOP-CAPPED"), "{text}");
    for i in 0..40 {
        db.execute(
            "INSERT INTO nudges (key, task_id, context_id, target, kind, created_at) VALUES (?, 'abcdef1234567890', 'ctx-x', 't', 'assignee', 'now')",
            [format!("k{i}")],
        )
        .unwrap();
    }
    let (text, _) = run(&["oc", "status"], &dir);
    assert!(text.contains("HOP-CAPPED (run: oc continue ctx-x)"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn oc_open_project_outside_tmux_fails_cleanly() {
    let dir = std::env::temp_dir().join(format!("a2a-oc-open-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (_, ok) = run(&["oc", "open-project", dir.to_str().unwrap()], &dir);
    assert!(!ok);
    let (_, ok) = run(&["oc", "open-project"], &dir);
    assert!(!ok);
    let _ = std::fs::remove_dir_all(&dir);
}
