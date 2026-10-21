//! Several agents racing for one task: exactly one claims it, the rest are refused, and nobody can
//! work on a task that is unclaimed or owned by someone else.
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_codetypera2a");

struct Env(PathBuf);

impl Env {
    fn new(name: &str) -> Env {
        let dir = std::env::temp_dir().join(format!("a2a-claim-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        Env(dir)
    }

    /// One MCP tool call against a short-lived hub process: Ok(task/result json) or Err(message).
    fn hub(&self, tool: &str, args: Value) -> Result<Value, String> {
        let mut child = Command::new(BIN)
            .arg("hub")
            .env("A2A_DB", self.0.join("hub.db"))
            .env("A2A_AGENT_DIR", self.0.join("agents"))
            .env("A2A_NO_DISPATCHER", "1")
            .env("OPENCODE_BIN", "/usr/bin/false")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let req = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}});
        writeln!(child.stdin.take().unwrap(), "{req}").unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let _ = child.wait();
        let v: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad reply {line:?}: {e}"));
        let text = v["result"]["content"][0]["text"].as_str().unwrap_or("").to_string();
        if v["result"]["isError"] == json!(true) || v.get("error").is_some() {
            return Err(if text.is_empty() { v.to_string() } else { text });
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    fn send_any(&self) -> String {
        let t = self.hub("send_message", json!({"from":"orchestrator","to":"any","text":"do it","directory":"/tmp/p"})).unwrap();
        t["id"].as_str().unwrap().to_string()
    }
}

#[test]
fn exactly_one_of_many_concurrent_claims_wins() {
    let env = std::sync::Arc::new(Env::new("race"));
    let id = env.send_any();
    let handles: Vec<_> = (0..6)
        .map(|i| {
            let (env, id) = (env.clone(), id.clone());
            std::thread::spawn(move || env.hub("claim_task", json!({"id": id, "agent": format!("agent-{i}")})))
        })
        .collect();
    let results: Vec<Result<Value, String>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let winners: Vec<_> = results.iter().filter(|r| r.is_ok()).collect();
    assert_eq!(winners.len(), 1, "{results:?}");
    for r in results.iter().filter(|r| r.is_err()) {
        assert!(r.as_ref().unwrap_err().contains("already claimed"), "{r:?}");
    }
    let owner = winners[0].as_ref().unwrap()["to"].as_str().unwrap().to_string();
    let t = env.hub("get_task", json!({"id": id})).unwrap();
    assert_eq!(t["to"], owner);
    assert_eq!(t["status"]["state"], "working");
    let _ = std::fs::remove_dir_all(&env.0);
}

#[test]
fn nobody_works_on_an_unclaimed_or_foreign_task() {
    let env = Env::new("guards");
    let id = env.send_any();
    // unclaimed 'any' task: update refused
    let err = env.hub("update_task", json!({"id": id, "agent": "a", "state": "completed", "text": "x"})).unwrap_err();
    assert!(err.contains("claim_task first"), "{err}");
    env.hub("claim_task", json!({"id": id, "agent": "a"})).unwrap();
    // the loser cannot update either
    let err = env.hub("update_task", json!({"id": id, "agent": "b", "state": "completed", "text": "x"})).unwrap_err();
    assert!(err.contains("only the assignee"), "{err}");
    // the claimed task disappears from the loser's inbox and stays in the owner's
    assert_eq!(env.hub("inbox", json!({"agent": "b"})).unwrap()["count"], 0);
    assert_eq!(env.hub("inbox", json!({"agent": "a"})).unwrap()["count"], 1);
    // compare-and-swap
    let v = env.hub("get_task", json!({"id": id})).unwrap();
    let err = env.hub("update_task", json!({"id": id, "agent": "a", "text": "p", "expectedVersion": 99})).unwrap_err();
    assert!(err.contains("version conflict"), "{err} {v}");
    env.hub("update_task", json!({"id": id, "agent": "a", "state": "completed", "text": "done"})).unwrap();
    let _ = std::fs::remove_dir_all(&env.0);
}

#[test]
fn a_task_sent_by_name_can_only_be_claimed_by_that_agent() {
    let env = Env::new("named");
    let t = env.hub("send_message", json!({"from":"orchestrator","to":"mentor","text":"hi","directory":"/tmp/p"})).unwrap();
    let id = t["id"].as_str().unwrap();
    let err = env.hub("claim_task", json!({"id": id, "agent": "intruder"})).unwrap_err();
    assert!(err.contains("addressed to 'mentor'"), "{err}");
    env.hub("claim_task", json!({"id": id, "agent": "mentor"})).unwrap();
    let _ = std::fs::remove_dir_all(&env.0);
}

#[test]
fn nobody_sends_a_task_to_itself_or_to_the_orchestrator() {
    let env = Env::new("selfsend");
    let err = env.hub("send_message", json!({"from":"mentor","to":"mentor","text":"x","directory":"/tmp/p"})).unwrap_err();
    assert!(err.contains("cannot send a task to itself"), "{err}");
    let err = env.hub("send_message", json!({"from":"brain","to":"orchestrator","text":"x","directory":"/tmp/p"})).unwrap_err();
    assert!(err.contains("orchestrator"), "{err}");
    // normal delegation still works
    env.hub("send_message", json!({"from":"orchestrator","to":"mentor","text":"x","directory":"/tmp/p"})).unwrap();
    let _ = std::fs::remove_dir_all(&env.0);
}
