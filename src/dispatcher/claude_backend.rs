//! Claude Code backend: every wake is one headless `claude -p` turn.
//!
//!  - the first wake of an agent in a project creates a session id (a UUID, registered with
//!    `sessions.backend = 'claude'`) and runs `claude -p <prompt> --agent <name> --session-id <id>`;
//!    every later wake runs `--resume <id>`, so the agent keeps its conversation like an opencode tab
//!  - the turn runs in the folder the session was registered under (Claude Code looks sessions up by
//!    folder); the task's own folder is added with `--add-dir` when it is a sub-folder
//!  - headless turns cannot ask for permission, so `--allowedTools` is built from the `tools:` line of
//!    the installed agent file (always with the hub's `mcp__a2a__*`)
//!  - a turn is a child process in its own process group, tracked in `<db>.claude-runs/<session>.pid`
//!    (busy = the pid is alive; `oc cancel` signals the group from another process); its stream-json
//!    output goes to `<session>.log`, which the agent's tmux window follows (`oc follow`, opened by
//!    `request_tab`, off with `DISPATCH_AUTO_OPEN=0`); closing the window never touches the turn
//!  - a non-zero exit or an `is_error` result is kept in memory and reported by `failure_since`, which
//!    feeds the dispatcher's existing `agent_error:` path; a spawn error goes through `delivery_failed`
//!  - usage is NOT reported (`usage` is `None`): headless turns write normal transcripts, which the
//!    transcript ingester (`launcher/claude_usage.rs`) already counts; recording them here too would
//!    count every turn twice
//!  - the requester is never woken through this backend unless it is a registered headless session: an
//!    interactive Claude Code session cannot be injected into; it sees results through the
//!    `UserPromptSubmit` hook and the status line (`oc statusline`)
//!
//! Binary: `CLAUDE_BIN` (default `claude`).
use super::backend::{Backend, R, SessionState, Style};
use super::dispatcher::{first_line, log};
use crate::hub::project::project_root;
use crate::opencode::Usage;
use crate::{paths, sys};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// What a headless turn printed (`--output-format stream-json`, one event per line).
#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub is_error: bool,
    pub text: String,
    pub session_id: Option<String>,
}

fn outcome_of(v: &Value) -> Option<Outcome> {
    (v["type"] == "result").then(|| Outcome {
        is_error: v["is_error"].as_bool().unwrap_or(false),
        text: v["result"].as_str().unwrap_or("").to_string(),
        session_id: v["session_id"].as_str().map(String::from),
    })
}

/// The `result` message of a turn: the whole output as one object or an event list, else the last
/// line that is one (a CLI may print notices around it).
pub fn parse_result(stdout: &str) -> Option<Outcome> {
    let whole = serde_json::from_str::<Value>(stdout.trim()).ok();
    let events = whole.as_ref().and_then(Value::as_array).map(|a| a.iter().rev().find_map(outcome_of));
    events.flatten().or_else(|| whole.as_ref().and_then(outcome_of)).or_else(|| {
        stdout.lines().rev().filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok()).find_map(|v| outcome_of(&v))
    })
}

/// Tools a headless agent may use when its file lists none (the agent inherits everything).
const DEFAULT_TOOLS: [&str; 6] = ["Read", "Grep", "Glob", "Edit", "Write", "Bash"];
const HUB_TOOLS: &str = "mcp__a2a__*";

