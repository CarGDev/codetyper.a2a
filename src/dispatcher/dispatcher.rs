//! a2a dispatcher: wakes OpenCode sessions when the hub has work for them.
//! No model runs here. It polls the hub database and sends one-line prompts through the OpenCode
//! server API (`opencode api`), then reports back to the requester on state changes.
//!
//! Rules
//!  - wake the assignee when a task is `submitted` (new, or reopened by a reply)
//!  - wake the requester only on input-required | auth-required | completed | failed | rejected
//!  - one nudge per (task, status message): dedup key in the `nudges` table
//!  - one global dispatcher serves every project: the folder comes from the task, never from cwd.
//!    A pidfile (<A2A_DB>.dispatcher.pid) keeps it single-instance; `oc` reuses it.
//!  - every agent works in ITS OWN opencode session, one per agent per project, shown as a tab in
//!    the opencode you already have open. The dispatcher creates the session the first time the
//!    agent gets a task (`session.create`, with the agent set), registers it, and appends a line to
//!    `tabs.jsonl`; the TUI plugin installed by `init` tails that file and opens the tab
//!    (`context.ui.tabs.open`). Later tasks reuse the same session/tab. Results are sent back to
//!    the session the task was sent from (`from_session`, detected by the hub), i.e. the
//!    orchestrator's tab. A session that is running a turn is left alone until it is idle.
//!    If a session cannot be opened the task gets a `no_session` message and the requester is woken.
//!  - hop cap per contextId (agent-to-agent nudges only). At the cap: log once, post one
//!    `hop_cap:` message (input-required) on the newest open task, wake the requester. A requester
//!    reply with "continue" (or `oc continue <contextId>`) grants MAX_HOPS more (table `hop_allow`).
//!  - stall safety net (one retry, then fail the task)
//!
//! Env: A2A_DB, DISPATCH_POLL_MS (3000), DISPATCH_MAX_HOPS (40), DISPATCH_STALL_SECS (300),
//!      DISPATCH_DRY_RUN=1 (log only), OPENCODE_BIN (opencode), A2A_DISPATCHER_PID (pidfile path)
use crate::hub::db::{self, TaskRow, iso_ago_ms, now, parse_ms};
use crate::hub::project::{clear_project_cache, project_root, same_project};
use crate::{opencode, paths, sys};
use regex::Regex;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use uuid::Uuid;

type R<T> = Result<T, String>;

fn e<E: ToString>(err: E) -> String {
    err.to_string()
}

const WAKE_REQUESTER: [&str; 5] = ["input-required", "auth-required", "completed", "failed", "rejected"];

