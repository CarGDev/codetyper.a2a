//! End-to-end test of the hub: two separate hub processes ("tabs") sharing one database.
//! Run: `codetypera2a e2e-test` (also driven by `cargo test` through tests/e2e.rs).
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Client {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    n: u64,
}

impl Client {
    fn spawn(db: &str, agents: &str) -> Client {
        let exe = std::env::current_exe().expect("current_exe");
        let mut child = Command::new(exe)
            .arg("hub")
            .env("A2A_DB", db)
            .env("A2A_AGENT_DIR", agents)
            .env("A2A_NO_DISPATCHER", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn hub");
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        Client { child, stdin, out, n: 0 }
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.n += 1;
        let req = json!({ "jsonrpc": "2.0", "id": self.n, "method": method, "params": params });
        writeln!(self.stdin, "{req}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).expect("hub reply is JSON")
    }

    /// (is_error, structured result or error text)
    fn call(&mut self, name: &str, args: Value) -> (bool, Value) {
        let r = self.rpc("tools/call", json!({ "name": name, "arguments": args }))["result"].clone();
        let v = if r["structuredContent"].is_null() { r["content"][0]["text"].clone() } else { r["structuredContent"].clone() };
        (r["isError"].as_bool().unwrap_or(false), v)
    }

    fn close(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn run() -> i32 {
    let dir = std::env::temp_dir().join(format!("a2a-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let agents = dir.join("agent");
    std::fs::create_dir_all(&agents).unwrap();
    for n in ["debugger", "mentor", "qa-tester"] {
        std::fs::write(agents.join(format!("{n}.md")), format!("---\ndescription: {n} agent\nmode: primary\n---\nbody\n")).unwrap();
    }
    let db = dir.join("hub.db").display().to_string();
    let agents = agents.display().to_string();

    let mut fails = 0;
    let mut ok = |c: bool, m: &str| {
        println!("{} {m}", if c { "PASS" } else { "FAIL" });
        if !c {
            fails += 1;
        }
    };

    let mut a = Client::spawn(&db, &agents);
    let mut b = Client::spawn(&db, &agents); // two separate processes = two tabs

    let init = a.rpc("initialize", json!({ "protocolVersion": "2025-06-18" }));
    ok(init["result"]["serverInfo"]["name"] == "a2a-hub", "initialize");
    let tl = a.rpc("tools/list", json!({}));
    ok(tl["result"]["tools"].as_array().map(Vec::len) == Some(8), "tools/list = 8 tools");
    let agents_v = a.call("list_agents", json!({ "query": "debugger" })).1;
    ok(agents_v["agents"].as_array().is_some_and(|l| l.iter().any(|c| c["name"] == "debugger")), "agent cards auto-seeded");

    let t = a.call("send_message", json!({ "from": "brain", "to": "debugger", "text": "find the bug in X", "contextId": "ctx1" })).1;
    ok(t["status"]["state"] == "submitted" && t["contextId"] == "ctx1", "A creates task (submitted)");
    let id = t["id"].as_str().unwrap_or("").to_string();

    let inbox = b.call("inbox", json!({ "agent": "debugger" })).1;
    ok(inbox["count"] == 1 && inbox["tasks"][0]["id"] == id, "B (other process) sees it in inbox");
    ok(b.call("update_task", json!({ "id": id, "agent": "mentor", "state": "working" })).0, "non-assignee update rejected");
    let w = b.call("update_task", json!({ "id": id, "agent": "debugger", "state": "working", "text": "on it" })).1;
    ok(w["status"]["state"] == "working", "assignee claims -> working");
    let q = b.call("update_task", json!({ "id": id, "agent": "debugger", "state": "input-required", "text": "which env?" })).1;
    ok(q["status"]["state"] == "input-required", "assignee asks question");

    let rq = a.call("inbox", json!({ "agent": "brain", "as": "requester" })).1;
    ok(rq["count"] == 1 && rq["tasks"][0]["status"]["state"] == "input-required", "requester inbox shows question");
    ok(a.call("inbox", json!({ "agent": "brain", "as": "requester" })).1["count"] == 0, "requester inbox marks seen");

    let re = a.call("send_message", json!({ "from": "brain", "taskId": id, "text": "prod" })).1;
    ok(re["status"]["state"] == "submitted" && re["history"].as_array().map(Vec::len) == Some(4), "requester reply re-opens task");

    let fin = b.call("update_task", json!({
        "id": id, "agent": "debugger", "state": "completed", "text": "done",
        "artifact": { "name": "report", "text": "root cause: null deref at a.js:10" },
    })).1;
    ok(fin["status"]["state"] == "completed" && fin["artifacts"].as_array().map(Vec::len) == Some(1), "assignee completes with artifact");
    let rf = a.call("inbox", json!({ "agent": "brain", "as": "requester" })).1;
    ok(
        rf["count"] == 1 && rf["tasks"][0]["artifacts"][0]["parts"][0]["text"].as_str().is_some_and(|t| t.contains("null deref")),
        "requester receives artifact",
    );
    ok(b.call("update_task", json!({ "id": id, "agent": "debugger", "state": "working" })).0, "terminal task is immutable");
    ok(a.call("send_message", json!({ "from": "brain", "taskId": id, "text": "more" })).0, "reply to terminal task rejected");

    let t2 = a.call("send_message", json!({ "from": "brain", "to": "any", "text": "anyone", "referenceTaskIds": [id] })).1;
    ok(b.call("inbox", json!({ "agent": "qa-tester" })).1["count"] == 1, "'any' task visible to any agent");
    let t2id = t2["id"].as_str().unwrap_or("").to_string();
    ok(a.call("cancel_task", json!({ "id": t2id, "agent": "brain", "reason": "nvm" })).1["status"]["state"] == "canceled", "cancel works");
    ok(a.call("list_tasks", json!({ "contextId": "ctx1" })).1["count"] == 1, "list_tasks by contextId");
    ok(a.call("send_message", json!({ "from": "x" })).0, "validation error returned as isError");

    a.close();
    b.close();
    let _ = std::fs::remove_dir_all(&dir);
    println!("{}", if fails > 0 { format!("{fails} FAILED") } else { "ALL PASSED".to_string() });
    i32::from(fails > 0)
}