/// Split on commas that are not inside parentheses (`Agent(a, b), Read`).
fn split_tools(line: &str) -> Vec<String> {
    let (mut out, mut depth, mut cur) = (Vec::new(), 0_i32, String::new());
    for c in line.chars() {
        depth += i32::from(c == '(') - i32::from(c == ')');
        if c == ',' && depth == 0 {
            out.push(std::mem::take(&mut cur));
            continue;
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect()
}

/// `--allowedTools` for an agent: the `tools:` line of its installed file (front matter only), plus the hub.
pub fn allowed_tools(agent_file: Option<&str>) -> Vec<String> {
    let front = agent_file.and_then(|t| t.strip_prefix("---")).and_then(|t| t.split("\n---").next());
    let listed = front.and_then(|f| f.lines().find_map(|l| l.strip_prefix("tools:"))).map(split_tools);
    let mut tools = listed.unwrap_or_else(|| DEFAULT_TOOLS.iter().map(|t| t.to_string()).collect());
    if !tools.iter().any(|t| t == HUB_TOOLS) {
        tools.push(HUB_TOOLS.to_string());
    }
    tools
}

/// The arguments of one turn. The variadic `--allowedTools` list goes last so it cannot swallow a flag.
pub fn argv(prompt: &str, agent: &str, session: &str, resume: bool, task_dir: Option<&str>, cwd: &str, tools: &[String]) -> Vec<String> {
    let session_flag = if resume { "--resume" } else { "--session-id" };
    let mut a: Vec<String> = ["-p", prompt, "--output-format", "stream-json", "--verbose", "--agent", agent, session_flag, session].map(String::from).to_vec();
    if let Some(d) = task_dir.filter(|d| *d != cwd) {
        a.extend(["--add-dir".to_string(), d.to_string()]);
    }
    a.push("--allowedTools".into());
    a.extend(tools.iter().cloned());
    a
}

#[derive(Debug, PartialEq)]
pub struct Settled {
    /// Why the turn counts as failed; `None` = fine (or interrupted).
    pub failure: Option<String>,
    /// Claude Code got far enough to hold the session, so the next wake must `--resume` it.
    pub started: bool,
}

/// How a finished child maps to the dispatcher's failure path. `code` is `None` when a signal ended
/// it (`oc cancel`): that is not a failure.
pub fn settle(code: Option<i32>, stdout: &str, stderr: &str) -> Settled {
    let Some(code) = code else { return Settled { failure: None, started: false } };
    let outcome = parse_result(stdout);
    let started = code == 0 || outcome.as_ref().is_some_and(|o| o.session_id.is_some());
    let reported = outcome.filter(|o| o.is_error).map(|o| match o.text.is_empty() {
        true => "claude reported an error".to_string(),
        false => first_line(&o.text).to_string(),
    });
    let failure = reported.or_else(|| {
        let why = [stderr, stdout].into_iter().map(first_line).find(|l| !l.trim().is_empty()).unwrap_or("no output");
        (code != 0).then(|| format!("claude exited with code {code}: {why}"))
    });
    Settled { failure, started }
}

/// Claude Code stores a conversation as `<config>/projects/<folder slug>/<session>.jsonl`. A turn that was
/// cancelled may already have created it, and then only `--resume` can continue it.
fn transcript_exists(session: &str) -> bool {
    let Ok(rd) = std::fs::read_dir(paths::claude_dir().join("projects")) else { return false };
    rd.flatten().any(|e| e.path().join(format!("{session}.jsonl")).is_file())
}

/// What the agent's tmux window runs. Through the `oc` symlink the subcommand word is dropped (see
/// `ensure_status_window`).
pub fn follow_command(exe: &Path, log: &Path) -> String {
    let word = if exe.file_name().is_some_and(|n| n == "oc") { "follow" } else { "oc follow" };
    format!("A2A_STATUS_WINDOW=1 A2A_NO_DISPATCHER=1 {} {word} {}", sys::sh(&exe.display().to_string()), sys::sh(&log.display().to_string()))
}

/// One stream-json event as readable lines; events that carry nothing to read give none.
pub fn render_event(line: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { return Vec::new() };
    let blocks = v["message"]["content"].as_array().cloned().unwrap_or_default();
    let block = |b: &Value| match b["type"].as_str() {
        Some("text") => b["text"].as_str().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()),
        Some("tool_use") => Some(format!("> {} {}", b["name"].as_str().unwrap_or("tool"), first_line(&b["input"].to_string()).chars().take(100).collect::<String>())),
        _ => None,
    };
    match v["type"].as_str() {
        Some("assistant") => blocks.iter().filter_map(block).collect(),
        Some("result") => vec![format!("-- turn {} --", if v["is_error"].as_bool().unwrap_or(false) { "failed" } else { "done" })],
        Some("system") if v["subtype"] == "init" => vec![format!("-- turn started ({}) --", v["model"].as_str().unwrap_or("claude"))],
        _ => Vec::new(),
    }
}

/// `oc follow <log>`: print the turn's events as they arrive, across turns (a new turn truncates the
/// file), until the window is closed.
pub fn follow(log_file: &Path) -> i32 {
    use std::io::{Read, Seek, SeekFrom, Write};
    println!("following {}", log_file.display());
    let (mut pos, mut pending) = (0_u64, String::new());
    loop {
        if let Ok(mut f) = std::fs::File::open(log_file) {
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            if len < pos {
                (pos, pending) = (0, String::new());
            }
            let _ = f.seek(SeekFrom::Start(pos));
            let mut chunk = Vec::new();
            let _ = f.read_to_end(&mut chunk);
            pos += chunk.len() as u64;
            pending.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(i) = pending.find('\n') {
                let line: String = pending.drain(..=i).collect();
                render_event(&line).iter().for_each(|l| println!("{l}"));
            }
            let _ = std::io::stdout().flush();
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
}

pub struct ClaudeBackend {
    runs: PathBuf,
    bin: String,
    /// session id -> (finished at ms, error) of the last failed turn; the dispatcher process only.
    failures: Arc<Mutex<HashMap<String, (i64, String)>>>,
    /// Which credentials headless turns use; decided on the first turn and logged once.
    auth: std::sync::OnceLock<super::claude_auth::Choice>,
}

impl ClaudeBackend {
    pub fn from_env() -> Self {
        ClaudeBackend {
            runs: paths::claude_runs_dir(),
            bin: std::env::var("CLAUDE_BIN").ok().filter(|b| !b.is_empty()).unwrap_or_else(|| "claude".into()),
            failures: Arc::default(),
            auth: std::sync::OnceLock::new(),
        }
    }

    fn pid_file(&self, session: &str) -> PathBuf {
        self.runs.join(format!("{session}.pid"))
    }

    fn log_file(&self, session: &str) -> PathBuf {
        self.runs.join(format!("{session}.log"))
    }

    fn read_meta(runs: &Path, session: &str) -> Value {
        std::fs::read_to_string(runs.join(format!("{session}.json"))).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null)
    }

    fn write_meta(runs: &Path, session: &str, meta: &Value) {
        let _ = std::fs::create_dir_all(runs).and_then(|_| std::fs::write(runs.join(format!("{session}.json")), meta.to_string()));
    }

    /// The pid of the run in progress, if its process is alive; a dead one's file is removed.
    fn running_pid(&self, session: &str) -> Option<u32> {
        let file = self.pid_file(session);
        let pid = std::fs::read_to_string(&file).ok().and_then(|t| t.trim().parse::<u32>().ok())?;
        if sys::pid_alive(pid) {
            return Some(pid);
        }
        let _ = std::fs::remove_file(&file);
        None
    }

    /// The agent file Claude Code will use: the project's first, then the user's.
    fn agent_file(&self, name: &str, dir: Option<&str>) -> Option<PathBuf> {
        let project = dir.map(|d| Path::new(d).join(".claude").join("agents").join(format!("{name}.md")));
        let user = paths::claude_dir().join("agents").join(format!("{name}.md"));
        project.into_iter().chain([user]).find(|p| p.is_file())
    }
}

impl Backend for ClaudeBackend {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn stored_name(&self) -> Option<&'static str> {
        Some("claude")
    }

    fn style(&self) -> Style {
        Style {
            tool_prefix: "mcp__a2a__",
            assignee_note: " You run headless: do not wait or poll for anyone. When the work is done or you need an answer, call mcp__a2a__update_task and end your turn.",
        }
    }

    /// There is no server to ask: a registered session exists while its folder does.
    fn session_state(&self, _id: &str, registered_dir: &str) -> SessionState {
        match Path::new(registered_dir).is_dir() {
            true => SessionState::Usable,
            false => SessionState::FolderGone,
        }
    }

    /// Only picks the id and the folder; Claude Code creates the conversation on the first turn.
    fn create_session(&self, _agent: &str, dir: &str) -> R<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let root = project_root(dir).unwrap_or_else(|| dir.to_string());
        std::fs::create_dir_all(&self.runs).map_err(|e| format!("cannot create {}: {e}", self.runs.display()))?;
        Self::write_meta(&self.runs, &id, &json!({ "cwd": root, "started": false }));
        Ok(id)
    }

    fn active_ids(&self) -> R<HashSet<String>> {
        let Ok(rd) = std::fs::read_dir(&self.runs) else { return Ok(HashSet::new()) };
        let ids = rd.flatten().filter_map(|e| e.file_name().to_str()?.strip_suffix(".pid").map(String::from));
        Ok(ids.filter(|id| self.running_pid(id).is_some()).collect())
    }

    fn send(&self, session: &str, agent: &str, dir: Option<&str>, text: &str) -> R<bool> {
        // An interactive session in a tmux pane is woken by typing into it; a headless run cannot.
        if let Some(pane) = super::wake::pane_for(&self.runs, session) {
            return super::wake::inject(&pane, text);
        }
        if self.running_pid(session).is_some() {
            return Ok(false);
        }
        let meta = Self::read_meta(&self.runs, session);
        let cwd = meta["cwd"].as_str().or(dir).ok_or("the task has no folder")?.to_string();
        let tools = allowed_tools(self.agent_file(agent, dir).and_then(|p| std::fs::read_to_string(p).ok()).as_deref());
        let args = argv(text, agent, session, meta["started"].as_bool().unwrap_or(false), dir, &cwd, &tools);
        std::fs::create_dir_all(&self.runs).map_err(|e| e.to_string())?;
        let log_file = self.log_file(session);
        let out_file = std::fs::File::create(&log_file).map_err(|e| format!("cannot create {}: {e}", log_file.display()))?;
        let mut command = Command::new(&self.bin);
        let auth = self.auth.get_or_init(|| {
            let choice = super::claude_auth::choose(&self.bin);
            log(&choice.summary);
            choice
        });
        if auth.strip_key {
            command.env_remove("ANTHROPIC_API_KEY");
        }
        let child = command
            .args(&args)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .env("A2A_HEADLESS", "1")

            .stdout(out_file)
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", self.bin))?;
        let _ = std::fs::write(self.pid_file(session), child.id().to_string());
        let (runs, failures, session, agent, pid_file) = (self.runs.clone(), self.failures.clone(), session.to_string(), agent.to_string(), self.pid_file(session));
        let log_file = self.log_file(&session);
        std::thread::spawn(move || {
            let out = child.wait_with_output();
            let _ = std::fs::remove_file(&pid_file);
            let Ok(out) = out else { return };
            let stdout = std::fs::read(&log_file).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
            let settled = settle(out.status.code(), &stdout, &String::from_utf8_lossy(&out.stderr));
            let was = ClaudeBackend::read_meta(&runs, &session)["started"].as_bool().unwrap_or(false);
            let started = was || settled.started || transcript_exists(&session);
            ClaudeBackend::write_meta(&runs, &session, &json!({ "cwd": cwd, "started": started }));
            let source = super::claude_auth::reported_source(&stdout).unwrap_or_else(|| "unknown".into());
            let Some(why) = settled.failure else {
                log(&format!("claude turn of {agent} (session {session}) ended (credentials: {source})"));
                return;
            };
            log(&format!("claude turn of {agent} (session {session}) failed: {why} (credentials: {source})"));
            failures.lock().unwrap().insert(session, (chrono::Utc::now().timestamp_millis(), why));
        });
        Ok(true)
    }

    /// Always `None`: see the module comment (the transcript ingester owns Claude Code usage).
    fn usage(&self, _session: &str) -> Option<Usage> {
        None
    }

    fn failure_since(&self, session: &str, since_ms: i64) -> Option<String> {
        self.failures.lock().unwrap().get(session).filter(|(at, _)| *at > since_ms).map(|(_, why)| why.clone())
    }

    /// An installed agent file, in the project or in the Claude config folder.
    fn agent_known(&self, name: &str, dir: Option<&str>) -> bool {
        self.agent_file(name, dir).is_some()
    }

    /// A tmux window named after the agent that follows the turn's log. The turn writes to a file, so the
    /// window can be closed at any time without cancelling it.
    fn request_tab(&self, session: &str, agent: &str, _dir: &str) {
        if std::env::var("DISPATCH_AUTO_OPEN").as_deref() == Ok("0") {
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        let cmd = follow_command(&exe, &self.log_file(session));
        if let Err(err) = sys::tmux_ensure_window(agent, &cmd) {
            log(&format!("could not open the tmux window for {agent}: {err}"));
        }
    }

    fn interrupt(&self, session: &str) -> R<()> {
        let pid = self.running_pid(session).ok_or_else(|| format!("session {session} has no running turn"))?;
        sys::kill_group_term(pid);
        let _ = std::fs::remove_file(self.pid_file(session));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_object_or_the_last_result_of_an_event_list_is_parsed() {
        let one = r#"{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"s-1","total_cost_usd":0.1}"#;
        assert_eq!(parse_result(one), Some(Outcome { is_error: false, text: "done".into(), session_id: Some("s-1".into()) }));
        let list = r#"[{"type":"system","session_id":"s-1"},{"type":"result","is_error":true,"result":"Credit balance is too low","session_id":"s-1"}]"#;
        assert_eq!(parse_result(list), Some(Outcome { is_error: true, text: "Credit balance is too low".into(), session_id: Some("s-1".into()) }));
        assert_eq!(parse_result("not json"), None);
        assert_eq!(parse_result(r#"{"type":"system"}"#), None);
        assert_eq!(parse_result(&format!("noise\n{one}\n")), Some(Outcome { is_error: false, text: "done".into(), session_id: Some("s-1".into()) }));
    }

    #[test]
    fn the_agents_tools_line_becomes_the_allow_list_and_always_has_the_hub() {
        let file = "---\nname: coder\ndescription: d, with a comma\ntools: Read, Bash, Agent(a, b), mcp__engram__*\nmodel: sonnet\n---\nbody\ntools: not this one\n";
        assert_eq!(allowed_tools(Some(file)), ["Read", "Bash", "Agent(a, b)", "mcp__engram__*", "mcp__a2a__*"]);
        let hub = "---\ntools: mcp__a2a__*, Read\n---\n";
        assert_eq!(allowed_tools(Some(hub)), ["mcp__a2a__*", "Read"], "not added twice");
        let none = allowed_tools(Some("---\nname: x\n---\nbody"));
        assert!(none.contains(&"Edit".to_string()) && none.contains(&"mcp__a2a__*".to_string()), "no tools line = the usual working set: {none:?}");
        assert_eq!(allowed_tools(None), none);
    }

    #[test]
    fn the_first_wake_names_the_session_and_later_ones_resume_it() {
        let tools = vec!["Read".to_string(), "mcp__a2a__*".to_string()];
        let first = argv("hi", "coder", "uuid-1", false, Some("/p"), "/p", &tools);
        assert_eq!(first, ["-p", "hi", "--output-format", "stream-json", "--verbose", "--agent", "coder", "--session-id", "uuid-1", "--allowedTools", "Read", "mcp__a2a__*"]);
        let later = argv("hi", "coder", "uuid-1", true, Some("/p/sub"), "/p", &tools);
        assert_eq!(later, ["-p", "hi", "--output-format", "stream-json", "--verbose", "--agent", "coder", "--resume", "uuid-1", "--add-dir", "/p/sub", "--allowedTools", "Read", "mcp__a2a__*"]);
        assert_eq!(*first.last().unwrap(), "mcp__a2a__*", "the variadic list comes last so it cannot swallow other flags");
    }

    #[test]
    fn stream_events_render_as_readable_lines() {
        let init = r#"{"type":"system","subtype":"init","model":"sonnet"}"#;
        assert_eq!(render_event(init), ["-- turn started (sonnet) --"]);
        let msg = r#"{"type":"assistant","message":{"content":[{"type":"text","text":" hello "},{"type":"tool_use","name":"Read","input":{"file_path":"/a"}},{"type":"thinking"}]}}"#;
        assert_eq!(render_event(msg), ["hello", "> Read {\"file_path\":\"/a\"}"]);
        assert_eq!(render_event(r#"{"type":"result","is_error":true}"#), ["-- turn failed --"]);
        assert_eq!(render_event(r#"{"type":"result","is_error":false}"#), ["-- turn done --"]);
        assert!(render_event("noise").is_empty() && render_event(r#"{"type":"user"}"#).is_empty());
    }

    #[test]
    fn the_window_command_follows_the_log_and_survives_the_oc_name() {
        let log = Path::new("/r/s 1.log");
        assert_eq!(follow_command(Path::new("/b/codetypera2a"), log), "A2A_STATUS_WINDOW=1 A2A_NO_DISPATCHER=1 '/b/codetypera2a' oc follow '/r/s 1.log'");
        assert!(follow_command(Path::new("/b/oc"), log).contains("'/b/oc' follow "));
    }

    #[test]
    fn a_stream_log_settles_like_the_old_single_result() {
        let log = "{\"type\":\"system\",\"subtype\":\"init\"}\n{\"type\":\"result\",\"is_error\":false,\"result\":\"ok\",\"session_id\":\"s\"}\n";
        assert_eq!(settle(Some(0), log, ""), Settled { failure: None, started: true });
    }

    #[test]
    fn exit_codes_and_error_results_map_to_failures() {
        let ok = r#"{"type":"result","is_error":false,"result":"done","session_id":"s"}"#;
        assert_eq!(settle(Some(0), ok, ""), Settled { failure: None, started: true });
        let bad = r#"{"type":"result","is_error":true,"result":"Credit balance is too low","session_id":"s"}"#;
        assert_eq!(settle(Some(1), bad, "x"), Settled { failure: Some("Credit balance is too low".into()), started: true });
        assert_eq!(settle(Some(1), "", "boom\nmore"), Settled { failure: Some("claude exited with code 1: boom".into()), started: false });
        assert_eq!(settle(Some(0), bad, ""), Settled { failure: Some("Credit balance is too low".into()), started: true });
        assert_eq!(settle(Some(0), "plain text", ""), Settled { failure: None, started: true }, "a clean exit is a success");
        assert_eq!(settle(None, "", ""), Settled { failure: None, started: false }, "killed by a signal (oc cancel): not a failure");
    }
}
