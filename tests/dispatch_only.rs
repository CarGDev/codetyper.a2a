//! `DISPATCH_ONLY` keeps one runtime in charge: new tasks route to it whatever the caller identity
//! or per-agent override says, so a Claude-only install never touches opencode (and vice versa).
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_codetypera2a");

fn routed_backend(name: &str, only: Option<&str>, runtime: &str) -> String {
    let dir = std::env::temp_dir().join(format!("a2a-only-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("agents")).unwrap();
    let mut cmd = Command::new(BIN);
    cmd.arg("hub")
        .env("A2A_DB", dir.join("hub.db"))
        .env("CODETYPERA2A_HOME", dir.join("home"))
        .env("HOME", dir.join("fakehome"))
        .env("A2A_AGENT_DIR", dir.join("agents"))
        .env("A2A_NO_DISPATCHER", "1")
        .env("A2A_RUNTIME", runtime)
        .env("DISPATCH_BACKEND_ORCHESTRATOR", "opencode")
        .env("OPENCODE_BIN", "/usr/bin/false")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("DISPATCH_ONLY");
    if let Some(only) = only {
        cmd.env("DISPATCH_ONLY", only);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let args =
        json!({"from":"orchestrator","to":"build","text":"x","contextId":"c1","sessionId":null});
    let req = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_message","arguments":args}});
    writeln!(child.stdin.take().unwrap(), "{req}").unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let _ = child.wait();
    let conn = rusqlite::Connection::open(dir.join("hub.db")).unwrap();
    let meta: String = conn
        .query_row("SELECT metadata FROM contexts WHERE id='c1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let v: serde_json::Value = serde_json::from_str(&meta).unwrap();
    v["routing"]["backend"].as_str().unwrap().to_string()
}

#[test]
fn dispatch_only_claude_routes_to_claude_even_against_an_opencode_override() {
    assert_eq!(
        routed_backend("claude", Some("claude"), "opencode"),
        "claude"
    );
}

#[test]
fn dispatch_only_opencode_routes_to_opencode() {
    assert_eq!(
        routed_backend("opencode", Some("opencode"), "claude"),
        "opencode"
    );
}

#[test]
fn without_dispatch_only_the_existing_precedence_applies() {
    assert_eq!(routed_backend("none", None, "claude"), "opencode"); // per-agent override wins
}
