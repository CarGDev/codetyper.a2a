//! `codetypera2a eval`: replay a fixed set of deterministic scenarios against the real hub and
//! dispatcher with a fake `opencode` (no LLM, no network). One line per scenario, exit 0 only
//! when nothing FAILs. Add a scenario by adding one entry to `SCENARIOS`.
use crate::hub::db;
use crate::trace;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const CODE_REVIEWER: &str = include_str!("../agents/code-reviewer.md");
const DIR: &str = "/tmp/projx";

enum Outcome {
    Pass(String),
    Fail(String),
    Skip(String),
}

type Check = Result<String, String>;

struct Scenario {
    name: &'static str,
    run: fn() -> Outcome,
}

/// The single scenario table.
const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "routing",
        run: || checked(routing),
    },
    Scenario {
        name: "hop-cap",
        run: || checked(hop_cap),
    },
    Scenario {
        name: "no-duplicate-nudges",
        run: || checked(no_duplicate_nudges),
    },
    Scenario {
        name: "reviewer-seeded-bug",
        run: || checked(reviewer_seeded_bug),
    },
    Scenario {
        name: "reviewer-live",
        run: || Outcome::Skip("needs --live, no model grading offline".into()),
    },
];

fn checked(f: fn() -> Check) -> Outcome {
    match f() {
        Ok(m) => Outcome::Pass(m),
        Err(m) => Outcome::Fail(m),
    }
}

pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--list") {
        SCENARIOS.iter().for_each(|s| println!("{}", s.name));
        return 0;
    }
    let filter = args.iter().find(|a| !a.starts_with('-'));
    let chosen: Vec<&Scenario> = SCENARIOS
        .iter()
        .filter(|s| filter.is_none_or(|f| s.name == f.as_str()))
        .collect();
    if chosen.is_empty() {
        eprintln!(
            "unknown scenario: {}",
            filter.map(String::as_str).unwrap_or("")
        );
        return 2;
    }
    let (mut pass, mut fail, mut skip) = (0, 0, 0);
    for s in chosen {
        match (s.run)() {
            Outcome::Pass(m) => {
                pass += 1;
                println!("PASS {} - {m}", s.name);
            }
            Outcome::Fail(m) => {
                fail += 1;
                println!("FAIL {} - {m}", s.name);
            }
            Outcome::Skip(m) => {
                skip += 1;
                println!("SKIP {} ({m})", s.name);
            }
        }
    }
    println!("{pass} passed, {fail} failed, {skip} skipped");
    if fail == 0 { 0 } else { 1 }
}

