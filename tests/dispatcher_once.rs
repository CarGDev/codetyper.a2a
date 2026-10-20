//! The dispatcher must deliver every nudge exactly once, even when two of them run (a pidfile race
//! used to start two, and every task was then sent twice and crashed the tick on the duplicate key).
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
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
        let mut child = self.cmd("hub", "unused.pid").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let req = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}});
        writeln!(child.stdin.as_mut().unwrap(), "{req}").unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        drop(child.stdin.take());
        let _ = child.wait();
        serde_json::from_str::<Value>(&line).unwrap()["result"]["structuredContent"].clone()
    }

    fn calls(&self, needle: &str) -> usize {
        std::fs::read_to_string(self.dir.join("calls.log")).unwrap_or_default().lines().filter(|l| l.contains(needle)).count()
    }
}

fn stop(mut c: Child) {
    let _ = c.kill();
    let _ = c.wait();
}

fn alive(c: &mut Child) -> bool {
    c.try_wait().unwrap().is_none()
}

#[test]
fn a_stampede_of_dispatchers_leaves_exactly_one() {
    let env = Env::new("stampede");
    let mut all: Vec<Child> = (0..8).map(|_| env.dispatcher("same.pid")).collect();
    std::thread::sleep(Duration::from_millis(1500));
    let running = all.iter_mut().filter_map(|c| alive(c).then_some(())).count();
    all.into_iter().for_each(stop);
    let _ = std::fs::remove_dir_all(&env.dir);
    assert_eq!(running, 1, "pidfile must allow exactly one dispatcher");
}

#[test]
fn two_dispatchers_still_deliver_each_nudge_once() {
    let env = Env::new("twice");
    // different pidfiles on purpose: both run against the same database
    let (a, b) = (env.dispatcher("a.pid"), env.dispatcher("b.pid"));
    std::thread::sleep(Duration::from_millis(400));

    let task = env.hub("send_message", json!({"from":"orchestrator","to":"mentor","text":"hi","directory":"/tmp/projx","sessionId":"orch1"}));
    let id = task["id"].as_str().unwrap().to_string();
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(env.calls("post /api/session -d"), 1, "the mentor session must be created once");
    assert_eq!(env.calls("post /api/session/ses_m1/prompt"), 1, "the assignee must be prompted once");

    env.hub("update_task", json!({"id": id, "agent":"mentor", "state":"completed", "text":"done"}));
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(env.calls("post /api/session/orch1/prompt"), 1, "the requester must be woken once");

    stop(a);
    stop(b);
    let _ = std::fs::remove_dir_all(Path::new(&env.dir));
}

#[test]
fn a_new_agent_session_gets_the_model_chosen_for_that_agent() {
    let env = Env::new("model");
    let d = env.dispatcher("m.pid");
    std::thread::sleep(Duration::from_millis(400));
    env.hub("send_message", json!({"from":"orchestrator","to":"mentor","text":"hi","directory":"/tmp/projx","sessionId":"orch1"}));
    std::thread::sleep(Duration::from_millis(1500));
    let log = std::fs::read_to_string(env.dir.join("calls.log")).unwrap_or_default();
    let create = log.lines().find(|l| l.contains("post /api/session -d")).expect("a session was created");
    assert!(
        create.contains("\"model\":{\"id\":\"claude-haiku-5.5\",\"providerID\":\"github-copilot\",\"variant\":\"high\"}"),
        "session create must carry the agent's model: {create}"
    );
    stop(d);
    let _ = std::fs::remove_dir_all(&env.dir);
}