fn log(msg: &str) {
    println!("{} {msg}", now());
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

struct Cfg {
    poll_ms: u64,
    max_hops: i64,
    stall_secs: i64,
    dry: bool,
    auto_open: bool,
    agent_dir: PathBuf,
    pid_file: PathBuf,
}

impl Cfg {
    fn from_env() -> Self {
        Cfg {
            poll_ms: paths::env_u64("DISPATCH_POLL_MS", 3000),
            max_hops: paths::env_u64("DISPATCH_MAX_HOPS", 40) as i64,
            stall_secs: paths::env_u64("DISPATCH_STALL_SECS", 300) as i64,
            dry: std::env::var("DISPATCH_DRY_RUN").as_deref() == Ok("1"),
            auto_open: std::env::var("DISPATCH_AUTO_OPEN").as_deref() != Ok("0"),
            agent_dir: paths::agent_dir(),
            pid_file: paths::dispatcher_pid_path(),
        }
    }
}

enum Lock {
    Held,
    Other(i64),
}

/// Single instance per DB: `Held` when we own the pidfile, else the pid of the running one.
fn acquire_lock(pid_file: &std::path::Path) -> R<Lock> {
    // The pid must already be inside the file when it appears: write a private temp file, then
    // hard-link it into place (atomic, fails if the pidfile exists). Creating the pidfile empty and
    // writing afterwards let a second dispatcher read an empty file, call it stale, delete it and
    // start too: two dispatchers, every task delivered twice.
    let me = std::process::id();
    let tmp = std::path::PathBuf::from(format!("{}.{me}.tmp", pid_file.display()));
    std::fs::write(&tmp, me.to_string()).map_err(e)?;
    let result = (|| {
        for attempt in 0..6 {
            match std::fs::hard_link(&tmp, pid_file) {
                Ok(()) => return Ok(Lock::Held),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    match std::fs::read_to_string(pid_file).ok().and_then(|t| t.trim().parse::<u32>().ok()) {
                        Some(pid) if pid != me && pid_is_dispatcher(pid) => return Ok(Lock::Other(pid as i64)),
                        Some(_) => {
                            let _ = std::fs::remove_file(pid_file); // stale: its owner is gone
                        }
                        // empty or unreadable (older binary mid-write): give its owner a moment
                        None if attempt < 4 => std::thread::sleep(Duration::from_millis(100)),
                        None => {
                            let _ = std::fs::remove_file(pid_file);
                        }
                    }
                }
                Err(err) => return Err(e(err)),
            }
        }
        Ok(Lock::Other(-1))
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

fn pid_is_dispatcher(pid: u32) -> bool {
    if !sys::pid_alive(pid) {
        return false;
    }
    sys::pid_command(pid).is_none_or(|c| sys::is_dispatcher_command(&c))
}

fn release_lock(pid_file: &std::path::Path) {
    let mine = std::fs::read_to_string(pid_file)
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
        == Some(std::process::id());
    if mine {
        let _ = std::fs::remove_file(pid_file);
    }
}

struct Dispatcher {
    conn: Connection,
    cfg: Cfg,
    active: (Option<Instant>, HashSet<String>),
}

impl Dispatcher {
    fn active_sessions(&mut self) -> HashSet<String> {
        if self.active.0.is_some_and(|at| at.elapsed() < Duration::from_millis(1500)) {
            return self.active.1.clone();
        }
        match opencode::active_ids() {
            Ok(ids) => self.active = (Some(Instant::now()), ids),
            Err(err) => log(&format!("active lookup failed: {}", first_line(&err))),
        }
        self.active.1.clone()
    }

    /// The registered, still existing session of `agent` for the project of `dir`.
    fn agent_session(&self, agent: &str, dir: &str) -> R<Option<String>> {
        let rows = db::sessions(&self.conn, "WHERE agent = ? AND status = 'live' ORDER BY created_at DESC", &[agent])?;
        for r in rows.iter().filter(|r| r.directory == dir || same_project(dir, &r.directory)) {
            if opencode::get(&r.session_id).is_some() {
                return Ok(Some(r.session_id.clone()));
            }
            self.conn
                .execute(
                    "UPDATE sessions SET status='stale' WHERE agent=? AND directory=? AND session_id=?",
                    params![r.agent, r.directory, r.session_id],
                )
                .map_err(e)?;
            log(&format!("session {} for {agent} no longer exists; marked stale", r.session_id));
        }
        Ok(None)
    }

    /// Where a result goes: the session the task was sent from, else the requester's own
    /// registered session, else the newest session open in the folder.
    fn requester_session(&self, t: &TaskRow, directory: Option<&str>) -> R<Option<String>> {
        if let Some(id) = t.from_session.as_deref().filter(|s| !s.is_empty()) {
            if opencode::get(id).is_some() {
                return Ok(Some(id.to_string()));
            }
            log(&format!("session {id} of task {} no longer exists", &t.id[..8]));
        }
        let Some(dir) = directory else { return Ok(None) };
        if let Some(id) = self.agent_session(&t.from_agent, dir)? {
            return Ok(Some(id));
        }
        let mut found = opencode::list(dir).into_iter().next();
        if found.is_none() {
            if let Some(root) = project_root(dir).filter(|r| r != dir) {
                found = opencode::list(&root).into_iter().next();
            }
        }
        Ok(found.map(|s| s.id))
    }

    /// The assignee's own session in this project; created (and shown as a tab) the first time.
    fn assignee_session(&mut self, agent: &str, dir: &str) -> Result<String, String> {
        if let Some(id) = self.agent_session(agent, dir)? {
            return Ok(id);
        }
        if !self.cfg.auto_open {
            return Err("opening sessions is disabled (DISPATCH_AUTO_OPEN=0)".into());
        }
        let mut body = json!({ "title": format!("{agent} (a2a)"), "agent": agent, "location": { "directory": dir } });
        // A session keeps its own model and ignores the agent's, so hand it the one opencode resolved
        // for this agent (set with `codetypera2a models` or in the agent file).
        if let Some(model) = opencode::agents(Some(dir)).ok().and_then(|l| l.into_iter().find(|a| a.id == agent)).and_then(|a| a.model) {
            body["model"] = model;
        }
        let r = opencode::create_session(&body).map_err(|err| format!("session create failed: {}", first_line(&err)))?;
        let root = project_root(dir).unwrap_or_else(|| dir.to_string());
        self.conn
            .execute("UPDATE sessions SET status='stale' WHERE agent=? AND directory=?", params![agent, root])
            .map_err(e)?;
        self.conn
            .execute(
                "INSERT INTO sessions (agent, directory, session_id, tmux_target, created_at) VALUES (?,?,?,?,?)",
                params![agent, root, r, Option::<String>::None, now()],
            )
            .map_err(e)?;
        log(&format!("opened session {r} for {agent} in {root}"));
        Ok(r)
    }

    /// Ask the opencode TUI plugin to show `session` as a tab (it tails tabs.jsonl).
    fn request_tab(&self, session: &str, agent: &str, dir: &str) {
        let line = json!({ "sessionID": session, "agent": agent, "directory": dir, "at": now() });
        let path = opencode::tabs_file();
        if let Err(err) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| writeln!(f, "{line}"))
        {
            log(&format!("could not write {}: {err}", path.display()));
        }
    }

    /// Wake `agent` in `session`: switch the session to it, then queue the prompt. `Ok(false)` =
    /// the session is mid-turn; try again on a later tick.
    fn deliver(&mut self, session: &str, agent: &str, text: &str) -> R<bool> {
        if self.active_sessions().contains(session) {
            return Ok(false);
        }
        let info = opencode::get(session).ok_or_else(|| format!("session {session} no longer exists"))?;
        if info.agent.as_deref() != Some(agent) {
            opencode::switch_agent(session, agent)?;
        }
        opencode::prompt(session, text)?;
        Ok(true)
    }

    /// Valid assignee: any agent opencode knows (not only the installed template files). If the
    /// opencode agent list cannot be read, give the benefit of the doubt: session create will say.
    fn agent_known(&self, name: &str, dir: Option<&str>) -> bool {
        if !Regex::new(r"^[A-Za-z0-9._-]+$").unwrap().is_match(name) {
            return false;
        }
        if self.cfg.agent_dir.join(format!("{name}.md")).exists() {
            return true;
        }
        match opencode::agents(dir) {
            Ok(list) => list.iter().any(|a| a.id == name),
            Err(_) => true,
        }
    }

    /// A task without a folder inherits the requester's stored folder, else its newest live session's.
    fn effective_directory(&self, t: &TaskRow) -> R<Option<String>> {
        if let Some(d) = t.directory.as_ref().filter(|d| !d.is_empty()) {
            return Ok(Some(d.clone()));
        }
        if let Some(d) = t.from_directory.as_ref().filter(|d| !d.is_empty()) {
            return Ok(Some(d.clone()));
        }
        let rows = db::sessions(&self.conn, "WHERE agent = ? AND status = 'live' ORDER BY created_at DESC", &[&t.from_agent])?;
        Ok(rows.into_iter().next().map(|r| r.directory))
    }

    /// Only agent-to-agent wake-ups count; no-session / baseline / stall rows do not.
    fn hops(&self, context_id: &str) -> R<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM nudges WHERE context_id = ? AND kind IN ('assignee','requester')",
                [context_id],
                |r| r.get(0),
            )
            .map_err(e)
    }

    fn allowed_hops(&self, context_id: &str) -> i64 {
        let grants: i64 = self
            .conn
            .query_row("SELECT grants FROM hop_allow WHERE context_id = ?", [context_id], |r| r.get(0))
            .unwrap_or(0);
        self.cfg.max_hops * (1 + grants)
    }

    /// Once per cap level: log, and tell the user through the newest open task of the context.
    fn announce_cap(&self, t: &TaskRow, n: i64, allowed: i64) -> R<()> {
        let notified: Option<i64> = self
            .conn
            .query_row("SELECT notified_level FROM hop_allow WHERE context_id = ?", [&t.context_id], |r| r.get(0))
            .ok();
        if notified.is_some_and(|l| l >= allowed) {
            return Ok(());
        }
        self.conn
            .execute(
                "INSERT INTO hop_allow (context_id, notified_level) VALUES (?, ?) ON CONFLICT(context_id) DO UPDATE SET notified_level = excluded.notified_level",
                params![t.context_id, allowed],
            )
            .map_err(e)?;
        log(&format!(
            "hop cap ({allowed}) reached for context {} ({n} hops); work is paused until \"continue\"",
            t.context_id
        ));
        let open = db::tasks(
            &self.conn,
            "WHERE context_id = ? AND state IN ('submitted','working','input-required','auth-required') ORDER BY updated_at DESC",
            &[&t.context_id],
        )?
        .into_iter()
        .next();
        let Some(open) = open else {
            log(&format!(
                "context {}: no open task to carry the hop_cap notice; use: oc continue {}",
                t.context_id, t.context_id
            ));
            return Ok(());
        };
        self.event(&open, "hop_cap", "dispatcher", &format!("{n}/{allowed}"));
        self.post_system_message(
            &open,
            "input-required",
            &format!(
                "hop_cap: context {} reached {n} hops; reply 'continue' to allow {} more, or cancel",
                t.context_id, self.cfg.max_hops
            ),
        )
    }

    /// A requester reply containing "continue" on a capped context raises the allowance by MAX_HOPS.
    fn check_continue(&self) -> R<()> {
        let cont = Regex::new(r"(?i)\bcontinue\b").unwrap();
        let rows = db::tasks(&self.conn, "WHERE state = 'submitted' AND updated_at > ?", &[&iso_ago_ms(86_400_000)])?;
        for t in rows {
            let Ok((notified, last)) = self.conn.query_row(
                "SELECT notified_level, last_continue_msg FROM hop_allow WHERE context_id = ?",
                [&t.context_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)),
            ) else {
                continue;
            };
            if notified < self.allowed_hops(&t.context_id) {
                continue; // not currently capped
            }
            let Ok(m) = serde_json::from_str::<Value>(t.status_message.as_deref().unwrap_or("{}")) else { continue };
            let msg_id = m["messageId"].as_str().unwrap_or("");
            if m["role"] != "user" || m["from"] == "dispatcher" || last.as_deref() == Some(msg_id) {
                continue;
            }
            if !cont.is_match(&status_text(&t)) {
                continue;
            }
            self.conn
                .execute(
                    "UPDATE hop_allow SET grants = grants + 1, last_continue_msg = ? WHERE context_id = ?",
                    params![msg_id, t.context_id],
                )
                .map_err(e)?;
            log(&format!(
                "context {}: \"continue\" received; allowance now {} hops",
                t.context_id,
                self.allowed_hops(&t.context_id)
            ));
        }
        Ok(())
    }

    fn post_system_message(&self, t: &TaskRow, state: &str, text: &str) -> R<()> {
        let msg = json!({
            "messageId": Uuid::new_v4().to_string(), "role": "agent", "from": "dispatcher",
            "parts": [{ "text": text }], "timestamp": now(), "taskId": t.id,
        });
        let mut history: Value = serde_json::from_str(&t.history).map_err(e)?;
        history.as_array_mut().map(|h| h.push(msg.clone()));
        self.conn
            .execute(
                "UPDATE tasks SET state=?, status_message=?, history=?, updated_at=? WHERE id=?",
                params![state, msg.to_string(), history.to_string(), now(), t.id],
            )
            .map_err(e)?;
        self.event(t, "state_change", "dispatcher", state);
        Ok(())
    }

    fn event(&self, t: &TaskRow, kind: &str, agent: &str, detail: &str) {
        db::record_event(&self.conn, Some(&t.id), Some(&t.context_id), kind, Some(agent), Some(detail));
    }

    /// Claim (task, status message, kind) in the shared database. `true` = we own it. The insert is
    /// atomic, so even two dispatchers cannot both deliver the same nudge.
    fn insert_nudge(&self, key: &str, t: &TaskRow, target: &str, kind: &str) -> R<bool> {
        let n = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO nudges (key, task_id, context_id, target, kind, created_at) VALUES (?,?,?,?,?,?)",
                params![key, t.id, t.context_id, target, kind, now()],
            )
            .map_err(e)?;
        Ok(n == 1)
    }

    /// Give a claim back (nothing was delivered), so a later tick tries again.
    fn release_nudge(&self, key: &str) {
        let _ = self.conn.execute("DELETE FROM nudges WHERE key = ?", [key]);
    }

    fn nudge(&mut self, t: &TaskRow, kind: &str, target: &str, directory: Option<&str>) -> R<()> {
        let key = format!("{}:{}:{kind}", t.id, status_message_id(t));
        if db::exists(&self.conn, "SELECT 1 FROM nudges WHERE key = ?", &[&key]) {
            return Ok(());
        }
        let (n, allowed) = (self.hops(&t.context_id)?, self.allowed_hops(&t.context_id));
        if n >= allowed && !status_text(t).starts_with("hop_cap:") {
            return self.announce_cap(t, n, allowed);
        }
        if kind == "assignee" && !self.agent_known(target, directory) {
            if !self.insert_nudge(&key, t, target, "no-agent")? {
                return Ok(()); // another dispatcher already handled it
            }
            self.event(t, "no_agent", target, target);
            log(&format!("task {}: no agent file for \"{target}\"; failing it", &t.id[..8]));
            return self.post_system_message(
                t,
                "failed",
                &format!("no_agent: opencode has no agent named \"{target}\" (see a2a_list_agents)."),
            );
        }
        // Claim before doing anything with side effects (opening a session, prompting it).
        if !self.insert_nudge(&key, t, target, kind)? {
            return Ok(());
        }
        self.event(t, "nudge_claimed", target, kind);
        let session: Result<String, String> = if kind == "assignee" {
            match directory {
                None => Err("the task has no folder".into()),
                Some(d) if self.cfg.dry => match self.agent_session(target, d)? {
                    Some(id) => Ok(id),
                    None => {
                        log(&format!("[dry] would open a new session for {target} in {d}"));
                        return Ok(());
                    }
                },
                Some(d) => self.assignee_session(target, d),
            }
        } else {
            self.requester_session(t, directory)?.ok_or_else(|| "no open session".to_string())
        };
        let session = match session {
            Ok(id) => id,
            Err(why) => {
                self.event(t, "no_session", target, &why);
                let _ = self.conn.execute("UPDATE nudges SET kind = 'no-session' WHERE key = ?", [&key]);
                if kind == "assignee" {
                    let in_dir = directory.map(|d| format!(" in {d}")).unwrap_or_default();
                    log(&format!("no session for {target}{in_dir} ({why}); telling requester {}", t.from_agent));
                    self.post_system_message(
                        t,
                        "input-required",
                        &format!("no_session: could not open a session for \"{target}\"{in_dir}: {why}. Fix that, then reply on this task to retry."),
                    )?;
                } else {
                    log(&format!("no open session for requester {target} ({why}); result stays in the hub (task {})", t.id));
                }
                return Ok(());
            }
        };
        let text = nudge_text(kind, t);
        if self.cfg.dry {
            log(&format!("[dry] nudge {kind} -> {target} (session {session}) task {} state {}", &t.id[..8], t.state));
            return Ok(());
        }
        match self.deliver(&session, target, &text) {
            Ok(true) => {
                self.event(t, "nudge_delivered", target, kind);
                log(&format!("nudge {kind} -> {target} (session {session}) task {} state {}", &t.id[..8], t.state));
                if let (true, Some(d)) = (kind == "assignee", directory) {
                    self.request_tab(&session, target, d);
                }
            }
            Ok(false) => {
                self.event(t, "nudge_released", target, "busy");
                self.release_nudge(&key); // session busy: retry next tick
            }
            Err(err) => {
                self.event(t, "nudge_released", target, "error");
                log(&format!("prompt failed: {}", first_line(&err)));
                self.release_nudge(&key);
            }
        }
        Ok(())
    }

    fn tick(&mut self) -> R<()> {
        clear_project_cache();
        self.check_continue()?;
        let submitted = db::tasks(
            &self.conn,
            "WHERE state = 'submitted' AND to_agent != 'any' AND to_agent != from_agent ORDER BY created_at",
            &[],
        )?;
        for mut t in submitted {
            t.explicit_dir = t.directory.as_deref().is_some_and(|d| !d.is_empty())
                || t.from_directory.as_deref().is_some_and(|d| !d.is_empty());
            let d = self.effective_directory(&t)?;
            t.directory = d.clone();
            let target = t.to_agent.clone();
            self.nudge(&t, "assignee", &target, d.as_deref())?;
        }
        let since = iso_ago_ms(86_400_000);
        let mut params: Vec<&str> = WAKE_REQUESTER.to_vec();
        params.push(&since);
        let finished = db::tasks(&self.conn, "WHERE state IN (?,?,?,?,?) AND updated_at > ? ORDER BY updated_at", &params)?;
        for mut t in finished {
            if t.from_agent == t.to_agent {
                continue;
            }
            t.explicit_dir = t.directory.as_deref().is_some_and(|d| !d.is_empty())
                || t.from_directory.as_deref().is_some_and(|d| !d.is_empty());
            let d = self.effective_directory(&t)?;
            t.directory = d.clone();
            let target = t.from_agent.clone();
            self.nudge(&t, "requester", &target, d.as_deref())?;
        }
        self.stall_check()
    }

    fn stall_check(&mut self) -> R<()> {
        let open = db::tasks(&self.conn, "WHERE state IN ('submitted','working') AND to_agent != 'any'", &[])?;
        let active = self.active_sessions();
        for t in open {
            let last: Option<String> = self
                .conn
                .query_row(
                    "SELECT created_at FROM nudges WHERE task_id = ? AND kind = 'assignee' ORDER BY created_at DESC LIMIT 1",
                    [&t.id],
                    |r| r.get(0),
                )
                .ok();
            let Some(last) = last else { continue };
            let age_s = (chrono::Utc::now().timestamp_millis() - parse_ms(&last)) as f64 / 1000.0;
            if age_s < self.cfg.stall_secs as f64 {
                continue;
            }
            let session = match t.directory.as_deref() {
                Some(d) => self.agent_session(&t.to_agent, d)?,
                None => None,
            };
            if session.as_ref().is_some_and(|s| active.contains(s)) {
                continue; // still working
            }
            let retried = db::exists(&self.conn, "SELECT 1 FROM nudges WHERE task_id = ? AND kind = 'stall-retry'", &[&t.id]);
            if !retried {
                log(&format!("stalled task {} ({}); retrying once", &t.id[..8], t.to_agent));
                let _ = self.insert_nudge(&format!("{}:stall-retry", t.id), &t, &t.to_agent, "stall-retry")?;
                self.event(&t, "stall_retry", &t.to_agent, &format!("{}s", age_s.round()));
                if let (false, Some(s)) = (self.cfg.dry, &session) {
                    if let Err(err) = self.deliver(s, &t.to_agent, &nudge_text("assignee", &t)) {
                        log(&format!("retry failed: {}", first_line(&err)));
                    }
                }
            } else if age_s > (self.cfg.stall_secs * 2) as f64 {
                log(&format!("task {} still stalled; marking failed", &t.id[..8]));
                self.conn
                    .execute(
                        "INSERT OR IGNORE INTO nudges (key, task_id, context_id, target, kind, created_at) VALUES (?,?,?,?,?,?)",
                        params![format!("{}:stall-failed", t.id), t.id, t.context_id, t.to_agent, "stall-failed", now()],
                    )
                    .map_err(e)?;
                self.event(&t, "stall_failed", &t.to_agent, &format!("{}s", age_s.round()));
                self.post_system_message(
                    &t,
                    "failed",
                    &format!("stalled: {} did not report after {}s and one retry.", t.to_agent, age_s.round()),
                )?;
            }
        }
        Ok(())
    }

    /// On start, treat every already-finished task as handled so old results never wake anyone.
    fn baseline(&self) -> R<()> {
        let rows = db::tasks(&self.conn, "WHERE state IN (?,?,?,?,?)", &WAKE_REQUESTER)?;
        for t in &rows {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO nudges (key, task_id, context_id, target, kind, created_at) VALUES (?,?,?,?,?,?)",
                    params![format!("{}:{}:requester", t.id, status_message_id(t)), t.id, t.context_id, t.from_agent, "baseline", now()],
                )
                .map_err(e)?;
        }
        log(&format!("baseline: {} finished task(s) marked as already handled", rows.len()));
        Ok(())
    }
}

