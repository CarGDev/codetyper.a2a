//! The seam between the dispatcher's state machine (claim, hop cap, retries, stall check, who is
//! woken when) and the runtime that actually runs an agent. A backend finds or creates the agent's
//! session, tells which sessions are running a turn, hands them a prompt and reports how a turn
//! ended. The dispatcher itself never names a runtime.
//!
//! `OpencodeBackend` is the original behaviour (the `opencode api` server); `ClaudeBackend`
//! (`claude_backend.rs`) runs headless `claude -p` turns.
use super::claude_backend::ClaudeBackend;
use super::dispatcher::log;
use crate::hub::project::project_root;
use crate::{opencode, paths};
use serde_json::json;
use std::collections::{BTreeSet, HashSet};
use std::io::Write;

pub type R<T> = Result<T, String>;

/// What a backend knows about a session the hub has registered.
#[derive(Debug, PartialEq)]
pub enum SessionState {
    Usable,
    FolderGone,
    Gone,
}

/// How the prompts name the hub tools and what else a backend needs the agent to know.
pub struct Style {
    /// Put before the tool name: `a2a_` (opencode) or `mcp__a2a__` (Claude Code).
    pub tool_prefix: &'static str,
    /// Appended to the assignee prompt (leading space included); empty when nothing is needed.
    pub assignee_note: &'static str,
}

pub trait Backend: Send + Sync {
    /// The name used in `DISPATCH_BACKEND` and in messages.
    fn name(&self) -> &'static str;

    /// What `sessions.backend` holds for this backend's sessions (`None` = NULL, the opencode default).
    fn stored_name(&self) -> Option<&'static str> {
        None
    }

    fn style(&self) -> Style;

    /// Is the registered session (`registered_dir` is the folder it was stored under) still usable?
    fn session_state(&self, id: &str, registered_dir: &str) -> SessionState;

    /// A new session for `agent` in `dir`; returns its id. The dispatcher registers it.
    fn create_session(&self, agent: &str, dir: &str) -> R<String>;

    /// Ids of sessions running a turn right now.
    fn active_ids(&self) -> R<HashSet<String>>;

    /// Wake `agent` in `session` with `text`. `Ok(false)` = the session is mid-turn; try again later.
    /// The dispatcher has already checked `active_ids` (cached), so a backend that can be raced
    /// re-checks itself.
    fn send(&self, session: &str, agent: &str, dir: Option<&str>, text: &str) -> R<bool>;

    /// Cumulative usage of a session. `None` = the dispatcher must not record usage for it.
    fn usage(&self, session: &str) -> Option<opencode::Usage>;

    /// The error the session's last turn ended with, when that turn ended after `since_ms`.
    fn failure_since(&self, session: &str, since_ms: i64) -> Option<String>;

    /// Does the runtime know an agent called `name`? (The name was already checked for shape.)
    fn agent_known(&self, name: &str, dir: Option<&str>) -> bool;

    /// Show the session to the user, where the runtime has a place for that.
    fn request_tab(&self, _session: &str, _agent: &str, _dir: &str) {}

    /// Stop the turn a session is running (`oc cancel`).
    fn interrupt(&self, session: &str) -> R<()>;

    /// The usable session a task was sent from, when the runtime can tell.
    fn sent_from(&self, _session: &str) -> Option<String> {
        None
    }

    /// The newest usable session open in `dir` (a requester that never registered).
    fn newest_in(&self, _dir: &str) -> Option<String> {
        None
    }
}

/// The names `DISPATCH_BACKEND` accepts.
pub const NAMES: [&str; 2] = ["opencode", "claude"];

/// Backends are built by name; add a line here to add one.
fn build_one(name: &str) -> Option<Box<dyn Backend>> {
    match name {
        "opencode" => Some(Box::new(OpencodeBackend)),
        "claude" => Some(Box::new(ClaudeBackend::from_env())),
        _ => None,
    }
}

