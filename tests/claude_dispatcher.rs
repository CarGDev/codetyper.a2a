//! The dispatcher with `DISPATCH_BACKEND=claude`: a fake `claude` script stands in for the real one
//! (its model calls cannot run in CI), records its arguments and answers through the hub like an agent.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_codetypera2a");

struct Proc(Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Env {
    dir: PathBuf,
    project: PathBuf,
}

impl Env {
    fn new(name: &str) -> Env {
        let dir = std::env::temp_dir().join(format!("a2a-claude-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = dir.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(dir.join("claude/agents")).unwrap();
        std::fs::write(
            dir.join("claude/agents/mentor.md"),
            "---\nname: mentor\ndescription: m\ntools: Read, Bash, mcp__engram__*\n---\nbody\n",
        )
        .unwrap();
        // never reached by the claude backend, but the hub asks it for the agent list
        let opencode = dir.join("opencode");
        std::fs::write(&opencode, "#!/bin/sh\necho '{\"data\":[]}'\n").unwrap();
        // the fake claude: logs its arguments (and folder), then behaves as `mode` says
        let fake = dir.join("claude-bin");
        let hub_call = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"update_task","arguments":{"id":"'"$id"'","agent":"mentor","state":"completed","text":"done"}}}"#;
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\necho \"$*\" >> {dir}/argv.log\necho \"${{ANTHROPIC_API_KEY:-none}}\" >> {dir}/key.log\npwd >> {dir}/cwd.log\nmode=$(cat {dir}/mode 2>/dev/null || echo complete)\n\
                 id=$(echo \"$2\" | sed -n 's/.*Task \\([0-9a-f-]*\\) is waiting.*/\\1/p')\n\
                 case \"$mode\" in\n  fail) echo boom >&2; exit 3;;\n  slow) echo $$ > {dir}/slow.pid; sleep 60; exit 0;;\n\
                 esac\n\
                 echo '{hub_call}' | {bin} hub >/dev/null\n\
                 echo '{{\"type\":\"result\",\"is_error\":false,\"result\":\"ok\",\"session_id\":\"fake\"}}'\n",
                dir = dir.display(),
                bin = BIN
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        for f in [&opencode, &fake] {
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Env { dir, project }
    }

    fn cmd(&self, arg: &str) -> Command {
        let mut c = Command::new(BIN);
        c.arg(arg)
            .env("A2A_DB", self.dir.join("hub.db"))
            .env("A2A_DISPATCHER_PID", self.dir.join("d.pid"))
            .env("OPENCODE_BIN", self.dir.join("opencode"))
            .env("CLAUDE_BIN", self.dir.join("claude-bin"))
            .env("CLAUDE_CONFIG_DIR", self.dir.join("claude"))
            // The MCP child is target-owned; ordinary request fields are not identity.
            .env("A2A_RUNTIME", "claude")
            .env("DISPATCH_BACKEND", "claude")
            .env("A2A_NO_DISPATCHER", "1")
            .env("DISPATCH_POLL_MS", "100")
            .env("DISPATCH_RETRY_BASE_MS", "50")
            .env_remove("TMUX")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("TMUX_PANE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        c
    }

    fn dispatcher(&self) -> Proc {
        Proc(self.cmd("dispatcher").spawn().unwrap())
    }

    /// The dispatcher started with an `ANTHROPIC_API_KEY` in its environment and the given auth mode.
    fn dispatcher_with_key(&self, auth: &str) -> Proc {
        let mut cmd = self.cmd("dispatcher");
        cmd.env("ANTHROPIC_API_KEY", "test-key-not-real").env("A2A_CLAUDE_AUTH", auth);
        Proc(cmd.spawn().unwrap())
    }

    fn keys_seen_by_claude(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("key.log"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn hub(&self, tool: &str, args: Value) -> Value {
        let mut child = self
            .cmd("hub")
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

    fn hub_meta(
        &self,
        tool: &str,
        args: Value,
        meta: Value,
        backend: Option<&str>,
        per_agent: Option<&str>,
    ) -> Value {
        let mut cmd = self.cmd("hub");
        match backend {
            Some(value) => {
                cmd.env("DISPATCH_BACKEND", value);
            }
            None => {
                cmd.env_remove("DISPATCH_BACKEND");
                // Exercise the safe fallback separately from the marker-attributed path.
                cmd.env_remove("A2A_RUNTIME");
            }
        }
        if let Some(value) = per_agent {
            cmd.env("DISPATCH_BACKEND_ORCHESTRATOR", value);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let req = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args,"_meta":meta}});
        writeln!(child.stdin.as_mut().unwrap(), "{req}").unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        drop(child.stdin.take());
        let _ = child.wait();
        serde_json::from_str::<Value>(&line).unwrap()["result"]["structuredContent"].clone()
    }

    fn send(&self) -> String {
        let dir = self.project.display().to_string();
        self.hub(
            "send_message",
            json!({"from":"orchestrator","to":"mentor","text":"hi","directory":dir}),
        )["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn rows(&self, sql: &str) -> Vec<String> {
        let c = rusqlite::Connection::open(self.dir.join("hub.db")).unwrap();
        let mut st = c.prepare(sql).unwrap();
        st.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect()
    }

    fn state(&self, id: &str) -> Vec<String> {
        self.rows(&format!("SELECT state FROM tasks WHERE id = '{id}'"))
    }

    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("argv.log"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }
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
fn a_task_wakes_the_agent_once_resumes_its_session_and_completes() {
    let env = Env::new("wake");
    let _d = env.dispatcher();
    let first = env.send();
    wait_for("the fake agent completes the task", || {
        env.state(&first) == vec!["completed"]
    });
    let argv = env.argv();
    assert_eq!(argv.len(), 1, "{argv:?}");
    let line = &argv[0];
    assert!(
        line.starts_with("-p A2A: you are agent \"mentor\"") && line.contains("mcp__a2a__inbox"),
        "{line}"
    );
    assert!(
        line.contains("--output-format stream-json --verbose --agent mentor --session-id "),
        "{line}"
    );
    assert!(
        line.ends_with("--allowedTools Read Bash mcp__engram__* mcp__a2a__*"),
        "{line}"
    );
    assert!(!line.contains("--resume"), "{line}");
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(env.argv().len(), 1, "no duplicate wake on later ticks");
    let rows = env.rows("SELECT backend || ' ' || session_id FROM sessions WHERE agent = 'mentor'");
    assert_eq!(rows.len(), 1);
    let (backend, session) = rows[0].split_once(' ').unwrap();
    assert_eq!(backend, "claude");
    assert!(line.contains(&format!("--session-id {session}")));

    let second = env.send();
    wait_for("the second task completes", || {
        env.state(&second) == vec!["completed"]
    });
    let argv = env.argv();
    assert_eq!(argv.len(), 2, "{argv:?}");
    assert!(
        argv[1].contains(&format!("--resume {session}")) && !argv[1].contains("--session-id"),
        "{}",
        argv[1]
    );
    assert_eq!(
        env.rows("SELECT CAST(COUNT(*) AS TEXT) FROM sessions WHERE agent = 'mentor'"),
        vec!["1"],
        "the session is reused"
    );
    let cwds = std::fs::read_to_string(env.dir.join("cwd.log")).unwrap();
    let want = std::fs::canonicalize(&env.project).unwrap();
    assert!(
        cwds.lines()
            .all(|l| std::fs::canonicalize(l).unwrap() == want),
        "{cwds}"
    );
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn a_failing_turn_fails_the_task_with_its_error() {
    let env = Env::new("fail");
    std::fs::write(env.dir.join("mode"), "fail").unwrap();
    let _d = env.dispatcher();
    let id = env.send();
    wait_for("the task fails", || env.state(&id) == vec!["failed"]);
    let msg = env.rows(&format!(
        "SELECT status_message FROM tasks WHERE id = '{id}'"
    ));
    assert!(
        msg[0].contains("agent_error") && msg[0].contains("exited with code 3: boom"),
        "{}",
        msg[0]
    );
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn an_unknown_claude_agent_fails_the_task_at_once() {
    let env = Env::new("unknown");
    let _d = env.dispatcher();
    let dir = env.project.display().to_string();
    let id = env.hub(
        "send_message",
        json!({"from":"orchestrator","to":"ghost","text":"hi","directory":dir}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for("the task fails", || env.state(&id) == vec!["failed"]);
    let msg = env.rows(&format!(
        "SELECT status_message FROM tasks WHERE id = '{id}'"
    ));
    assert!(
        msg[0].contains("no_agent: claude has no agent named"),
        "{}",
        msg[0]
    );
    assert!(env.argv().is_empty());
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn oc_cancel_stops_a_running_turn() {
    let env = Env::new("cancel");
    std::fs::write(env.dir.join("mode"), "slow").unwrap();
    let _d = env.dispatcher();
    let id = env.send();
    wait_for("the turn is running", || env.dir.join("slow.pid").exists());
    let pid: u32 = std::fs::read_to_string(env.dir.join("slow.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let alive = |pid: u32| {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    assert!(alive(pid));
    let out = env
        .cmd("oc")
        .args(["cancel", "--all", "--yes"])
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        said.contains("canceled 1 task(s)") && said.contains("interrupted mentor"),
        "{said}"
    );
    wait_for("the turn is gone", || !alive(pid));
    assert_eq!(env.state(&id), vec!["canceled"]);
    assert!(
        env.rows("SELECT 'x' FROM nudges WHERE kind = 'assignee'")
            .len()
            == 1
    );
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn an_agents_own_opencode_registration_does_not_retire_its_claude_session() {
    let env = Env::new("register");
    let _d = env.dispatcher();
    let id = env.send();
    wait_for("the task completes", || env.state(&id) == vec!["completed"]);
    let dir = env
        .rows("SELECT directory FROM sessions WHERE backend = 'claude'")
        .remove(0);
    env.hub(
        "register_agent",
        json!({"name":"mentor","sessionId":"ses_oc","directory":dir}),
    );
    assert_eq!(
        env.rows("SELECT status FROM sessions WHERE backend = 'claude'"),
        vec!["live"],
        "a registration without a backend never touches claude sessions"
    );
    assert_eq!(env.rows("SELECT status || ' ' || IFNULL(backend, 'opencode') FROM sessions WHERE session_id = 'ses_oc'"), vec!["live opencode"]);
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn automatic_routing_persists_source_and_override_precedence() {
    let env = Env::new("routing");
    let per_agent = env.hub_meta(
        "send_message",
        json!({"from":"orchestrator","to":"mentor","text":"hi","directory":env.project.display().to_string()}),
        json!({"runtime":"claude"}),
        Some("claude"),
        Some("opencode"),
    );
    let context = per_agent["contextId"].as_str().unwrap();
    let metadata = env.rows(&format!(
        "SELECT metadata FROM contexts WHERE id = '{context}'"
    ));
    assert!(metadata[0].contains("agent-override"));
    assert!(metadata[0].contains("\"backend\":\"opencode\""));

    let global = env.hub_meta(
        "send_message",
        json!({"from":"orchestrator","to":"mentor","text":"hi","directory":env.project.display().to_string()}),
        json!({"runtime":"claude"}),
        Some("claude"),
        None,
    );
    let context = global["contextId"].as_str().unwrap();
    let metadata = env.rows(&format!(
        "SELECT metadata FROM contexts WHERE id = '{context}'"
    ));
    assert!(metadata[0].contains("global-override"));
    assert!(metadata[0].contains("\"backend\":\"claude\""));
}

#[test]
fn absent_identity_safely_selects_opencode_without_leaking_metadata() {
    let env = Env::new("safe-default");
    let task = env.hub_meta(
        "send_message",
        json!({"from":"orchestrator","to":"mentor","text":"secret prompt","runtime":"claude","backend":"claude","sessionId":"fake-unregistered","directory":env.project.display().to_string()}),
        json!({}),
        None,
        None,
    );
    let context = task["contextId"].as_str().unwrap();
    let metadata = env.rows(&format!(
        "SELECT metadata FROM contexts WHERE id = '{context}'"
    ));
    assert!(metadata[0].contains("safe-default"));
    assert!(metadata[0].contains("\"backend\":\"opencode\""));
    assert!(!metadata[0].contains("secret prompt"));
    assert!(!metadata[0].contains("fake-unregistered"));
}

#[test]
fn marker_attribution_ignores_spoofed_arguments_and_fake_session() {
    let env = Env::new("spoof");
    let task = env.hub_meta(
        "send_message",
        json!({
            "from":"orchestrator", "to":"mentor", "text":"hi",
            "runtime":"opencode", "backend":"opencode", "sessionId":"fake-unregistered",
            "directory":env.project.display().to_string()
        }),
        json!({}),
        Some("claude"),
        None,
    );
    let context = task["contextId"].as_str().unwrap();
    let metadata = env.rows(&format!(
        "SELECT metadata FROM contexts WHERE id = '{context}'"
    ));
    assert!(metadata[0].contains("safe-default"), "{}", metadata[0]);
    assert!(metadata[0].contains("\"backend\":\"opencode\""));
    assert!(env
        .rows("SELECT session_id FROM sessions WHERE session_id = 'fake-unregistered'")
        .is_empty());
    let _ = std::fs::remove_dir_all(&env.dir);
}

fn tmux(server: &str, args: &[&str]) -> String {
    let out = Command::new("tmux")
        .args(["-L", server, "-f", "/dev/null"])
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Kills the throwaway tmux server when the test ends, pass or fail.
struct Server(String);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = tmux(&self.0, &["kill-server"]);
    }
}

#[test]
fn a_claude_turn_gets_its_own_tmux_window_and_closing_it_does_not_cancel_the_turn() {
    if Command::new("tmux").arg("-V").output().is_err() {
        return;
    }
    let env = Env::new("tmux");
    std::fs::write(env.dir.join("mode"), "slow").unwrap();
    let server = Server(format!("a2atest-{}", std::process::id()));
    tmux(&server.0, &["new-session", "-d", "-s", "t", "sleep 600"]);
    let socket = tmux(
        &server.0,
        &["display-message", "-p", "#{socket_path},#{pid},0"],
    );
    let pane = tmux(&server.0, &["display-message", "-p", "#{pane_id}"]);
    let mut cmd = env.cmd("dispatcher");
    cmd.env("TMUX", socket).env("TMUX_PANE", pane);
    let _d = Proc(cmd.spawn().unwrap());
    env.send();
    wait_for("the turn is running", || env.dir.join("slow.pid").exists());
    wait_for("the agent window opens", || {
        tmux(&server.0, &["list-windows", "-F", "#{window_name}"])
            .lines()
            .any(|w| w == "mentor")
    });
    let pid: u32 = std::fs::read_to_string(env.dir.join("slow.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let alive = |pid: u32| {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    tmux(&server.0, &["kill-window", "-t", "t:mentor"]);
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !tmux(&server.0, &["list-windows", "-F", "#{window_name}"])
            .lines()
            .any(|w| w == "mentor"),
        "window closed"
    );
    assert!(alive(pid), "the turn survives its window");
    let _ = Command::new("kill").arg(pid.to_string()).status();
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[test]
fn the_api_key_is_withheld_from_headless_turns_unless_api_mode_asks_for_it() {
    for (auth, expected) in [("subscription", "none"), ("api", "test-key-not-real")] {
        let env = Env::new(&format!("key-{auth}"));
        let _d = env.dispatcher_with_key(auth);
        let task = env.send();
        wait_for("the fake agent completes the task", || {
            env.state(&task) == vec!["completed"]
        });
        assert_eq!(env.keys_seen_by_claude(), vec![expected.to_string()], "A2A_CLAUDE_AUTH={auth}");
    }
}