fn status_message_id(t: &TaskRow) -> String {
    serde_json::from_str::<Value>(t.status_message.as_deref().unwrap_or("{}"))
        .ok()
        .and_then(|m| m["messageId"].as_str().filter(|s| !s.is_empty()).map(String::from))
        .unwrap_or_else(|| t.updated_at.clone())
}

fn status_text(t: &TaskRow) -> String {
    serde_json::from_str::<Value>(t.status_message.as_deref().unwrap_or("{}"))
        .ok()
        .and_then(|m| m["parts"].as_array().cloned())
        .map(|ps| ps.iter().map(|p| p["text"].as_str().unwrap_or("")).collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

fn nudge_text(kind: &str, t: &TaskRow) -> String {
    if kind == "assignee" {
        let proj = t.directory.as_deref().map(|d| format!(" (project {d})")).unwrap_or_default();
        let dir = t.directory.as_deref().map(|d| format!(", directory:\"{d}\"")).unwrap_or_default();
        return format!(
            "A2A: you are agent \"{to}\". Task {id} is waiting for you{proj}. Call a2a_inbox {{agent:\"{to}\"{dir}}}, claim the task with a2a_update_task state \"working\", do the work, then finish with a2a_update_task state completed | input-required | failed and put the deliverable in an artifact.",
            to = t.to_agent, id = t.id
        );
    }
    format!(
        "A2A: task {id} that you sent to \"{to}\" is now {state}. Call a2a_get_task {{id:\"{id}\"}}, judge the result, then continue your plan, answer the question with a2a_send_message {{taskId}}, or ask the user if it needs their decision.",
        id = t.id, to = t.to_agent, state = t.state
    )
}

/// Make sure a dispatcher is running, starting one detached (own process group, logging to a
/// file) rather than opening another window or pane. `Some(pid)` = started now, `None` = was
/// already running. Used by the hub (so opencode alone is enough) and by `oc`.
pub fn ensure_running() -> Result<Option<u32>, String> {
    if sys::dispatcher_pid(&paths::dispatcher_pid_path()).is_some() {
        return Ok(None);
    }
    let exe = std::env::current_exe().map_err(e)?;
    sys::spawn_detached(&exe, &["dispatcher"], &log_path()).map(Some).map_err(e)
}

pub fn log_path() -> PathBuf {
    let mut log = paths::db_path().into_os_string();
    log.push(".dispatcher.log");
    PathBuf::from(log)
}

/// Inside tmux, keep a detached `a2a-status` window following the dispatcher live
/// (`oc watch`), so the A2A traffic is visible without taking over your pane.
pub fn ensure_status_window() -> Result<sys::Tab, String> {
    let exe = std::env::current_exe().map_err(e)?;
    sys::tmux_ensure_window("a2a-status", &format!("{} oc watch", sys::sh(&exe.display().to_string())))
}

pub fn run() -> i32 {
    let cfg = Cfg::from_env();
    let pid_file = cfg.pid_file.clone();
    match acquire_lock(&pid_file) {
        Ok(Lock::Held) => {}
        Ok(Lock::Other(pid)) => {
            log(&format!("another dispatcher is already running (pid {pid}, {}); exiting", pid_file.display()));
            return 0;
        }
        Err(err) => {
            eprintln!("dispatcher: {err}");
            return 1;
        }
    }
    let pf = pid_file.clone();
    let _ = ctrlc::set_handler(move || {
        release_lock(&pf);
        std::process::exit(0);
    });
    let db_path = paths::db_path();
    let conn = match db::open(&db_path) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("dispatcher: {err}");
            release_lock(&pid_file);
            return 1;
        }
    };
    let mut d = Dispatcher { conn, cfg, active: (None, HashSet::new()) };
    log(&format!(
        "dispatcher up: db={} poll={}ms maxHops={} stall={}s agents={}{}",
        db_path.display(), d.cfg.poll_ms, d.cfg.max_hops, d.cfg.stall_secs, d.cfg.agent_dir.display(),
        if d.cfg.dry { " DRY-RUN" } else { "" }
    ));
    if std::env::var("DISPATCH_NO_BASELINE").as_deref() != Ok("1") {
        if let Err(err) = d.baseline() {
            log(&format!("baseline failed: {err}"));
            release_lock(&pid_file);
            return 1;
        }
    }
    loop {
        if let Err(err) = d.tick() {
            log(&format!("tick error: {err}"));
        }
        std::thread::sleep(Duration::from_millis(d.cfg.poll_ms));
    }
}
