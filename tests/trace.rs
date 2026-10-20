//! `oc trace` / `oc stats` over a task run through the real hub and dispatcher with a fake opencode.
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_codetypera2a");

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(name: &str) -> Env {
        let dir = std::env::temp_dir().join(format!("a2a-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        std::fs::write(dir.join("agents/mentor.md"), "---\ndescription: m\n---\n").unwrap();
        // a fake `opencode`: logs every call, answers the few routes the dispatcher uses
        let stub = dir.join("opencode");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\necho \"$@\" >> {log}\ncase \"$2 $3\" in\n  \"get /api/session/active\") echo '{{\"data\":{{}}}}';;\n  \"get /api/session/orch1\") echo '{{\"data\":{{\"id\":\"orch1\",\"agent\":\"orchestrator\",\"location\":{{\"directory\":\"/tmp/projx\"}},\"time\":{{\"created\":1,\"updated\":2}}}}}}';;\n  \"get /api/session/ses_m1\") echo '{{\"data\":{{\"id\":\"ses_m1\",\"agent\":\"mentor\",\"location\":{{\"directory\":\"/tmp/projx\"}},\"time\":{{\"created\":1,\"updated\":2}}}}}}';;\n  \"get /api/agent\"*) echo '{{\"data\":[{{\"id\":\"mentor\",\"description\":\"m\",\"mode\":\"primary\",\"hidden\":false,\"model\":{{\"id\":\"claude-haiku-5.5\",\"providerID\":\"github-copilot\",\"variant\":\"high\"}}}}]}}';;\n  \"post /api/session\") echo '{{\"data\":{{\"id\":\"ses_m1\"}}}}';;\nesac\n",
                log = dir.join("calls.log").display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        Env { dir }
    }

    fn cmd(&self, arg: &str, pidfile: &str) -> Command {
        let mut c = Command::new(BIN);
        c.arg(arg)
            .env("A2A_DB", self.dir.join("hub.db"))
            .env("A2A_AGENT_DIR", self.dir.join("agents"))
            .env("A2A_DISPATCHER_PID", self.dir.join(pidfile))
            .env("OPENCODE_BIN", self.dir.join("opencode"))
            .env("A2A_NO_DISPATCHER", "1")
            .env("DISPATCH_POLL_MS", "100")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        c
    }

    fn dispatcher(&self, pidfile: &str) -> Child {
        self.cmd("dispatcher", pidfile).spawn().unwrap()
    }

    /// One MCP tool call against a short-lived hub process.
    fn hub(&self, tool: &str, args: Value) -> Value {
        let mut child = self
            .cmd("hub", "unused.pid")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let req = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}});
        writeln!(child.stdin.as_mut().unwrap(), "{req}").unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        drop(child.stdin.take());
        let _ = child.wait();
        serde_json::from_str::<Value>(&line).unwrap()["result"]["structuredContent"].clone()
    }
}

impl Env {
    fn oc(&self, args: &[&str]) -> (String, bool) {
        let o = Command::new(BIN)
            .arg("oc")
            .args(args)
            .env("A2A_DB", self.dir.join("hub.db"))
            .env("OPENCODE_BIN", "/usr/bin/false")
            .env_remove("TMUX")
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr),
            o.status.success(),
        )
    }
}

#[test]
fn trace_lists_the_whole_life_of_a_task() {
    let env = Env::new("trace");
    let mut d = env.dispatcher("t.pid");
    std::thread::sleep(Duration::from_millis(400));
    let task = env.hub("send_message", json!({"from":"orchestrator","to":"mentor","text":"hi","directory":"/tmp/projx","sessionId":"orch1"}));
    let id = task["id"].as_str().unwrap().to_string();
    std::thread::sleep(Duration::from_millis(1500));
    env.hub(
        "update_task",
        json!({"id": id, "agent":"mentor", "state":"completed", "text":"done"}),
    );
    std::thread::sleep(Duration::from_millis(1500));
    let _ = d.kill();
    let _ = d.wait();

    let (out, ok) = env.oc(&["trace", &id[..8]]);
    assert!(ok, "{out}");
    for k in [
        "task_created",
        "nudge_claimed",
        "nudge_delivered",
        "state_change",
        "orchestrator -> mentor",
        "total duration",
    ] {
        assert!(out.contains(k), "missing {k}: {out}");
    }
    let (out, ok) = env.oc(&["trace", "zzzzzzzz"]);
    assert!(!ok && out.contains("no task matches"), "{out}");
    let (out, ok) = env.oc(&["stats"]);
    assert!(ok && out.contains("mentor"), "{out}");
    let _ = std::fs::remove_dir_all(&env.dir);
}
