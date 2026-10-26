//! A Claude session in tmux is registered by its start hook so results find it, and a Claude caller
//! never reaches for opencode.
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_codetypera2a");

fn sandbox(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("a2a-wake-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("agents")).unwrap();
    dir
}

fn base<'a>(dir: &std::path::Path, cmd: &'a mut Command) -> &'a mut Command {
    cmd.env("A2A_DB", dir.join("hub.db"))
        .env("CODETYPERA2A_HOME", dir.join("home"))
        .env("HOME", dir.join("fakehome"))
        .env("A2A_AGENT_DIR", dir.join("agents"))
        .env("A2A_CLAUDE_RUNS_DIR", dir.join("runs"))
        .env("A2A_DISPATCHER_PID", dir.join("d.pid"))
        .env("A2A_STATUSLINE_DIR", dir.join("sub"))
        .env("A2A_NO_DISPATCHER", "1")
        .env_remove("A2A_HEADLESS")
        .env_remove("TMUX")
}

#[test]
fn the_start_hook_registers_the_interactive_session_and_its_pane() {
    let dir = sandbox("hook");
    let mut cmd = Command::new(BIN);
    base(&dir, &mut cmd)
        .args(["oc", "statusline", "--hook", "ensure"])
        .env("TMUX_PANE", "%7")
        .stdin(Stdio::piped())
        .stdout(Stdio::null());
    let mut child = cmd.spawn().unwrap();
    let cwd = dir.display().to_string();
    let input = json!({"session_id": "sess-1", "cwd": cwd}).to_string();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());

    assert_eq!(
        std::fs::read_to_string(dir.join("runs/sess-1.pane")).unwrap(),
        "%7"
    );
    let conn = rusqlite::Connection::open(dir.join("hub.db")).unwrap();
    let row: (String, String, String, String) = conn
        .query_row(
            "SELECT agent, backend, tmux_target, status FROM sessions WHERE session_id='sess-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "orchestrator".into(),
            "claude".into(),
            "%7".into(),
            "live".into()
        )
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_headless_run_and_a_missing_pane_register_nothing() {
    let dir = sandbox("none");
    for (envs, name) in [
        (vec![("A2A_HEADLESS", "1"), ("TMUX_PANE", "%7")], "headless"),
        (vec![], "nopane"),
    ] {
        let mut cmd = Command::new(BIN);
        base(&dir, &mut cmd)
            .args(["oc", "statusline", "--hook", "ensure"])
            .env_remove("TMUX_PANE")
            .stdin(Stdio::piped())
            .stdout(Stdio::null());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"session_id":"s2","cwd":"/x"}"#)
            .unwrap();
        assert!(child.wait().unwrap().success(), "{name}");
    }
    assert!(!dir.join("runs/s2.pane").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn list_agents_for_a_claude_caller_never_runs_opencode() {
    let dir = sandbox("list");
    let marker = dir.join("opencode-was-run");
    let fake = dir.join("fake-opencode.sh");
    std::fs::write(
        &fake,
        format!("#!/bin/sh\ntouch '{}'\necho '[]'\n", marker.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let call = |runtime: &str| {
        let mut cmd = Command::new(BIN);
        base(&dir, &mut cmd)
            .arg("hub")
            .env("A2A_RUNTIME", runtime)
            .env("OPENCODE_BIN", &fake)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().unwrap();
        let req = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_agents","arguments":{}}});
        writeln!(child.stdin.take().unwrap(), "{req}").unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let _ = child.wait();
    };
    call("claude");
    assert!(!marker.exists(), "a Claude caller must not run opencode");
    call("opencode");
    assert!(marker.exists(), "an opencode caller still does");
    let _ = std::fs::remove_dir_all(dir);
}