/// One backend per distinct name; unknown names are skipped.
pub fn build(names: &BTreeSet<&str>) -> Vec<Box<dyn Backend>> {
    names.iter().filter_map(|n| build_one(n)).collect()
}

/// Ids of the sessions running a turn in any backend (one that cannot be asked counts as none).
pub fn running_everywhere() -> HashSet<String> {
    NAMES.iter().filter_map(|n| build_one(n)).flat_map(|b| b.active_ids().unwrap_or_default()).collect()
}

/// The backend a `sessions.backend` value stands for (NULL = opencode).
pub fn for_stored(stored: Option<&str>) -> Option<Box<dyn Backend>> {
    build_one(stored.unwrap_or("opencode"))
}

/// opencode's own check: a session is only usable if its folder exists.
fn usable(info: &opencode::SessionInfo) -> bool {
    info.directory.is_empty() || std::path::Path::new(&info.directory).is_dir()
}

pub struct OpencodeBackend;

impl Backend for OpencodeBackend {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn style(&self) -> Style {
        Style { tool_prefix: "a2a_", assignee_note: "" }
    }

    fn session_state(&self, id: &str, _registered_dir: &str) -> SessionState {
        match opencode::get(id) {
            Some(info) if usable(&info) => SessionState::Usable,
            Some(_) => SessionState::FolderGone,
            None => SessionState::Gone,
        }
    }

    fn create_session(&self, agent: &str, dir: &str) -> R<String> {
        let mut body = json!({ "title": format!("{agent} (a2a)"), "agent": agent, "location": { "directory": dir } });
        // A session keeps its own model and ignores the agent's, so hand it the one opencode resolved
        // for this agent (set with `codetypera2a models` or in the agent file).
        if let Some(model) = opencode::agents(Some(dir)).ok().and_then(|l| l.into_iter().find(|a| a.id == agent)).and_then(|a| a.model) {
            body["model"] = model;
        }
        opencode::create_session(&body)
    }

    fn active_ids(&self) -> R<HashSet<String>> {
        opencode::active_ids()
    }

    fn send(&self, session: &str, agent: &str, _dir: Option<&str>, text: &str) -> R<bool> {
        let info = opencode::get(session).ok_or_else(|| format!("session {session} no longer exists"))?;
        if info.agent.as_deref() != Some(agent) {
            opencode::switch_agent(session, agent)?;
        }
        opencode::prompt(session, text)?;
        Ok(true)
    }

    fn usage(&self, session: &str) -> Option<opencode::Usage> {
        opencode::usage(session)
    }

    fn failure_since(&self, session: &str, since_ms: i64) -> Option<String> {
        opencode::failure_since(session, since_ms)
    }

    /// Any agent opencode knows (not only the installed template files). If the opencode agent list
    /// cannot be read, give the benefit of the doubt: session create will say.
    fn agent_known(&self, name: &str, dir: Option<&str>) -> bool {
        if paths::agent_dir().join(format!("{name}.md")).exists() {
            return true;
        }
        match opencode::agents(dir) {
            Ok(list) => list.iter().any(|a| a.id == name),
            Err(_) => true,
        }
    }

    /// Ask the opencode TUI plugin to show `session` as a tab (it tails tabs.jsonl).
    fn request_tab(&self, session: &str, agent: &str, dir: &str) {
        let line = json!({ "sessionID": session, "agent": agent, "directory": dir, "at": crate::hub::db::now() });
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

    fn interrupt(&self, session: &str) -> R<()> {
        opencode::interrupt(session)
    }

    fn sent_from(&self, session: &str) -> Option<String> {
        opencode::get(session).as_ref().filter(|i| usable(i)).map(|i| i.id.clone())
    }

    fn newest_in(&self, dir: &str) -> Option<String> {
        let mut found = opencode::list(dir).into_iter().find(usable);
        if found.is_none() {
            if let Some(root) = project_root(dir).filter(|r| r != dir) {
                found = opencode::list(&root).into_iter().find(usable);
            }
        }
        found.map(|s| s.id)
    }
}
