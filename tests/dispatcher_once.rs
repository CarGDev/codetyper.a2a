//! The dispatcher must deliver every nudge exactly once, even when two of them run (a pidfile race
//! used to start two, and every task was then sent twice and crashed the tick on the duplicate key).
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_codetypera2a");

/// A child process that is killed when dropped, so a failing assertion never leaks a dispatcher.
struct Proc(Child);

impl std::ops::Deref for Proc {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for Proc {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(name: &str) -> Env {
        let dir = std::env::temp_dir().join(format!("a2a-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        // the dispatcher only uses sessions and tasks whose folder exists
        std::fs::create_dir_all("/tmp/projx").unwrap();
        std::fs::write(dir.join("agents/mentor.md"), "---\ndescription: m\n---\n").unwrap();
        // a fake `opencode`: logs every call, answers the few routes the dispatcher uses
        let stub = dir.join("opencode");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\necho \"$@\" >> {log}\ncase \"$2 $3\" in\n  \"get /api/session/active\") echo '{{\"data\":{{}}}}';;\n  \"get /api/session/orch1\") echo '{{\"data\":{{\"id\":\"orch1\",\"agent\":\"orchestrator\",\"location\":{{\"directory\":\"/tmp/projx\"}},\"time\":{{\"created\":1,\"updated\":2}}}}}}';;\n  \"get /api/session/ses_m1\") if [ -f {dir}/ses_m1.json ]; then cat {dir}/ses_m1.json; else echo '{{\"data\":{{\"id\":\"ses_m1\",\"agent\":\"mentor\",\"location\":{{\"directory\":\"/tmp/projx\"}},\"time\":{{\"created\":1,\"updated\":2}}}}}}'; fi;;\n  \"get /api/agent\"*) echo '{{\"data\":[{{\"id\":\"mentor\",\"description\":\"m\",\"mode\":\"primary\",\"hidden\":false,\"model\":{{\"id\":\"claude-haiku-5.5\",\"providerID\":\"github-copilot\",\"variant\":\"high\"}}}}]}}';;\n  \"get /api/session/ses_m1/message\") if [ -f {dir}/ses_m1_messages.json ]; then cat {dir}/ses_m1_messages.json; else echo '{{\"data\":[]}}'; fi;;\n  \"post /api/session/\"*\"/prompt\") if [ -f {dir}/fail_prompt ]; then echo \"HTTP 404 Not Found\" >&2; exit 1; fi;;\n  \"get /api/session/ses_c\"*) echo \"{{\\\"data\\\":{{\\\"id\\\":\\\"$3\\\",\\\"agent\\\":\\\"mentor\\\",\\\"location\\\":{{\\\"directory\\\":\\\"/tmp/projx\\\"}},\\\"time\\\":{{\\\"created\\\":1,\\\"updated\\\":2}}}}}}\" | sed \"s#/api/session/##\";;\n  \"post /api/session\") n=$(cat {dir}/cnt 2>/dev/null || echo 0); n=$((n+1)); echo $n > {dir}/cnt; if [ -f {dir}/fresh_ids ]; then id=ses_c$n; else id=ses_m1; fi; echo \"{{\\\"data\\\":{{\\\"id\\\":\\\"$id\\\"}}}}\";;\nesac\n",
                log = dir.join("calls.log").display(),
                dir = dir.display()
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
            .env("DISPATCH_RETRY_BASE_MS", "50")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        c
    }

    fn dispatcher(&self, pidfile: &str) -> Proc {
        Proc(self.cmd("dispatcher", pidfile).spawn().unwrap())
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

fn stop(c: Proc) {
    drop(c);
}

fn alive(c: &mut Proc) -> bool {
    c.try_wait().unwrap().is_none()
}

#[test]
fn a_stampede_of_dispatchers_leaves_exactly_one() {
    let env = Env::new("stampede");
    let mut all: Vec<Proc> = (0..8).map(|_| env.dispatcher("same.pid")).collect();
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

fn session_json(input: i64, output: i64, cache_read: i64, cost: f64) -> String {
    json!({"data":{"id":"ses_m1","agent":"mentor","location":{"directory":"/tmp/projx"},"time":{"created":1,"updated":2},
        "model":{"id":"claude-haiku-5.5","providerID":"github-copilot"},"cost":cost,
        "tokens":{"input":input,"output":output,"reasoning":0,"cache":{"read":cache_read,"write":0}}}})
    .to_string()
}

fn events(env: &Env, sql: &str) -> Vec<(Option<i64>, Option<i64>, Option<f64>)> {
    let c = rusqlite::Connection::open(env.dir.join("hub.db")).unwrap();
    let mut st = c.prepare(sql).unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().flatten().collect()
}

#[test]
fn what_an_agent_spends_on_a_task_is_recorded_and_priced() {
    let env = Env::new("usage");
    // the mentor session already spent tokens on earlier work: only the delta belongs to this task
    std::fs::write(env.dir.join("ses_m1.json"), session_json(10_000, 500, 0, 0.0)).unwrap();
    let d = env.dispatcher("u.pid");
    let task = env.hub("send_message", json!({"from":"orchestrator","to":"mentor","text":"hi","directory":"/tmp/projx","sessionId":"orch1"}));
    let id = task["id"].as_str().unwrap().to_string();
    let base = "SELECT tokens_in, tokens_out, cost_usd FROM events WHERE kind = 'usage_baseline'";
    for _ in 0..50 {
        if !events(&env, base).is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(events(&env, base).len(), 1, "a baseline is taken when the task is handed over");
    // the mentor works, then reports
    std::fs::write(env.dir.join("ses_m1.json"), session_json(1_010_000, 1_000_500, 0, 0.0)).unwrap();
    env.hub("update_task", json!({"id": id, "agent":"mentor", "state":"completed", "text":"done"}));
    let sql = "SELECT tokens_in, tokens_out, cost_usd FROM events WHERE kind = 'usage'";
    for _ in 0..50 {
        if !events(&env, sql).is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    stop(d);
    let got = events(&env, sql);
    assert_eq!(got.len(), 1, "{got:?}");
    let (tin, tout, cost) = got[0];
    assert_eq!((tin, tout), (Some(1_000_000), Some(1_000_000)), "only the delta since the baseline");
    // claude-haiku-5.5 is $0.10 in / $0.50 out per 1M tokens and opencode charged 0: estimated
    assert!((cost.unwrap() - 0.60).abs() < 1e-6, "{cost:?}");
    let _ = std::fs::remove_dir_all(&env.dir);
}

fn rows(env: &Env, sql: &str) -> Vec<String> {
    let c = rusqlite::Connection::open(env.dir.join("hub.db")).unwrap();
    let mut st = c.prepare(sql).unwrap();
    st.query_map([], |r| r.get::<_, String>(0)).unwrap().flatten().collect()
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..150 {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for: {what}");
}

#[test]
fn a_session_whose_folder_is_gone_is_retired_and_replaced() {
    let env = Env::new("moved");
    // the mentor's registered session lives in a folder that no longer exists (project moved)
    env.hub("register_agent", json!({"name":"mentor","sessionId":"ses_m1","directory":"/tmp/projx"}));
    let gone = json!({"data":{"id":"ses_m1","agent":"mentor","location":{"directory":"/definitely/not/here"},"time":{"created":1,"updated":2}}});
    std::fs::write(env.dir.join("ses_m1.json"), gone.to_string()).unwrap();
    std::fs::write(env.dir.join("fresh_ids"), "").unwrap();
    let _d = env.dispatcher("m.pid");
    env.hub("send_message", json!({"from":"orchestrator","to":"mentor","text":"hi","directory":"/tmp/projx","sessionId":"orch1"}));
    wait_for("the prompt reaches a new session", || env.calls("post /api/session/ses_c1/prompt") == 1);
    assert_eq!(env.calls("post /api/session -d"), 1, "exactly one new session");
    assert_eq!(env.calls("post /api/session/ses_m1/prompt"), 0, "nothing is sent to the dead session");
    assert_eq!(rows(&env, "SELECT status FROM sessions WHERE session_id = 'ses_m1'"), vec!["stale"]);
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn delivery_that_keeps_failing_gives_up_and_tells_the_requester() {
    let env = Env::new("always404");
    std::fs::write(env.dir.join("fail_prompt"), "").unwrap();
    std::fs::write(env.dir.join("fresh_ids"), "").unwrap();
    let _d = env.dispatcher("f.pid");
    let task = env.hub("send_message", json!({"from":"orchestrator","to":"mentor","text":"hi","directory":"/tmp/projx","sessionId":"orch1"}));
    let id = task["id"].as_str().unwrap().to_string();
    wait_for("the nudge is given up", || {
        !rows(&env, "SELECT 'x' FROM nudges WHERE kind = 'assignee' AND status = 'dead'").is_empty()
    });
    // the requester is told about the failure too (and that wake-up fails the same way), so only
    // the mentor's sessions are counted: one retired session, then a fresh one tried 4 more times
    let to_mentor = |e: &Env| e.calls("/api/session/ses_c1/prompt") + e.calls("/api/session/ses_c2/prompt");
    assert_eq!(to_mentor(&env), 5, "five attempts, then it stops");
    assert_eq!(env.calls("post /api/session -d"), 2, "one replacement session, not one per attempt");
    let state = rows(&env, &format!("SELECT state FROM tasks WHERE id = '{id}'"));
    assert_eq!(state, vec!["input-required"]);
    let msg = rows(&env, &format!("SELECT status_message FROM tasks WHERE id = '{id}'"));
    assert!(msg[0].contains("delivery_failed"), "{}", msg[0]);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(to_mentor(&env), 5, "no further attempts after giving up");
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_task_for_a_folder_that_does_not_exist_fails_at_once() {
    let env = Env::new("nodir");
    let _d = env.dispatcher("n.pid");
    let task = env.hub("send_message", json!({"from":"orchestrator","to":"mentor","text":"hi","directory":"/no/such/project","sessionId":"orch1"}));
    let id = task["id"].as_str().unwrap().to_string();
    wait_for("the task fails", || rows(&env, &format!("SELECT state FROM tasks WHERE id = '{id}'")) == vec!["failed"]);
    let msg = rows(&env, &format!("SELECT status_message FROM tasks WHERE id = '{id}'"));
    assert!(msg[0].contains("directory_missing"), "{}", msg[0]);
    assert_eq!(env.calls("post /api/session -d"), 0, "no session is opened in a missing folder");
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn the_dispatcher_never_opens_an_orchestrator_session() {
    let env = Env::new("noorch");
    std::fs::write(env.dir.join("agents/orchestrator.md"), "---\ndescription: o\n---\n").unwrap();
    let task = env.hub("send_message", json!({"from":"orchestrator","to":"mentor","text":"hi","directory":"/tmp/projx","sessionId":"orch1"}));
    let id = task["id"].as_str().unwrap().to_string();
    // a task that reached the orchestrator some other way (older hub, manual edit)
    let c = rusqlite::Connection::open(env.dir.join("hub.db")).unwrap();
    c.execute("INSERT OR IGNORE INTO agents (name, card, created_at, updated_at) VALUES ('orchestrator','{}','t','t'), ('brain','{}','t','t')", []).unwrap();
    c.execute("UPDATE tasks SET from_agent='brain', to_agent='orchestrator', from_session=NULL WHERE id = ?", [&id]).unwrap();
    drop(c);
    let _d = env.dispatcher("o.pid");
    wait_for("the task is told there is no orchestrator", || {
        rows(&env, &format!("SELECT state FROM tasks WHERE id = '{id}'")) == vec!["input-required"]
    });
    assert_eq!(env.calls("post /api/session -d"), 0, "no orchestrator session is opened");
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_task_fails_at_once_when_the_agents_turn_failed() {
    let env = Env::new("agenterr");
    let _d = env.dispatcher("e.pid");
    let task = env.hub("send_message", json!({"from":"orchestrator","to":"mentor","text":"hi","directory":"/tmp/projx","sessionId":"orch1"}));
    let id = task["id"].as_str().unwrap().to_string();
    wait_for("the task is handed over", || !rows(&env, "SELECT 'x' FROM nudges WHERE kind = 'assignee' AND status = 'delivered'").is_empty());
    // the mentor's session now reports a failed turn (e.g. provider 403)
    let failed = json!({"data":{"id":"ses_m1","agent":"mentor","location":{"directory":"/tmp/projx"},"outcome":"failed",
        "time":{"created":1,"updated": 99_999_999_999_999_i64}}});
    std::fs::write(env.dir.join("ses_m1.json"), failed.to_string()).unwrap();
    std::fs::write(env.dir.join("ses_m1_messages.json"), r#"{"data":[{"type":"assistant","error":{"message":"HTTP 403: not licensed"}}]}"#).unwrap();
    wait_for("the task fails with the agent's error", || rows(&env, &format!("SELECT state FROM tasks WHERE id = '{id}'")) == vec!["failed"]);
    let msg = rows(&env, &format!("SELECT status_message FROM tasks WHERE id = '{id}'"));
    assert!(msg[0].contains("agent_error") && msg[0].contains("HTTP 403"), "{}", msg[0]);
    let _ = std::fs::remove_dir_all(&env.dir);
}