// ---------- sandbox: temp dir, fake opencode, hub client, dispatcher processes ----------

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Sandbox {
    dir: PathBuf,
    max_hops: u32,
    children: Vec<Child>,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        for c in &mut self.children {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Sandbox {
    /// `assignee` is the one agent the fake opencode knows; its session is always `ses_a1`.
    fn new(assignee: &str, max_hops: u32) -> Result<Sandbox, String> {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("a2a-eval-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("agents")).map_err(|e| e.to_string())?;
        std::fs::write(
            dir.join(format!("agents/{assignee}.md")),
            "---\ndescription: eval\n---\n",
        )
        .map_err(|e| e.to_string())?;
        let stub = dir.join("opencode");
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> {log}\ncase \"$2 $3\" in\n  \"get /api/session/active\") echo '{{\"data\":{{}}}}';;\n  \"get /api/session/orch1\") echo '{{\"data\":{{\"id\":\"orch1\",\"agent\":\"orchestrator\",\"location\":{{\"directory\":\"{DIR}\"}},\"time\":{{\"created\":1,\"updated\":2}}}}}}';;\n  \"get /api/session/ses_a1\") echo '{{\"data\":{{\"id\":\"ses_a1\",\"agent\":\"{assignee}\",\"location\":{{\"directory\":\"{DIR}\"}},\"time\":{{\"created\":1,\"updated\":2}}}}}}';;\n  \"get /api/agent\"*) echo '{{\"data\":[{{\"id\":\"{assignee}\",\"description\":\"eval\",\"mode\":\"primary\",\"hidden\":false}}]}}';;\n  \"post /api/session\") echo '{{\"data\":{{\"id\":\"ses_a1\"}}}}';;\nesac\n",
            log = dir.join("calls.log").display()
        );
        std::fs::write(&stub, script).map_err(|e| e.to_string())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        Ok(Sandbox {
            dir,
            max_hops,
            children: Vec::new(),
        })
    }

    fn cmd(&self, arg: &str, pidfile: &str) -> Result<Command, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut c = Command::new(exe);
        c.arg(arg)
            .env("A2A_DB", self.dir.join("hub.db"))
            .env("A2A_AGENT_DIR", self.dir.join("agents"))
            .env("A2A_DISPATCHER_PID", self.dir.join(pidfile))
            .env("OPENCODE_BIN", self.dir.join("opencode"))
            .env("A2A_NO_DISPATCHER", "1")
            .env("DISPATCH_POLL_MS", "100")
            .env("DISPATCH_MAX_HOPS", self.max_hops.to_string())
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Ok(c)
    }

    /// Dispatchers get distinct pidfiles on purpose, so several can race on one database.
    fn dispatchers(&mut self, n: usize) -> Result<(), String> {
        for i in 0..n {
            let child = self
                .cmd("dispatcher", &format!("d{i}.pid"))?
                .spawn()
                .map_err(|e| e.to_string())?;
            self.children.push(child);
        }
        std::thread::sleep(Duration::from_millis(400));
        Ok(())
    }

    /// One MCP tool call against a short-lived hub process.
    fn hub(&self, tool: &str, args: Value) -> Result<Value, String> {
        let mut child = self
            .cmd("hub", "unused.pid")?
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let req = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}});
        writeln!(child.stdin.as_mut().ok_or("no hub stdin")?, "{req}")
            .map_err(|e| e.to_string())?;
        let mut line = String::new();
        BufReader::new(child.stdout.take().ok_or("no hub stdout")?)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        drop(child.stdin.take());
        let _ = child.wait();
        let v: Value = serde_json::from_str(&line)
            .map_err(|e| format!("hub {tool}: bad reply {line:?}: {e}"))?;
        Ok(v["result"]["structuredContent"].clone())
    }

    fn send(&self, to: &str, text: &str, context: Option<&str>) -> Result<String, String> {
        let mut args =
            json!({"from":"orchestrator","to":to,"text":text,"directory":DIR,"sessionId":"orch1"});
        if let Some(c) = context {
            args["contextId"] = json!(c);
        }
        let task = self.hub("send_message", args)?;
        task["id"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| format!("send_message returned no id: {task}"))
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("calls.log")).unwrap_or_default()
    }

    fn calls(&self, needle: &str) -> usize {
        self.log().lines().filter(|l| l.contains(needle)).count()
    }

    /// Poll until `needle` has appeared at least once, then give a stray duplicate time to show up.
    fn wait_calls(&self, needle: &str) -> Result<usize, String> {
        let end = Instant::now() + Duration::from_secs(10);
        while self.calls(needle) == 0 {
            if Instant::now() > end {
                return Err(format!("timed out waiting for `{needle}`"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        std::thread::sleep(Duration::from_millis(700));
        Ok(self.calls(needle))
    }

    fn conn(&self) -> Result<rusqlite::Connection, String> {
        db::open(&self.dir.join("hub.db"))
    }

    fn count(&self, sql: &str) -> Result<i64, String> {
        self.conn()?
            .query_row(sql, [], |r| r.get(0))
            .map_err(|e| e.to_string())
    }
}

fn expect(what: &str, got: usize, want: usize) -> Result<(), String> {
    if got == want {
        Ok(())
    } else {
        Err(format!("{what}: expected {want}, saw {got}"))
    }
}

const ASSIGNEE_PROMPT: &str = "post /api/session/ses_a1/prompt";
const REQUESTER_PROMPT: &str = "post /api/session/orch1/prompt";

/// orchestrator -> mentor -> completed -> orchestrator woken, with `dispatchers` racing.
fn round_trip(dispatchers: usize) -> Result<(Sandbox, String), String> {
    let mut sb = Sandbox::new("mentor", 40)?;
    sb.dispatchers(dispatchers)?;
    let id = sb.send("mentor", "please explain borrowing", None)?;
    expect("assignee prompts", sb.wait_calls(ASSIGNEE_PROMPT)?, 1)?;
    expect("session creates", sb.calls("post /api/session -d"), 1)?;
    sb.hub(
        "update_task",
        json!({"id": id, "agent":"mentor", "state":"completed", "text":"done"}),
    )?;
    expect("requester wake-ups", sb.wait_calls(REQUESTER_PROMPT)?, 1)?;
    Ok((sb, id))
}

fn nudge_kinds(sb: &Sandbox) -> Result<(i64, i64, i64), String> {
    let total = sb.count("SELECT COUNT(*) FROM nudges")?;
    let distinct = sb.count("SELECT COUNT(DISTINCT key) FROM nudges")?;
    let assignee = sb.count("SELECT COUNT(*) FROM nudges WHERE kind = 'assignee'")?;
    let requester = sb.count("SELECT COUNT(*) FROM nudges WHERE kind = 'requester'")?;
    if total != distinct {
        return Err(format!(
            "duplicate nudge keys: {total} rows, {distinct} distinct"
        ));
    }
    Ok((assignee, requester, total))
}

// ---------- scenarios ----------

fn routing() -> Check {
    let (sb, id) = round_trip(1)?;
    let (assignee, requester, _) = nudge_kinds(&sb)?;
    if (assignee, requester) != (1, 1) {
        return Err(format!(
            "nudges: assignee={assignee} requester={requester}, want 1/1"
        ));
    }
    let events = trace::events_for(&sb.conn()?, &id)?;
    if !events
        .iter()
        .any(|e| e.kind == "state_change" && e.detail.as_deref() == Some("completed"))
    {
        return Err(format!(
            "no completed state_change event in {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        ));
    }
    Ok("mentor prompted once, requester woken once after completion".into())
}

fn hop_cap() -> Check {
    let mut sb = Sandbox::new("mentor", 2)?;
    sb.dispatchers(1)?;
    // hop 1 (assignee) and hop 2 (requester) use up the allowance of 2 for the context
    let first = sb.send("mentor", "round one", Some("eval-ctx"))?;
    expect("assignee prompts", sb.wait_calls(ASSIGNEE_PROMPT)?, 1)?;
    sb.hub(
        "update_task",
        json!({"id": first, "agent":"mentor", "state":"completed", "text":"done"}),
    )?;
    expect("requester wake-ups", sb.wait_calls(REQUESTER_PROMPT)?, 1)?;
    // the next task in the same context must not reach the assignee
    let second = sb.send("mentor", "round two", Some("eval-ctx"))?;
    let end = Instant::now() + Duration::from_secs(10);
    while sb
        .count("SELECT COUNT(*) FROM events WHERE kind = 'hop_cap' AND context_id = 'eval-ctx'")?
        == 0
    {
        if Instant::now() > end {
            return Err("no hop_cap event recorded".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(700));
    expect(
        "assignee prompts after the cap",
        sb.calls(ASSIGNEE_PROMPT),
        1,
    )?;
    let delivered = sb
        .count("SELECT COUNT(*) FROM nudges WHERE context_id = 'eval-ctx' AND kind = 'assignee'")?;
    if delivered != 1 {
        return Err(format!(
            "{delivered} assignee deliveries, cap allows 1 ({second})"
        ));
    }
    // past the cap the only wake-up is the requester's hop_cap notice (input-required), never more work
    let notice = sb.log().lines().any(|l| {
        l.contains(REQUESTER_PROMPT) && l.contains(&second) && l.contains("is now input-required")
    });
    if !notice {
        return Err(format!(
            "the requester was not told about the cap: {:?}",
            sb.log()
        ));
    }
    Ok("2 hops delivered, 2nd task never reached the assignee, hop_cap recorded and requester told".into())
}

fn no_duplicate_nudges() -> Check {
    let (sb, _) = round_trip(2)?;
    let (assignee, requester, total) = nudge_kinds(&sb)?;
    if (assignee, requester) != (1, 1) {
        return Err(format!(
            "nudges: assignee={assignee} requester={requester}, want 1/1 ({total} rows)"
        ));
    }
    Ok("two dispatchers, each nudge key delivered once".into())
}

fn reviewer_seeded_bug() -> Check {
    let mut sb = Sandbox::new("code-reviewer", 40)?;
    let fixture = sb.dir.join("fixture");
    std::fs::create_dir_all(fixture.join("src")).map_err(|e| e.to_string())?;
    // seeded bug: `..=` reads one element past the end
    std::fs::write(
        fixture.join("src/sum.rs"),
        "pub fn sum(v: &[i32]) -> i32 {\n    let mut t = 0;\n    for i in 0..=v.len() {\n        t += v[i];\n    }\n    t\n}\n",
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(
        fixture.join("REVIEW.md"),
        "Review src/sum.rs for correctness bugs.\n",
    )
    .map_err(|e| e.to_string())?;
    let fixture_s = fixture.display().to_string();
    sb.dispatchers(1)?;
    let id = sb.send(
        "code-reviewer",
        &format!("Review {fixture_s}/src/sum.rs, see {fixture_s}/REVIEW.md"),
        None,
    )?;
    expect("reviewer prompts", sb.wait_calls(ASSIGNEE_PROMPT)?, 1)?;
    // the wake-up prompt names the task; the review request itself (with the path) lives in the task
    if !sb
        .log()
        .lines()
        .any(|l| l.contains(ASSIGNEE_PROMPT) && l.contains(&id) && l.contains("code-reviewer"))
    {
        return Err(format!(
            "the reviewer was not woken for task {id}: {:?}",
            sb.log()
        ));
    }
    let like = format!("%{}%", fixture_s.replace('%', ""));
    let carried: i64 = sb
        .conn()?
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE id = ? AND to_agent = 'code-reviewer' AND history LIKE ?",
            rusqlite::params![id, like],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if carried != 1 {
        return Err("the task routed to code-reviewer does not carry the fixture path".into());
    }
    template_is_read_only(CODE_REVIEWER)?;
    Ok("routed to code-reviewer with fixture path; template is read-only".into())
}

/// Same checks as tests/agents.rs for read-only agents.
fn template_is_read_only(text: &str) -> Result<(), String> {
    let fm = text
        .split("---\n")
        .nth(1)
        .ok_or("code-reviewer.md has no frontmatter")?;
    if !(fm.contains("  edit: false") && fm.contains("  write: false")) {
        return Err("code-reviewer must have edit: false and write: false".into());
    }
    let bash = fm
        .split("  bash:\n")
        .nth(1)
        .ok_or("code-reviewer has no permission.bash block")?;
    let rules: Vec<&str> = bash.lines().take_while(|l| l.starts_with("    ")).collect();
    if rules.first().map(|r| r.trim()) != Some("\"*\": deny") {
        return Err("code-reviewer bash must deny everything first".into());
    }
    if !rules
        .iter()
        .any(|r| r.contains("git diff*") && r.contains("allow"))
    {
        return Err("code-reviewer must allow git diff".into());
    }
    if rules
        .iter()
        .any(|r| r.contains("git commit") || r.contains("git push"))
    {
        return Err("code-reviewer allows a write command".into());
    }
    Ok(())
}
