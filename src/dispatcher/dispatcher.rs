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
//!      DISPATCH_DRY_RUN=1 (log only), OPENCODE_BIN (opencode), A2A_DISPATCHER_PID (pidfile path),
//!      DISPATCH_BACKEND (opencode | claude; default opencode), DISPATCH_BACKEND_<AGENT> (one agent's
//!      backend; AGENT upper-case, every non-alphanumeric character `_`), CLAUDE_BIN (claude)
use super::backend::{self, Backend, SessionState};
use crate::hub::db::{self, TaskRow, iso_ago_ms, now, parse_ms};
use crate::hub::project::{clear_project_cache, project_root, same_project};
use crate::{opencode, paths, sys};
use regex::Regex;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use uuid::Uuid;

type R<T> = Result<T, String>;

fn e<E: ToString>(err: E) -> String {
    err.to_string()
}

const WAKE_REQUESTER: [&str; 5] = [
    "input-required",
    "auth-required",
    "completed",
    "failed",
    "rejected",
];

pub(super) fn log(msg: &str) {
    println!("{} {msg}", now());
}

/// A delivery is given up after this many attempts.
const MAX_ATTEMPTS: i64 = 5;

/// Delay before attempt `attempt + 1` (attempt counts from 1): base, base*3, base*9, ... max 2 min.
fn retry_delay_ms(attempt: i64, base_ms: u64) -> i64 {
    (base_ms as i64)
        .saturating_mul(3_i64.saturating_pow((attempt - 1).max(0) as u32))
        .min(120_000.max(base_ms as i64))
}

pub(super) fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

/// Which backend runs which agent: `DISPATCH_BACKEND` is the default, `DISPATCH_BACKEND_<AGENT>`
/// overrides it for one agent. Names the dispatcher does not know are ignored.
#[derive(Debug, PartialEq)]
struct Choice {
    default: &'static str,
    global: Option<&'static str>,
    per_agent: HashMap<String, &'static str>,
}

const DEFAULT_BACKEND: &str = "opencode";
const BACKEND_VAR: &str = "DISPATCH_BACKEND";

/// `mentor-2` -> `MENTOR_2`: how an agent name is spelled in an environment variable.
fn env_key(agent: &str) -> String {
    agent
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn known_backend(name: &str) -> Option<&'static str> {
    backend::NAMES.iter().copied().find(|n| *n == name.trim())
}

impl Choice {
    fn from_vars(vars: impl Iterator<Item = (String, String)>) -> Choice {
        let (mut default, mut global, mut per_agent) = (DEFAULT_BACKEND, None, HashMap::new());
        for (key, value) in vars {
            let Some(name) = known_backend(&value) else {
                continue;
            };
            match key.strip_prefix(BACKEND_VAR) {
                Some("") => {
                    default = name;
                    global = Some(name);
                }
                Some(agent) if agent.starts_with('_') => {
                    per_agent.insert(agent[1..].to_ascii_uppercase(), name);
                }
                _ => {}
            }
        }
        Choice {
            default,
            global,
            per_agent,
        }
    }

    fn of(&self, agent: &str) -> &'static str {
        self.per_agent
            .get(&env_key(agent))
            .copied()
            .unwrap_or(self.default)
    }

    /// Configuration seam for automatic routing: explicit values are kept separate from the
    /// legacy OpenCode default so the resolver can apply identity only when no override exists.
    pub(super) fn overrides(&self, agent: &str) -> (Option<&str>, Option<&str>) {
        let per_agent = self.per_agent.get(&env_key(agent)).copied();
        (per_agent, self.global)
    }

    /// Every backend that has to be running.
    fn used(&self) -> BTreeSet<&'static str> {
        self.per_agent
            .values()
            .copied()
            .chain([self.default])
            .collect()
    }
}

struct Cfg {
    choice: Choice,
    poll_ms: u64,
    max_hops: i64,
    stall_secs: i64,
    dry: bool,
    auto_open: bool,
    agent_dir: PathBuf,
    pid_file: PathBuf,
    /// First retry delay for a failed delivery; grows x3 per attempt (see `retry_delay_ms`).
    retry_base_ms: u64,
}

