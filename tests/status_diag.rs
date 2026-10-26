//! `oc status --json` carries an additive, read-only `diagnostics` block and leaves state alone.
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_codetypera2a");

fn base(dir: &std::path::Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .env("A2A_DB", dir.join("hub.db"))
        .env("CODETYPERA2A_HOME", dir.join("home"))
        .env("HOME", dir.join("fakehome"))
        .env("A2A_AGENT_DIR", dir.join("agents"))
        .env("A2A_DISPATCHER_PID", dir.join("d.pid"))
        .env("A2A_NO_DISPATCHER", "1")
        .env("OPENCODE_BIN", "/usr/bin/false")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    cmd
}

fn hub_call(dir: &std::path::Path, tool: &str, args: Value) {
    let mut child = base(dir, &["hub"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let req = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}});
    writeln!(child.stdin.take().unwrap(), "{req}").unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let _ = child.wait();
}

#[test]
fn status_json_adds_diagnostics_without_touching_existing_keys_or_state() {
    let dir = std::env::temp_dir().join(format!("a2a-status-diag-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("agents")).unwrap();
    std::fs::create_dir_all(dir.join("home")).unwrap();
    hub_call(
        &dir,
        "send_message",
        json!({"from":"orchestrator","to":"build","text":"x"}),
    );
    let before = std::fs::read(dir.join("hub.db")).unwrap();
    let out = base(&dir, &["oc", "status", "--json"]).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    for key in ["dispatcher", "tasks", "progress", "today"] {
        assert!(v.get(key).is_some(), "existing key {key} missing: {v}");
    }
    let d = &v["diagnostics"];
    assert_eq!(d["owner"], "canonical");
    assert_eq!(d["guard_enforced"], false);
    assert_eq!(d["open"]["submitted"], 1);
    assert!(d["oldest_open_age_secs"].as_i64().unwrap() >= 0);
    assert_eq!(d["last_24h"]["stall_retry"], 0);
    assert!(
        !dir.join("d.pid").exists(),
        "status must not start a dispatcher"
    );
    assert_eq!(
        before.len(),
        std::fs::read(dir.join("hub.db")).unwrap().len(),
        "status must not write"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