impl Cfg {
    fn from_env() -> Self {
        Cfg {
            choice: Choice::from_vars(std::env::vars()),
            poll_ms: paths::env_u64("DISPATCH_POLL_MS", 3000),
            max_hops: paths::env_u64("DISPATCH_MAX_HOPS", 40) as i64,
            stall_secs: paths::env_u64("DISPATCH_STALL_SECS", 300) as i64,
            dry: std::env::var("DISPATCH_DRY_RUN").as_deref() == Ok("1"),
            auto_open: std::env::var("DISPATCH_AUTO_OPEN").as_deref() != Ok("0"),
            agent_dir: paths::agent_dir(),
            pid_file: paths::dispatcher_pid_path(),
            retry_base_ms: paths::env_u64("DISPATCH_RETRY_BASE_MS", 5000),
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
                    match std::fs::read_to_string(pid_file)
                        .ok()
                        .and_then(|t| t.trim().parse::<u32>().ok())
                    {
                        Some(pid) if pid != me && pid_is_dispatcher(pid) => {
                            return Ok(Lock::Other(pid as i64));
                        }
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
    unregister_self();
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
    backends: Vec<Box<dyn Backend>>,
}

impl Dispatcher {
    /// The backend that runs `agent`'s sessions.
    fn backend_for(&self, agent: &str) -> &dyn Backend {
        let name = self.cfg.choice.of(agent);
        self.backends
            .iter()
            .find(|b| b.name() == name)
            .unwrap_or(&self.backends[0])
            .as_ref()
    }

    /// A task's context routing is stable after creation; environment changes must not move it.
    fn task_backend_name(&self, t: &TaskRow, agent: &str) -> &'static str {
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT metadata FROM contexts WHERE id = ?",
                [&t.context_id],
                |r| r.get(0),
            )
            .ok();
        match stored_backend_name(stored.as_deref()) {
            Ok(Some(name)) => name,
            Ok(None) => self.cfg.choice.of(agent),
            Err(()) => {
                log("invalid persisted backend; using safe OpenCode routing");
                "opencode"
            }
        }
    }

    fn backend_for_task(&self, t: &TaskRow, agent: &str) -> &dyn Backend {
        let name = self.task_backend_name(t, agent);
        self.backends
            .iter()
            .find(|b| b.name() == name)
            .map(|b| b.as_ref())
            .unwrap_or_else(|| self.backend_for(agent))
    }

    /// Sessions running a turn in any backend in use (cached for 1.5 s). A backend that cannot be
    /// asked keeps the previous answer.
    fn active_sessions(&mut self) -> HashSet<String> {
        if self
            .active
            .0
            .is_some_and(|at| at.elapsed() < Duration::from_millis(1500))
        {
            return self.active.1.clone();
        }
        // Only ask a runtime that has open work: a Claude-only install never touches opencode.
        let needed = needed_backends(&self.conn);
        let asked: R<Vec<HashSet<String>>> = self
            .backends
            .iter()
            .filter(|b| needed.contains(b.name()))
            .map(|b| b.active_ids())
            .collect();
        match asked {
            Ok(sets) => self.active = (Some(Instant::now()), sets.into_iter().flatten().collect()),
            Err(err) => log(&format!("active lookup failed: {}", first_line(&err))),
        }
        self.active.1.clone()
    }

    /// The registered, still existing session of `agent` for the project of `dir`.
    fn agent_session(&self, agent: &str, dir: &str) -> R<Option<String>> {
        self.agent_session_backend(self.backend_for(agent), agent, dir)
    }

    fn agent_session_with(&self, t: &TaskRow, agent: &str, dir: &str) -> R<Option<String>> {
        self.agent_session_backend(self.backend_for_task(t, agent), agent, dir)
    }

    fn agent_session_backend(
        &self,
        backend: &dyn Backend,
        agent: &str,
        dir: &str,
    ) -> R<Option<String>> {
        let rows = db::sessions(
            &self.conn,
            "WHERE agent = ? AND status = 'live' ORDER BY created_at DESC",
            &[agent],
        )?;
        let mine = rows
            .iter()
            .filter(|r| r.backend.as_deref() == backend.stored_name());
        for r in mine.filter(|r| r.directory == dir || same_project(dir, &r.directory)) {
            let state = backend.session_state(&r.session_id, &r.directory);
            if state == SessionState::Usable {
                return Ok(Some(r.session_id.clone()));
            }
            self.conn
                .execute(
                    "UPDATE sessions SET status='stale' WHERE agent=? AND directory=? AND session_id=?",
                    params![r.agent, r.directory, r.session_id],
                )
                .map_err(e)?;
            let why = if state == SessionState::FolderGone {
                "its folder no longer exists"
            } else {
                "it no longer exists"
            };
            log(&format!(
                "stale {} session for agent {agent} ({why}); marked stale",
                backend.name()
            ));
        }
        Ok(None)
    }

    /// Where a result goes: the session the task was sent from, else the requester's own
    /// registered session, else the newest session open in the folder.
    fn requester_session(&self, t: &TaskRow, directory: Option<&str>) -> R<Option<String>> {
        let backend = self.backend_for_task(t, &t.from_agent);
        if let Some(id) = t.from_session.as_deref().filter(|s| !s.is_empty()) {
            let rows = db::sessions(
                &self.conn,
                "WHERE agent = ? AND session_id = ? AND status = 'live'",
                &[&t.from_agent, id],
            )?;
            if rows.iter().any(|r| {
                r.backend.as_deref() == backend.stored_name()
                    && directory.is_some_and(|d| d == r.directory || same_project(d, &r.directory))
            }) || (backend.name() == "opencode" && backend.session_matches(id, directory))
            {
                if let Some(found) = backend
                    .sent_from(id)
                    .or_else(|| Some(id.to_owned()).filter(|_| backend.name() == "claude"))
                {
                    return Ok(Some(found));
                }
            }
            log(&format!(
                "stored requester session mismatch for task {}",
                &t.id[..8]
            ));
        }
        let Some(dir) = directory else {
            return Ok(None);
        };
        if let Some(id) = self.agent_session_with(t, &t.from_agent, dir)? {
            return Ok(Some(id));
        }
        Ok(backend.newest_in(dir))
    }

    /// The assignee's own session in this project; created (and shown as a tab) the first time.
    fn assignee_session(&mut self, t: &TaskRow, agent: &str, dir: &str) -> Result<String, String> {
        if let Some(id) = self.agent_session_with(t, agent, dir)? {
            return Ok(id);
        }
        if agent == "orchestrator" {
            // the orchestrator is the user's own session; results return to it, and the dispatcher
            // must never open a second one
            return Err(format!(
                "no open orchestrator session in {dir}; open `oc` in that folder (the dispatcher never starts an orchestrator)"
            ));
        }
        if !self.cfg.auto_open {
            return Err("opening sessions is disabled (DISPATCH_AUTO_OPEN=0)".into());
        }
        let backend = self.backend_for_task(t, agent);
        let r = backend
            .create_session(agent, dir)
            .map_err(|err| format!("session create failed: {}", first_line(&err)))?;
        let root = project_root(dir).unwrap_or_else(|| dir.to_string());
        self.conn
            .execute(
                "UPDATE sessions SET status='stale' WHERE agent=? AND directory=?",
                params![agent, root],
            )
            .map_err(e)?;
        db::ensure_agent(&self.conn, agent).map_err(e)?;
        self.conn
            .execute(
                "INSERT INTO sessions (agent, directory, session_id, tmux_target, backend, created_at, last_seen_at) VALUES (?,?,?,?,?,?,?)",
                params![agent, root, r, Option::<String>::None, backend.stored_name(), now(), now()],
            )
            .map_err(e)?;
        log(&format!("opened session {r} for {agent} in {root}"));
        Ok(r)
    }

    /// Wake `agent` in `session`: switch the session to it, then queue the prompt. `Ok(false)` =
    /// the session is mid-turn; try again on a later tick.
    fn deliver_named(
        &mut self,
        name: &str,
        session: &str,
        agent: &str,
        dir: Option<&str>,
        text: &str,
    ) -> R<bool> {
        if self.active_sessions().contains(session) {
            return Ok(false);
        }
        let backend = self
            .backends
            .iter()
            .find(|b| b.name() == name)
            .map(|b| b.as_ref());
        // With DISPATCH_ONLY set, a session of the other runtime is never woken (no fallback).
        let Some(backend) = backend.or_else(|| match backend::only() {
            Some(_) => None,
            None => Some(self.backend_for(agent)),
        }) else {
            return Err(format!("backend {name} is disabled by DISPATCH_ONLY"));
        };
        backend.send(session, agent, dir, text)
    }

    fn deliver(&mut self, session: &str, agent: &str, dir: Option<&str>, text: &str) -> R<bool> {
        let name = self.cfg.choice.of(agent);
        self.deliver_named(name, session, agent, dir, text)
    }

    /// Valid assignee: a name of the right shape that the agent's backend knows.
    fn agent_known(&self, t: &TaskRow, name: &str, dir: Option<&str>) -> bool {
        Regex::new(r"^[A-Za-z0-9._-]+$").unwrap().is_match(name)
            && self.backend_for_task(t, name).agent_known(name, dir)
    }

    /// A task without a folder inherits the requester's stored folder, else its newest live session's.
    fn effective_directory(&self, t: &TaskRow) -> R<Option<String>> {
        if let Some(d) = t.directory.as_ref().filter(|d| !d.is_empty()) {
            return Ok(Some(d.clone()));
        }
        if let Some(d) = t.from_directory.as_ref().filter(|d| !d.is_empty()) {
            return Ok(Some(d.clone()));
        }
        let rows = db::sessions(
            &self.conn,
            "WHERE agent = ? AND status = 'live' ORDER BY created_at DESC",
            &[&t.from_agent],
        )?;
        Ok(rows.into_iter().next().map(|r| r.directory))
    }

    /// Only agent-to-agent wake-ups count; no-session / baseline / stall rows do not.
    fn hops(&self, context_id: &str) -> R<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM nudges WHERE context_id = ? AND kind IN ('assignee','requester') AND status IN ('claimed','delivered')",
                [context_id],
                |r| r.get(0),
            )
            .map_err(e)
    }

    fn allowed_hops(&self, context_id: &str) -> i64 {
        let grants: i64 = self
            .conn
            .query_row(
                "SELECT grants FROM hop_allow WHERE context_id = ?",
                [context_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        self.cfg.max_hops * (1 + grants)
    }

    /// Once per cap level: log, and tell the user through the newest open task of the context.
    fn announce_cap(&self, t: &TaskRow, n: i64, allowed: i64) -> R<()> {
        let notified: Option<i64> = self
            .conn
            .query_row(
                "SELECT notified_level FROM hop_allow WHERE context_id = ?",
                [&t.context_id],
                |r| r.get(0),
            )
            .ok();
        if notified.is_some_and(|l| l >= allowed) {
            return Ok(());
        }
        self.conn
            .execute(
                "INSERT INTO hop_allow (context_id, notified_level, updated_at) VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%fZ','now')) ON CONFLICT(context_id) DO UPDATE SET notified_level = excluded.notified_level, updated_at = excluded.updated_at",
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
        let rows = db::tasks(
            &self.conn,
            "WHERE state = 'submitted' AND updated_at > ?",
            &[&iso_ago_ms(86_400_000)],
        )?;
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
            let Ok(m) = serde_json::from_str::<Value>(t.status_message.as_deref().unwrap_or("{}"))
            else {
                continue;
            };
            let msg_id = m["messageId"].as_str().unwrap_or("");
            if m["role"] != "user" || m["from"] == "dispatcher" || last.as_deref() == Some(msg_id) {
                continue;
            }
            if !cont.is_match(&status_text(&t)) {
                continue;
            }
            self.conn
                .execute(
                    "UPDATE hop_allow SET grants = grants + 1, last_continue_msg = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE context_id = ?",
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
                "UPDATE tasks SET state=?, status_message=?, history=?, updated_at=?, version=version+1, completed_at=CASE WHEN ? IN ('completed','failed','canceled','rejected') THEN ? END WHERE id=?",
                params![state, msg.to_string(), history.to_string(), now(), state, now(), t.id],
            )
            .map_err(e)?;
        self.event(t, "state_change", "dispatcher", state);
        Ok(())
    }

    fn event(&self, t: &TaskRow, kind: &str, agent: &str, detail: &str) {
        db::record_event(
            &self.conn,
            Some(&t.id),
            Some(&t.context_id),
            kind,
            Some(agent),
            Some(detail),
        );
    }

    /// Claim (task, status message, kind) in the shared database. `true` = we own it. The insert is
    /// atomic, so even two dispatchers cannot both deliver the same nudge. A released nudge
    /// (`pending`, due) or one whose claimant died (`claimed`, lease expired) can be claimed again.
    fn insert_nudge(&self, key: &str, t: &TaskRow, target: &str, kind: &str) -> R<bool> {
        let (ts, lease) = (now(), iso_after_ms(NUDGE_LEASE_MS));
        let owner = format!("dispatcher-{}", std::process::id());
        db::ensure_context(&self.conn, &t.context_id).map_err(e)?;
        let n = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO nudges (key, task_id, context_id, target, kind, status, attempts, claimed_by, claim_expires_at, created_at, updated_at) VALUES (?,?,?,?,?,'claimed',1,?,?,?,?)",
                params![key, t.id, t.context_id, target, kind, owner, lease, ts, ts],
            )
            .map_err(e)?;
        if n == 1 {
            return Ok(true);
        }
        let n = self
            .conn
            .execute(
                "UPDATE nudges SET status='claimed', attempts=attempts+1, claimed_by=?, claim_expires_at=?, updated_at=?
                 WHERE key=? AND ((status='pending' AND (next_attempt_at IS NULL OR next_attempt_at <= ?)) OR (status='claimed' AND claim_expires_at < ?))",
                params![owner, lease, ts, key, ts, ts],
            )
            .map_err(e)?;
        Ok(n == 1)
    }

    /// Move a claimed nudge to its outcome: `delivered`, `dead` (nothing to deliver to) or `pending`
    /// (try again in `retry_in_ms`, with the reason in `last_error`).
    fn finish_nudge(&self, key: &str, status: &str, retry_in_ms: i64, err: Option<&str>) {
        let ts = now();
        let _ = self.conn.execute(
            "UPDATE nudges SET status=?, updated_at=?, delivered_at=CASE WHEN ?='delivered' THEN ? END, next_attempt_at=CASE WHEN ?='pending' THEN ? END, last_error=?, claimed_by=NULL, claim_expires_at=NULL WHERE key=?",
            params![status, ts, status, ts, status, iso_after_ms(retry_in_ms), err, key],
        );
    }

    /// True while the nudge must not be touched: delivered, dead, or claimed with a live lease or
    /// released but not yet due.
    fn nudge_busy(&self, key: &str) -> bool {
        let ts = now();
        db::exists(
            &self.conn,
            "SELECT 1 FROM nudges WHERE key = ? AND NOT ((status='pending' AND (next_attempt_at IS NULL OR next_attempt_at <= ?)) OR (status='claimed' AND claim_expires_at < ?))",
            &[key, &ts, &ts],
        )
    }

    fn nudge(&mut self, t: &TaskRow, kind: &str, target: &str, directory: Option<&str>) -> R<()> {
        let backend_name = self.task_backend_name(t, target);
        let key = format!("{}:{}:{kind}", t.id, status_message_id(t));
        if self.nudge_busy(&key) {
            return Ok(());
        }
        let (n, allowed) = (self.hops(&t.context_id)?, self.allowed_hops(&t.context_id));
        if n >= allowed && !status_text(t).starts_with("hop_cap:") {
            return self.announce_cap(t, n, allowed);
        }
        if kind == "assignee" && !self.agent_known(t, target, directory) {
            if !self.insert_nudge(&key, t, target, "no-agent")? {
                return Ok(()); // another dispatcher already handled it
            }
            self.finish_nudge(&key, "dead", 0, Some("no agent"));
            self.event(t, "no_agent", target, target);
            log(&format!(
                "task {}: no agent file for \"{target}\"; failing it",
                &t.id[..8]
            ));
            return self.post_system_message(
                t,
                "failed",
                &format!(
                    "no_agent: {} has no agent named \"{target}\" (see a2a_list_agents).",
                    backend_name
                ),
            );
        }
        // Claim before doing anything with side effects (opening a session, prompting it).
        if !self.insert_nudge(&key, t, target, kind)? {
            return Ok(());
        }
        self.event(t, "nudge_claimed", target, kind);
        if kind == "requester" {
            self.settle_usage(t);
        }
        if let (true, Some(d)) = (kind == "assignee", directory) {
            if !std::path::Path::new(d).is_dir() {
                self.event(t, "directory_missing", target, d);
                self.finish_nudge(&key, "dead", 0, Some("directory missing"));
                log(&format!(
                    "task {}: folder {d} does not exist; failing it",
                    &t.id[..8]
                ));
                return self.post_system_message(
                    t,
                    "failed",
                    &format!("directory_missing: {d} does not exist (was the project moved?). Send the task again with the new folder."),
                );
            }
        }
        let session: Result<String, String> = if kind == "assignee" {
            match directory {
                None => Err("the task has no folder".into()),
                Some(d) if self.cfg.dry => match self.agent_session_with(t, target, d)? {
                    Some(id) => Ok(id),
                    None => {
                        log(&format!(
                            "[dry] would open a new session for {target} in {d}"
                        ));
                        self.finish_nudge(&key, "delivered", 0, None);
                        return Ok(());
                    }
                },
                Some(d) => self.assignee_session(t, target, d),
            }
        } else {
            self.requester_session(t, directory)?
                .ok_or_else(|| "no open session".to_string())
        };
        let session = match session {
            Ok(id) => id,
            Err(why) => {
                self.event(t, "no_session", target, &why);
                let _ = self.conn.execute(
                    "UPDATE nudges SET kind = 'no-session' WHERE key = ?",
                    [&key],
                );
                self.finish_nudge(&key, "dead", 0, Some(&why));
                if kind == "assignee" {
                    let in_dir = directory.map(|d| format!(" in {d}")).unwrap_or_default();
                    log(&format!(
                        "no session for {target}{in_dir} ({why}); telling requester {}",
                        t.from_agent
                    ));
                    self.post_system_message(
                        t,
                        "input-required",
                        &format!("no_session: could not open a session for \"{target}\"{in_dir}: {why}. Fix that, then reply on this task to retry."),
                    )?;
                } else {
                    log(&format!(
                        "no open session for requester {target} ({why}); result stays in the hub (task {})",
                        t.id
                    ));
                }
                return Ok(());
            }
        };
        let style = self
            .backends
            .iter()
            .find(|b| b.name() == backend_name)
            .map(|b| b.as_ref())
            .unwrap_or_else(|| self.backend_for(target))
            .style();
        let text = nudge_text(kind, t, &style);
        if self.cfg.dry {
            log(&format!(
                "[dry] nudge {kind} -> {target} (session {session}) task {} state {}",
                &t.id[..8],
                t.state
            ));
            self.finish_nudge(&key, "delivered", 0, None);
            return Ok(());
        }
        match self.deliver_named(backend_name, &session, target, directory, &text) {
            Ok(true) => {
                self.finish_nudge(&key, "delivered", 0, None);
                if kind == "assignee" {
                    self.usage_baseline(t, target, &session);
                }
                self.event(t, "nudge_delivered", target, kind);
                log(&format!(
                    "nudge {kind} -> {target} (session {session}) task {} state {}",
                    &t.id[..8],
                    t.state
                ));
                if let (true, Some(d)) = (kind == "assignee", directory) {
                    if let Some(backend) = self.backends.iter().find(|b| b.name() == backend_name) {
                        backend.request_tab(&session, target, d);
                    }
                }
            }
            Ok(false) => {
                self.event(t, "nudge_released", target, "busy");
                self.finish_nudge(&key, "pending", 0, Some("session busy")); // retry next tick
            }
            Err(err) => self.delivery_failed(t, kind, target, &session, &key, &err)?,
        }
        Ok(())
    }

    /// A prompt could not be delivered: retry later with growing waits, or give up after
    /// MAX_ATTEMPTS. A 404 on the first attempt means the session is unusable (folder gone): it is
    /// retired and the retry is immediate, so the next try gets a fresh one.
    fn delivery_failed(
        &self,
        t: &TaskRow,
        kind: &str,
        target: &str,
        session: &str,
        key: &str,
        err: &str,
    ) -> R<()> {
        let attempt: i64 = self
            .conn
            .query_row("SELECT attempts FROM nudges WHERE key = ?", [key], |r| {
                r.get(0)
            })
            .unwrap_or(1);
        let why = first_line(err);
        log(&format!(
            "prompt failed (task {}, session {session}, attempt {attempt}/{MAX_ATTEMPTS}): {why}",
            &t.id[..8]
        ));
        let retire = why.contains("404") && attempt == 1;
        let _ = retire.then(|| {
            self.conn.execute(
                "UPDATE sessions SET status='stale' WHERE session_id = ?",
                [session],
            )
        });
        if attempt < MAX_ATTEMPTS {
            self.event(t, "nudge_released", target, "error");
            let wait = (!retire)
                .then(|| retry_delay_ms(attempt, self.cfg.retry_base_ms))
                .unwrap_or(0);
            self.finish_nudge(key, "pending", wait, Some(why));
            return Ok(());
        }
        self.give_up(t, kind, target, key, why)
    }

    /// Delivery is abandoned. Only an assignee nudge is reported on the task; a missed wake-up of
    /// the requester leaves the result in the hub.
    fn give_up(&self, t: &TaskRow, kind: &str, target: &str, key: &str, why: &str) -> R<()> {
        self.event(t, "nudge_dead", target, why);
        self.finish_nudge(key, "dead", 0, Some(why));
        log(&format!(
            "giving up on {target} for task {} ({kind})",
            &t.id[..8]
        ));
        let text = format!(
            "delivery_failed: could not hand the task to \"{target}\" after {MAX_ATTEMPTS} attempts ({why}). Fix that, then reply on this task to retry."
        );
        (kind == "assignee").then_some(text).map_or(Ok(()), |m| {
            self.post_system_message(t, "input-required", &m)
        })
    }

    /// Remember the assignee session's cumulative usage when it is handed the task, so what it
    /// spends on the task can be told apart from what the (reused) session spent before.
    fn usage_baseline(&self, t: &TaskRow, agent: &str, session: &str) {
        // a backend without usage figures (Claude: the transcript ingester counts those turns)
        // records no baseline, so nothing is settled for it either
        let Some(u) = self.backend_for_task(t, agent).usage(session) else {
            return;
        };
        let detail =
            json!({ "session": session, "cum": u.to_json(), "model": u.model }).to_string();
        db::record_event(
            &self.conn,
            Some(&t.id),
            Some(&t.context_id),
            "usage_baseline",
            Some(agent),
            Some(&detail),
        );
    }

    /// Before the requester is woken: record what the assignee's session spent on this task since
    /// the last baseline/usage event, as one `usage` event (tokens, and cost when it can be known).
    /// Best effort: never fails a tick.
    fn settle_usage(&self, t: &TaskRow) {
        let prev: Option<String> = self
            .conn
            .query_row(
                "SELECT detail FROM events WHERE task_id = ? AND kind IN ('usage_baseline','usage') ORDER BY id DESC LIMIT 1",
                [&t.id],
                |r| r.get(0),
            )
            .ok();
        let Some(prev) = prev.and_then(|d| serde_json::from_str::<Value>(&d).ok()) else {
            return;
        };
        let Some(session) = prev["session"].as_str() else {
            return;
        };
        let Some(cur) = self.backend_for_task(t, &t.to_agent).usage(session) else {
            return;
        };
        let delta = cur.since(&opencode::Usage::from_json(&prev["cum"], None));
        if delta.is_zero() && delta.cost == 0.0 {
            return;
        }
        let cost = delta.cost_usd();
        let detail = json!({
            "session": session, "cum": cur.to_json(), "delta": delta.to_json(), "model": cur.model,
            "cost_source": cost.map(|(_, est)| if est { "estimate" } else { "opencode" }),
        })
        .to_string();
        // cached input counts as input; reasoning counts as output
        db::record_usage(
            &self.conn,
            &t.id,
            &t.context_id,
            &t.to_agent,
            delta.input + delta.cache_read + delta.cache_write,
            delta.output + delta.reasoning,
            cost.map(|(c, _)| c),
            cur.model.as_deref(),
            &detail,
        );
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
        let finished = db::tasks(
            &self.conn,
            "WHERE state IN (?,?,?,?,?) AND updated_at > ? ORDER BY updated_at",
            &params,
        )?;
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

    /// The error the assignee's last turn ended with, when that turn ended after `since` (the
    /// hand-over). `None`: nothing to report, or no session to ask.
    fn failed_turn(&self, t: &TaskRow, since: &str, active: &HashSet<String>) -> R<Option<String>> {
        let Some(dir) = t.directory.as_deref().filter(|_| !self.cfg.dry) else {
            return Ok(None);
        };
        let Some(session) = self.agent_session_with(t, &t.to_agent, dir)? else {
            return Ok(None);
        };
        if active.contains(&session) {
            return Ok(None);
        }
        Ok(self
            .backend_for_task(t, &t.to_agent)
            .failure_since(&session, parse_ms(since)))
    }

    fn fail_for_agent_error(&self, t: &TaskRow, err: &str) -> R<()> {
        log(&format!(
            "task {}: {}'s turn failed ({err}); failing the task",
            &t.id[..8],
            t.to_agent
        ));
        self.event(t, "agent_error", &t.to_agent, err);
        let text = format!(
            "agent_error: {}'s turn failed: {err}. Fix that (model, credentials), then send the task again.",
            t.to_agent
        );
        self.post_system_message(t, "failed", &text)
    }

    fn stall_check(&mut self) -> R<()> {
        let open = db::tasks(
            &self.conn,
            "WHERE state IN ('submitted','working') AND to_agent != 'any'",
            &[],
        )?;
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
            // the agent's turn already failed (provider error, no licence, ...): fail the task now
            // with that error instead of leaving it to stall for minutes
            if let Some(err) = self.failed_turn(&t, &last, &active)? {
                self.fail_for_agent_error(&t, &err)?;
                continue;
            }
            let age_s = (chrono::Utc::now().timestamp_millis() - parse_ms(&last)) as f64 / 1000.0;
            if age_s < self.cfg.stall_secs as f64 {
                continue;
            }
            let session = match t.directory.as_deref() {
                Some(d) => self.agent_session_with(&t, &t.to_agent, d)?,
                None => None,
            };
            if session.as_ref().is_some_and(|s| active.contains(s)) {
                continue; // still working
            }
            let retried = db::exists(
                &self.conn,
                "SELECT 1 FROM nudges WHERE task_id = ? AND kind = 'stall-retry'",
                &[&t.id],
            );
            if !retried {
                log(&format!(
                    "stalled task {} ({}); retrying once",
                    &t.id[..8],
                    t.to_agent
                ));
                let sr_key = format!("{}:stall-retry", t.id);
                if self.insert_nudge(&sr_key, &t, &t.to_agent, "stall-retry")? {
                    self.finish_nudge(&sr_key, "delivered", 0, None);
                }
                self.event(
                    &t,
                    "stall_retry",
                    &t.to_agent,
                    &format!("{}s", age_s.round()),
                );
                if let (false, Some(s)) = (self.cfg.dry, &session) {
                    let style = self.backend_for_task(&t, &t.to_agent).style();
                    let text = nudge_text("assignee", &t, &style);
                    if let Err(err) = self.deliver_named(
                        self.task_backend_name(&t, &t.to_agent),
                        s,
                        &t.to_agent,
                        t.directory.as_deref(),
                        &text,
                    ) {
                        log(&format!("retry failed: {}", first_line(&err)));
                    }
                }
            } else if age_s > (self.cfg.stall_secs * 2) as f64 {
                log(&format!(
                    "task {} still stalled; marking failed",
                    &t.id[..8]
                ));
                self.conn
                    .execute(
                        "INSERT OR IGNORE INTO nudges (key, task_id, context_id, target, kind, status, delivered_at, created_at, updated_at) VALUES (?,?,?,?,?,'delivered',?,?,?)",
                        params![format!("{}:stall-failed", t.id), t.id, t.context_id, t.to_agent, "stall-failed", now(), now(), now()],
                    )
                    .map_err(e)?;
                self.event(
                    &t,
                    "stall_failed",
                    &t.to_agent,
                    &format!("{}s", age_s.round()),
                );
                self.post_system_message(
                    &t,
                    "failed",
                    &format!(
                        "stalled: {} did not report after {}s and one retry.",
                        t.to_agent,
                        age_s.round()
                    ),
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
                    "INSERT OR IGNORE INTO nudges (key, task_id, context_id, target, kind, status, delivered_at, created_at, updated_at) VALUES (?,?,?,?,?,'delivered',?,?,?)",
                    params![format!("{}:{}:requester", t.id, status_message_id(t)), t.id, t.context_id, t.from_agent, "baseline", now(), now(), now()],
                )
                .map_err(e)?;
        }
        log(&format!(
            "baseline: {} finished task(s) marked as already handled",
            rows.len()
        ));
        Ok(())
    }
}

/// Returns `Ok(None)` for legacy contexts without a routing association and `Err(())` for a
/// malformed or unsupported persisted association. Invalid stored values fail closed to OpenCode.
fn stored_backend_name(metadata: Option<&str>) -> Result<Option<&'static str>, ()> {
    let Some(metadata) = metadata else {
        return Ok(None);
    };
    let value = serde_json::from_str::<Value>(metadata).map_err(|_| ())?;
    let Some(routing) = value.get("routing") else {
        return Ok(None);
    };
    let Some(routing) = routing.as_object() else {
        return Err(());
    };
    let Some(backend) = routing.get("backend") else {
        return Err(());
    };
    match backend.as_str() {
        Some("opencode") => Ok(Some("opencode")),
        Some("claude") => Ok(Some("claude")),
        _ => Err(()),
    }
}

/// Runtimes that have open work: the backend each open task's context is routed to (NULL = opencode).
/// Nothing open means nothing to ask, so an idle or Claude-only install never contacts opencode.
fn needed_backends(conn: &rusqlite::Connection) -> BTreeSet<String> {
    let sql = "SELECT DISTINCT COALESCE(json_extract(c.metadata, '$.routing.backend'), 'opencode') \
               FROM tasks t JOIN contexts c ON c.id = t.context_id \
               WHERE t.state IN ('submitted','working','input-required','auth-required')";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return backend::NAMES.iter().map(|n| n.to_string()).collect();
    };
    stmt.query_map([], |r| r.get::<_, String>(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

const NUDGE_LEASE_MS: i64 = 60_000;

fn iso_after_ms(ms: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::milliseconds(ms))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    fn choice(vars: &[(&str, &str)]) -> Choice {
        Choice::from_vars(vars.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    #[test]
    fn opencode_runs_every_agent_unless_told_otherwise() {
        let c = choice(&[("PATH", "/bin"), ("DISPATCH_POLL_MS", "10")]);
        assert_eq!(
            (c.of("mentor"), c.of("any-agent"), c.used()),
            ("opencode", "opencode", BTreeSet::from(["opencode"]))
        );
    }

    #[test]
    fn the_default_and_the_per_agent_override_pick_the_backend() {
        let c = choice(&[
            ("DISPATCH_BACKEND", "claude"),
            ("DISPATCH_BACKEND_ORCHESTRATOR", "opencode"),
            ("DISPATCH_BACKEND_CODE_REVIEWER", " claude "),
        ]);
        assert_eq!(
            (c.of("mentor"), c.of("orchestrator"), c.of("code-reviewer")),
            ("claude", "opencode", "claude")
        );
        assert_eq!(c.used(), BTreeSet::from(["opencode", "claude"]));
        let only = choice(&[("DISPATCH_BACKEND_CODE_REVIEWER", "claude")]);
        assert_eq!(
            (
                only.of("code-reviewer"),
                only.of("code.reviewer"),
                only.of("mentor")
            ),
            ("claude", "claude", "opencode")
        );
    }

    #[test]
    fn explicit_global_opencode_remains_an_override() {
        let c = choice(&[("DISPATCH_BACKEND", "opencode")]);
        assert_eq!(c.overrides("mentor"), (None, Some("opencode")));
    }

    #[test]
    fn persisted_backend_is_tri_state_and_invalid_fails_closed() {
        assert_eq!(stored_backend_name(None), Ok(None));
        assert_eq!(
            stored_backend_name(Some(r#"{"routing":{"backend":"claude"}}"#)),
            Ok(Some("claude"))
        );
        assert_eq!(
            stored_backend_name(Some(r#"{"routing":{"backend":"other"}}"#)),
            Err(())
        );
        assert_eq!(stored_backend_name(Some(r#"{"routing":{}}"#)), Err(()));
        for routing in [r#""claude""#, "null", "[]", "42"] {
            assert_eq!(
                stored_backend_name(Some(&format!(r#"{{"routing":{routing}}}"#))),
                Err(())
            );
        }
    }

    #[test]
    fn unknown_backends_and_lookalike_variables_are_ignored() {
        let c = choice(&[
            ("DISPATCH_BACKEND", "gpt"),
            ("DISPATCH_BACKENDX", "claude"),
            ("DISPATCH_BACKEND_MENTOR", ""),
        ]);
        assert_eq!(c, choice(&[]));
    }

    #[test]
    fn retries_back_off_and_are_capped() {
        let d: Vec<i64> = (1..=5).map(|a| retry_delay_ms(a, 5000)).collect();
        assert_eq!(d, vec![5000, 15000, 45000, 120000, 120000]);
        assert_eq!(retry_delay_ms(1, 50), 50);
    }

    #[test]
    fn versions_compare_and_unknown_is_oldest() {
        assert!(is_older("", "1.3.0") && is_older("1.2.9", "1.3.0") && is_older("1.3.0", "1.10.0"));
        assert!(!is_older("1.3.0", "1.3.0") && !is_older("2.0.0", "1.3.0"));
    }
}

fn status_message_id(t: &TaskRow) -> String {
    serde_json::from_str::<Value>(t.status_message.as_deref().unwrap_or("{}"))
        .ok()
        .and_then(|m| {
            m["messageId"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
        .unwrap_or_else(|| t.updated_at.clone())
}

fn status_text(t: &TaskRow) -> String {
    serde_json::from_str::<Value>(t.status_message.as_deref().unwrap_or("{}"))
        .ok()
        .and_then(|m| m["parts"].as_array().cloned())
        .map(|ps| {
            ps.iter()
                .map(|p| p["text"].as_str().unwrap_or(""))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

fn nudge_text(kind: &str, t: &TaskRow, style: &backend::Style) -> String {
    let p = style.tool_prefix;
    if kind == "assignee" {
        let proj = t
            .directory
            .as_deref()
            .map(|d| format!(" (project {d})"))
            .unwrap_or_default();
        let dir = t
            .directory
            .as_deref()
            .map(|d| format!(", directory:\"{d}\""))
            .unwrap_or_default();
        return format!(
            "A2A: you are agent \"{to}\". Task {id} is waiting for you{proj}. Call {p}inbox {{agent:\"{to}\"{dir}}}, claim the task with {p}update_task state \"working\", do the work, then finish with {p}update_task state completed | input-required | failed and put the deliverable in an artifact. Your requester is \"{from}\": report ONLY with {p}update_task on this task, never by creating a new task. The only orchestrator is \"orchestrator\" (never \"gentle-orchestrator\"); do not send it work.{note}",
            from = t.from_agent,
            to = t.to_agent,
            id = t.id,
            note = style.assignee_note
        );
    }
    format!(
        "A2A: task {id} that you sent to \"{to}\" is now {state}. Call {p}get_task {{id:\"{id}\"}}, judge the result, then continue your plan, answer the question with {p}send_message {{taskId}}, or ask the user if it needs their decision.",
        id = t.id,
        to = t.to_agent,
        state = t.state
    )
}

/// Make sure a dispatcher is running, starting one detached (own process group, logging to a
/// file) rather than opening another window or pane. `Some(pid)` = started now, `None` = was
/// already running. Used by the hub (so opencode alone is enough) and by `oc`.
/// Registry of running dispatchers: one file per pid in `<pidfile>.d/`, content = the version of
/// the binary. The pidfile alone cannot tell a stale old-version dispatcher from the current one,
/// and old ones used to survive upgrades (and fail on the new schema).
fn registry_dir() -> PathBuf {
    let mut p = paths::dispatcher_pid_path().into_os_string();
    p.push(".d");
    PathBuf::from(p)
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn register_self() {
    let dir = registry_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(std::process::id().to_string()), VERSION);
}

fn unregister_self() {
    let _ = std::fs::remove_file(registry_dir().join(std::process::id().to_string()));
}

/// Live registered dispatchers as (pid, version); entries of dead processes are removed.
pub fn registered() -> Vec<(u32, String)> {
    let Ok(rd) = std::fs::read_dir(registry_dir()) else {
        return vec![];
    };
    let mut out = vec![];
    for entry in rd.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if pid_is_dispatcher(pid) {
            out.push((
                pid,
                std::fs::read_to_string(entry.path())
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            ));
        } else {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    out
}

/// "1.3.0" -> (1,3,0); anything unparsable sorts as the oldest.
fn version_key(v: &str) -> (u64, u64, u64) {
    let mut it = v.trim().split('.').map(|p| {
        p.split(['-', '+'])
            .next()
            .unwrap_or("")
            .parse::<u64>()
            .unwrap_or(0)
    });
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    )
}

/// True when `v` is a known version older than `than` (unknown/empty counts as older: only
/// dispatchers from before the registry existed lack one).
fn is_older(v: &str, than: &str) -> bool {
    v.trim().is_empty() || version_key(v) < version_key(than)
}

/// TERM `pid` and wait up to `wait_ms` for it to go away.
fn terminate(pid: u32, wait_ms: u64) -> bool {
    sys::kill_term(pid);
    let end = std::time::Instant::now() + Duration::from_millis(wait_ms);
    while sys::pid_alive(pid) {
        if std::time::Instant::now() > end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// Dispatchers from before the registry existed are not registered, so they can only be found by
/// name. Only done for the default database: a sandbox (A2A_DB set) must never touch other processes.
pub fn sweep_unregistered() {
    if std::env::var_os("A2A_DB").is_none() {
        let _ = std::process::Command::new("pkill")
            .args(["-f", "codetypera2a dispatcher"])
            .status();
    }
}

fn terminate_all(pids: impl Iterator<Item = u32>) {
    pids.for_each(|p| {
        terminate(p, 3000);
    });
}

/// `oc stop`: end every dispatcher of this database (registered ones and the pidfile owner).
/// Returns how many were signalled.
pub fn stop_all() -> usize {
    mark_stopped();
    let mut pids: Vec<u32> = registered().into_iter().map(|(p, _)| p).collect();
    if let Some(p) = sys::dispatcher_pid(&paths::dispatcher_pid_path()) {
        if !pids.contains(&p) {
            pids.push(p);
        }
    }
    for p in &pids {
        terminate(*p, 3000);
    }
    let _ = std::fs::remove_file(paths::dispatcher_pid_path());
    pids.len()
}

/// Present after `oc stop` / `oc shutdown`: the hubs that opencode keeps running supervise the
/// dispatcher and would restart it within seconds, so they must leave it stopped.
fn stopped_marker() -> PathBuf {
    let mut p = paths::dispatcher_pid_path().into_os_string();
    p.push(".stopped");
    PathBuf::from(p)
}

pub fn mark_stopped() {
    let _ = std::fs::write(stopped_marker(), "");
}

/// The supervising hubs' version of `ensure_running`: never restarts a dispatcher that was
/// stopped on purpose. Starting opencode again (`oc`, `oc ensure`, a new hub) clears the marker.
pub fn ensure_running_supervised() -> Result<Option<u32>, String> {
    match stopped_marker().exists() {
        true => Ok(None),
        false => ensure_running(),
    }
}

pub fn ensure_running() -> Result<Option<u32>, String> {
    let _ = std::fs::remove_file(stopped_marker()); // an explicit start
    let pid_file = paths::dispatcher_pid_path();
    let regs = registered();
    let Some(pid) = sys::dispatcher_pid(&pid_file) else {
        return start_dispatcher();
    };
    let ver = regs
        .iter()
        .find(|(p, _)| *p == pid)
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    if !is_older(ver, VERSION) {
        // current (or newer than this binary: never replace it) -> keep it, end any duplicates
        terminate_all(regs.iter().map(|(p, _)| *p).filter(|p| *p != pid));
        return Ok(None);
    }
    log(&format!(
        "replacing dispatcher pid {pid} (version {}) with {VERSION}",
        ["unknown", ver][usize::from(!ver.is_empty())]
    ));
    terminate(pid, 3000);
    let _ = std::fs::remove_file(&pid_file);
    // an unregistered one is an old build: its siblings are old too
    ver.is_empty().then(sweep_unregistered);
    start_dispatcher()
}

/// Nobody owns the pidfile: registered strays must not keep running next to the new one.
fn start_dispatcher() -> Result<Option<u32>, String> {
    terminate_all(registered().into_iter().map(|(p, _)| p));
    let exe = std::env::current_exe().map_err(e)?;
    // the `oc` name only knows `dispatch`; anything else would be handed to opencode as a folder
    let sub = if exe.file_name().is_some_and(|n| n == "oc") {
        "dispatch"
    } else {
        "dispatcher"
    };
    sys::spawn_detached(&exe, &[sub], &log_path())
        .map(Some)
        .map_err(e)
}

pub fn log_path() -> PathBuf {
    let mut log = paths::db_path().into_os_string();
    log.push(".dispatcher.log");
    PathBuf::from(log)
}

/// Inside tmux, keep a detached `a2a-status` window following the dispatcher live
/// (`oc watch`), so the A2A traffic is visible without taking over your pane.
pub fn ensure_status_window() -> Result<sys::Tab, String> {
    // Never from inside the status window itself: it would only spawn more of them.
    if std::env::var("A2A_STATUS_WINDOW").as_deref() == Ok("1") {
        return Ok(sys::Tab::AlreadyOpen);
    }
    let exe = std::env::current_exe().map_err(e)?;
    // Run through the `oc` symlink, current_exe is `oc` and the `oc` subcommand would be handed
    // to opencode (which fails, closes the window and gets it reopened forever).
    let watch = if exe.file_name().is_some_and(|n| n == "oc") {
        "watch"
    } else {
        "oc watch"
    };
    let cmd = format!(
        "A2A_STATUS_WINDOW=1 A2A_NO_DISPATCHER=1 {} {watch}",
        sys::sh(&exe.display().to_string())
    );
    sys::tmux_ensure_window("a2a-status", &cmd)
}

pub fn run() -> i32 {
    let mut cfg = Cfg::from_env();
    let mut migration_guard = if std::env::var_os("A2A_DB").is_none() {
        let home = paths::runtime_home();
        let lock = paths::migration_lock_path(&home);
        if let Some(parent) = lock.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                eprintln!("dispatcher: migration lock parent: {err}");
                return 1;
            }
        }
        let file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(file) => file,
            Err(err) => {
                eprintln!("dispatcher: migration lock unavailable: {err}");
                return 1;
            }
        };
        if let Err(err) = paths::ensure_private_file(&lock) {
            let _ = std::fs::remove_file(&lock);
            eprintln!("dispatcher: migration lock permissions: {err}");
            return 1;
        }
        if let Err(err) = paths::migrate_legacy_locked(&home, &paths::legacy_home()) {
            drop(file);
            let _ = std::fs::remove_file(&lock);
            eprintln!("dispatcher: legacy migration: {err}");
            return 1;
        }
        Some((file, lock))
    } else {
        None
    };
    // All dispatcher runtime paths are derived from the home whose migration lock we held.
    // A live legacy owner has already returned above, so startup can never mix homes.
    let owner = paths::resolve_runtime_home(&paths::runtime_home(), &paths::legacy_home());
    cfg.pid_file = paths::runtime_paths(&owner).pid;
    let pid_file = cfg.pid_file.clone();
    match acquire_lock(&pid_file) {
        Ok(Lock::Held) => {}
        Ok(Lock::Other(pid)) => {
            if let Some((file, lock)) = migration_guard.take() {
                drop(file);
                let _ = std::fs::remove_file(lock);
            }
            log(&format!(
                "another dispatcher is already running (pid {pid}, {}); exiting",
                pid_file.display()
            ));
            return 0;
        }
        Err(err) => {
            if let Some((file, lock)) = migration_guard.take() {
                drop(file);
                let _ = std::fs::remove_file(lock);
            }
            eprintln!("dispatcher: {err}");
            return 1;
        }
    }
    register_self();
    let pf = pid_file.clone();
    let migration_lock = migration_guard.as_ref().map(|(_, lock)| lock.clone());
    let _ = ctrlc::set_handler(move || {
        release_lock(&pf);
        if let Some(lock) = &migration_lock {
            let _ = std::fs::remove_file(lock);
        }
        std::process::exit(0);
    });
    let db_path = paths::db_path();
    let conn = match if migration_guard.is_some() {
        db::open_after_migration(&db_path)
    } else {
        db::open(&db_path)
    } {
        Ok(c) => c,
        Err(err) => {
            eprintln!("dispatcher: {err}");
            release_lock(&pid_file);
            if let Some((file, lock)) = migration_guard.take() {
                drop(file);
                let _ = std::fs::remove_file(lock);
            }
            return 1;
        }
    };
    // Persisted context routing may outlive environment overrides; keep both runtimes available so
    // retries never switch or fail merely because DISPATCH_BACKEND changed.
    let names: BTreeSet<&'static str> = backend::NAMES
        .into_iter()
        .chain(cfg.choice.used())
        .collect();
    let backends = backend::build(&names);
    let mut d = Dispatcher {
        conn,
        cfg,
        active: (None, HashSet::new()),
        backends,
    };
    log(&format!(
        "dispatcher up: db={} poll={}ms maxHops={} stall={}s agents={}{}",
        db_path.display(),
        d.cfg.poll_ms,
        d.cfg.max_hops,
        d.cfg.stall_secs,
        d.cfg.agent_dir.display(),
        if d.cfg.dry { " DRY-RUN" } else { "" }
    ));
    if std::env::var("DISPATCH_NO_BASELINE").as_deref() != Ok("1") {
        if let Err(err) = d.baseline() {
            log(&format!("baseline failed: {err}"));
            release_lock(&pid_file);
            if let Some((file, lock)) = migration_guard.take() {
                drop(file);
                let _ = std::fs::remove_file(lock);
            }
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

#[cfg(test)]
mod needed_tests {
    use super::*;

    #[test]
    fn only_runtimes_with_open_work_are_asked() {
        let dir = std::env::temp_dir().join(format!("a2a-needed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = db::open(&dir.join("hub.db")).unwrap();
        assert!(needed_backends(&conn).is_empty(), "idle asks nobody");
        let add = |ctx: &str, backend: Option<&str>, state: &str| {
            let meta = backend.map(|b| format!(r#"{{"routing":{{"backend":"{b}"}}}}"#));
            conn.execute("INSERT OR IGNORE INTO agents (name, card, status, created_at, updated_at) VALUES ('a','{}','active','t','t')", []).unwrap();
            conn.execute("INSERT INTO contexts (id, metadata, created_at, updated_at) VALUES (?, ?, 't', 't')", rusqlite::params![ctx, meta]).unwrap();
            conn.execute("INSERT INTO tasks (id, context_id, from_agent, to_agent, state, history, artifacts, created_at, updated_at) VALUES (?, ?, 'a', 'a', ?, '[]', '[]', 't', 't')", rusqlite::params![format!("t-{ctx}"), ctx, state]).unwrap();
        };
        add("c1", Some("claude"), "working");
        add("c2", None, "completed");
        assert_eq!(needed_backends(&conn), BTreeSet::from(["claude".to_string()]));
        add("c3", None, "submitted");
        assert_eq!(needed_backends(&conn).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
