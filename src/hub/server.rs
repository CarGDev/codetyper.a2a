//! a2a-hub: a local A2A-style hub exposed as an MCP (stdio) server.
//!
//! OpenCode agents are sessions, not network servers, so instead of one A2A HTTP endpoint per
//! agent every agent talks to this shared hub through tools. Semantics follow the A2A spec:
//! AgentCard registry, Task lifecycle, Messages made of Parts, Artifacts, contextId grouping,
//! send/get/list/cancel. Delivery is pull-based (agents poll inbox); no push exists between
//! sessions. State lives in the shared SQLite file (WAL), see `hub::db`.
use super::db::{self, STATES, TERMINAL, TaskRow, now};
use super::project::same_project;
use regex::Regex;
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};
use std::io::{BufRead, Write};
use std::path::Path;
use uuid::Uuid;

type Args = Map<String, Value>;

fn uuid() -> String {
    Uuid::new_v4().to_string()
}

/// A non-empty string argument (JS: truthy string).
fn s<'a>(a: &'a Args, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str).filter(|v| !v.is_empty())
}

fn need<'a>(a: &'a Args, k: &str) -> Result<&'a str, String> {
    s(a, k).ok_or_else(|| format!("'{k}' is required"))
}

/// JS `Number(v)`: numbers and numeric strings.
fn num(a: &Args, k: &str) -> Option<f64> {
    match a.get(k)? {
        Value::Number(n) => n.as_f64(),
        Value::String(t) => t.trim().parse().ok(),
        _ => None,
    }
}

fn limit(a: &Args) -> i64 {
    let n = num(a, "limit").filter(|n| *n != 0.0 && !n.is_nan()).unwrap_or(20.0);
    n.clamp(1.0, 100.0) as i64
}

fn truthy_bool(a: &Args, k: &str) -> bool {
    a.get(k).is_some_and(|v| match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::String(t) => !t.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
        _ => true,
    })
}

fn to_parts(a: &Args) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(Value::Array(p)) = a.get("parts") {
        out.extend(p.iter().cloned());
    }
    if let Some(t) = a.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()) {
        out.push(json!({ "text": t }));
    }
    if let Some(d) = a.get("data").filter(|d| !d.is_null()) {
        out.push(json!({ "data": d }));
    }
    out
}

fn mk_message(role: &str, from: &str, parts: Vec<Value>, extra: Vec<(&str, Value)>) -> Value {
    let mut m = json!({
        "messageId": uuid(), "role": role, "from": from, "parts": parts, "timestamp": now(),
    });
    for (k, v) in extra {
        if !v.is_null() {
            m[k] = v;
        }
    }
    m
}

fn parse(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or(Value::Null)
}

fn row_to_task(r: &TaskRow, history_length: usize) -> Value {
    let history = parse(&r.history);
    let all = history.as_array().cloned().unwrap_or_default();
    let tail: Vec<Value> = if history_length == 0 {
        vec![]
    } else {
        all[all.len().saturating_sub(history_length)..].to_vec()
    };
    json!({
        "id": r.id,
        "contextId": r.context_id,
        "directory": r.directory,
        "fromSession": r.from_session,
        "from": r.from_agent,
        "to": r.to_agent,
        "status": {
            "state": r.state,
            "timestamp": r.updated_at,
            "message": r.status_message.as_deref().map(parse),
        },
        "history": tail,
        "historyTotal": all.len(),
        "artifacts": parse(&r.artifacts),
        "createdAt": r.created_at,
    })
}

/// Same project (git root), not just the same folder. Rows without a folder match everyone.
fn in_project(dir: Option<&str>, folders: &[Option<&str>]) -> bool {
    match dir {
        None => true,
        Some(d) => {
            folders.iter().all(|f| f.is_none())
                || folders.iter().flatten().any(|f| same_project(d, f))
        }
    }
}

pub struct Hub {
    conn: Connection,
}

impl Hub {
    pub fn open(db_path: &Path, agent_dir: &Path) -> Result<Self, String> {
        let conn = db::open(db_path)?;
        seed_agent_cards(&conn, agent_dir);
        Ok(Hub { conn })
    }

    fn tx<T>(&self, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        self.conn.execute_batch("BEGIN IMMEDIATE").map_err(|e| e.to_string())?;
        match f() {
            Ok(v) => {
                self.conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    fn row(&self, id: &str) -> Result<TaskRow, String> {
        db::task_by_id(&self.conn, id)?.ok_or_else(|| format!("task not found: {id}"))
    }

    fn db_err<T>(r: rusqlite::Result<T>) -> Result<T, String> {
        r.map_err(|e| e.to_string())
    }

    /// Dispatch a tool call; `None` means the tool does not exist.
    pub fn call(&self, name: &str, a: &Args) -> Option<Result<Value, String>> {
        Some(match name {
            "register_agent" => self.register_agent(a),
            "list_agents" => self.list_agents(a),
            "send_message" => self.send_message(a),
            "inbox" => self.inbox(a),
            "get_task" => self.get_task(a),
            "list_tasks" => self.list_tasks(a),
            "update_task" => self.update_task(a),
            "claim_task" => self.claim_task(a),
            "cancel_task" => self.cancel_task(a),
            _ => return None,
        })
    }

    fn register_agent(&self, a: &Args) -> Result<Value, String> {
        let name = need(a, "name")?;
        let card = json!({
            "name": name,
            "description": a.get("description").and_then(Value::as_str).unwrap_or(""),
            "skills": a.get("skills").cloned().unwrap_or(json!([])),
            "tags": a.get("tags").cloned().unwrap_or(json!([])),
            "source": "registered",
        });
        let ts = now();
        Self::db_err(self.conn.execute(
            "INSERT INTO agents (name, card, status, created_at, updated_at) VALUES (?, ?, 'active', ?, ?) ON CONFLICT(name) DO UPDATE SET card = excluded.card, status = 'active', updated_at = excluded.updated_at",
            params![name, card.to_string(), ts, ts],
        ))?;
        // A registration comes from an opencode session (backend NULL); the dispatcher's headless Claude
        // sessions (`backend = 'claude'`) are its own and are never retired here.
        if let (Some(sid), Some(dir)) = (s(a, "sessionId"), s(a, "directory")) {
            Self::db_err(self.conn.execute(
                "UPDATE sessions SET status='stale' WHERE agent=? AND directory=? AND session_id != ? AND backend IS NULL",
                params![name, dir, sid],
            ))?;
            Self::db_err(self.conn.execute(
                "INSERT INTO sessions (agent, directory, session_id, created_at, last_seen_at) VALUES (?,?,?,?,?) ON CONFLICT(agent, directory, session_id) DO UPDATE SET status='live', last_seen_at = excluded.last_seen_at",
                params![name, dir, sid, ts, ts],
            ))?;
        }
        Ok(card)
    }

    fn list_agents(&self, a: &Args) -> Result<Value, String> {
        let mut st = Self::db_err(self.conn.prepare("SELECT card FROM agents WHERE card != '{}' AND status = 'active' ORDER BY name"))?;
        let mut cards: Vec<Value> = Self::db_err(st.query_map([], |r| r.get::<_, String>(0)))?
            .flatten()
            .map(|c| parse(&c))
            .collect();
        // Every agent the user has in opencode (not just the installed templates). Hidden ones
        // (compaction, title, internal reviewers) are left out unless asked for by name or flag.
        let query = s(a, "query");
        let want_hidden = truthy_bool(a, "includeHidden");
        if let Ok(known) = crate::opencode::agents(s(a, "directory")) {
            for ag in known {
                let named = query.is_some_and(|q| q.eq_ignore_ascii_case(&ag.id));
                if ag.hidden && !want_hidden && !named {
                    cards.retain(|c| c["name"] != json!(ag.id));
                    continue;
                }
                let card = json!({
                    "name": ag.id, "description": ag.description, "mode": ag.mode,
                    "skills": [], "source": "opencode", "hidden": ag.hidden,
                });
                match cards.iter_mut().find(|c| c["name"] == json!(ag.id)) {
                    Some(existing) => *existing = card,
                    None => cards.push(card),
                }
            }
            cards.sort_by(|x, y| x["name"].as_str().cmp(&y["name"].as_str()));
        }
        if let Some(q) = query {
            let q = q.to_lowercase();
            cards.retain(|c| c.to_string().to_lowercase().contains(&q));
        }
        Ok(json!({ "agents": cards }))
    }

    fn send_message(&self, a: &Args) -> Result<Value, String> {
        let from = need(a, "from")?;
        let p = to_parts(a);
        if p.is_empty() {
            return Err("provide at least one of text, data, parts".into());
        }
        self.tx(|| {
            let ts = now();
            if let Some(task_id) = s(a, "taskId") {
                let row = self.row(task_id)?;
                if TERMINAL.contains(&row.state.as_str()) {
                    return Err(format!(
                        "task {task_id} is {} (terminal); create a new task and set referenceTaskIds",
                        row.state
                    ));
                }
                let is_requester = from == row.from_agent;
                let msg = mk_message(
                    if is_requester { "user" } else { "agent" },
                    from,
                    p,
                    vec![("taskId", json!(task_id))],
                );
                let mut history = parse(&row.history);
                history.as_array_mut().map(|h| h.push(msg.clone()));
                // A reply from the requester re-opens the task for the assignee.
                let state = if is_requester { "submitted" } else { row.state.as_str() };
                Self::db_err(self.conn.execute(
                    "UPDATE tasks SET history=?, state=?, status_message=?, updated_at=?, version=version+1 WHERE id=?",
                    params![history.to_string(), state, msg.to_string(), ts, task_id],
                ))?;
                if is_requester {
                    db::record_event(&self.conn, Some(task_id), Some(&row.context_id), "state_change", Some(from), Some(state));
                }
                return Ok(row_to_task(&self.row(task_id)?, 10));
            }
            let to = need(a, "to")?;
            if to == from {
                return Err(format!(
                    "'{from}' cannot send a task to itself (it would never be delivered): delegate to a different agent, or do the work yourself"
                ));
            }
            if to == "orchestrator" {
                return Err("do not send a new task to the orchestrator (that would open another orchestrator session): report on the task you were given with update_task (completed / input-required) so the result returns to the one orchestrator that sent it".into());
            }
            let id = uuid();
            let msg = mk_message(
                "user",
                from,
                p,
                vec![
                    ("taskId", json!(id)),
                    ("referenceTaskIds", a.get("referenceTaskIds").cloned().unwrap_or(Value::Null)),
                ],
            );
            let ctx = s(a, "contextId").map(String::from).unwrap_or_else(uuid);
            // Route results back to the opencode session this call comes from.
            let session = s(a, "sessionId").map(String::from).or_else(|| {
                let own = s(a, "fromDirectory").or(s(a, "directory")).map(String::from).or_else(|| {
                    std::env::current_dir().ok().map(|d| d.display().to_string())
                })?;
                crate::opencode::detect(&own, from)
            });
            for ag in [from, to] {
                Self::db_err(db::ensure_agent(&self.conn, ag))?;
            }
            Self::db_err(db::ensure_context(&self.conn, &ctx))?;
            // the first referenced task that exists becomes the parent (lineage)
            let parent = a
                .get("referenceTaskIds")
                .and_then(Value::as_array)
                .and_then(|ids| ids.iter().filter_map(Value::as_str).find(|r| self.row(r).is_ok()))
                .map(String::from);
            let priority = num(a, "priority").unwrap_or(0.0) as i64;
            Self::db_err(self.conn.execute(
                "INSERT INTO tasks (id, context_id, parent_task_id, from_agent, to_agent, state, status_message, history, artifacts, priority, created_at, updated_at, directory, from_directory, from_session) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    id, ctx, parent, from, to, "submitted", msg.to_string(), json!([msg]).to_string(), "[]", priority,
                    ts, ts, s(a, "directory"), s(a, "fromDirectory"), session
                ],
            ))?;
            db::record_event(&self.conn, Some(&id), Some(&ctx), "task_created", Some(from), Some(to));
            Ok(row_to_task(&self.row(&id)?, 10))
        })
    }

    fn inbox(&self, a: &Args) -> Result<Value, String> {
        let agent = need(a, "agent")?;
        let lim = limit(a) as usize;
        let dir = s(a, "directory");
        if s(a, "as") == Some("requester") {
            let rows: Vec<TaskRow> = db::tasks(
                &self.conn,
                "WHERE from_agent = ? AND state != 'submitted' AND (requester_seen_at IS NULL OR updated_at > requester_seen_at) ORDER BY updated_at",
                &[agent],
            )?
            .into_iter()
            .filter(|r| {
                let own = r.from_directory.as_deref().or(r.directory.as_deref());
                in_project(dir, &[own, r.directory.as_deref()])
            })
            .take(lim)
            .collect();
            if !truthy_bool(a, "peek") && !rows.is_empty() {
                let ts = now();
                for r in &rows {
                    Self::db_err(self.conn.execute(
                        "UPDATE tasks SET requester_seen_at = ? WHERE id = ?",
                        params![ts, r.id],
                    ))?;
                }
            }
            return Ok(json!({
                "as": "requester", "count": rows.len(),
                "tasks": rows.iter().map(|r| row_to_task(r, 3)).collect::<Vec<_>>(),
            }));
        }
        let rows: Vec<TaskRow> = db::tasks(
            &self.conn,
            "WHERE ((to_agent = ? AND state IN ('submitted','working')) OR (to_agent = 'any' AND state = 'submitted'))
               AND (claimed_by IS NULL OR claimed_by = ?) ORDER BY priority DESC, created_at",
            &[agent, agent],
        )?
        .into_iter()
        .filter(|r| in_project(dir, &[r.directory.as_deref()]))
        .take(lim)
        .collect();
        Ok(json!({
            "as": "assignee", "count": rows.len(),
            "tasks": rows.iter().map(|r| row_to_task(r, 3)).collect::<Vec<_>>(),
        }))
    }

    fn get_task(&self, a: &Args) -> Result<Value, String> {
        let row = self.row(need(a, "id")?)?;
        let n = num(a, "historyLength").unwrap_or(10.0).max(0.0) as usize;
        Ok(row_to_task(&row, n))
    }

    fn list_tasks(&self, a: &Args) -> Result<Value, String> {
        let mut clauses = Vec::new();
        let mut args: Vec<&str> = Vec::new();
        for (key, col) in [("contextId", "context_id"), ("state", "state"), ("from", "from_agent"), ("to", "to_agent")] {
            if let Some(v) = s(a, key) {
                clauses.push(format!("{col} = ?"));
                args.push(v);
            }
        }
        let wh = if clauses.is_empty() { String::new() } else { format!("WHERE {} ", clauses.join(" AND ")) };
        let rows = db::tasks(&self.conn, &format!("{wh}ORDER BY updated_at DESC LIMIT {}", limit(a)), &args)?;
        Ok(json!({ "count": rows.len(), "tasks": rows.iter().map(|r| row_to_task(r, 1)).collect::<Vec<_>>() }))
    }

    fn update_task(&self, a: &Args) -> Result<Value, String> {
        let id = need(a, "id")?;
        let agent = need(a, "agent")?;
        let state = s(a, "state");
        if let Some(st) = state {
            if !STATES.contains(&st) {
                return Err(format!("invalid state '{st}'; use one of {}", STATES.join(", ")));
            }
        }
        self.tx(|| {
            let row = self.row(id)?;
            if TERMINAL.contains(&row.state.as_str()) {
                return Err(format!("task {id} is {} (terminal); it cannot change", row.state));
            }
            if row.to_agent == "any" {
                return Err(format!("task {id} is not claimed yet: call claim_task first"));
            }
            if row.to_agent != agent {
                return Err(format!(
                    "only the assignee '{}' may update this task (you are '{agent}')",
                    row.to_agent
                ));
            }
            if let Some(owner) = row.claimed_by.as_deref().filter(|o| *o != agent) {
                return Err(format!("task {id} is claimed by '{owner}', not '{agent}'"));
            }
            if let Some(want) = num(a, "expectedVersion") {
                if want as i64 != row.version {
                    return Err(format!("version conflict on task {id}: expected {want}, current {}", row.version));
                }
            }
            let ts = now();
            let mut history = parse(&row.history);
            let mut artifacts = parse(&row.artifacts);
            let mut status_message = row.status_message.clone();
            let p = to_parts(a);
            if !p.is_empty() {
                let msg = mk_message("agent", agent, p, vec![("taskId", json!(id))]);
                history.as_array_mut().map(|h| h.push(msg.clone()));
                status_message = Some(msg.to_string());
            }
            if let Some(Value::Object(art)) = a.get("artifact").filter(|v| !v.is_null()) {
                let ap = to_parts(art);
                if ap.is_empty() {
                    return Err("artifact needs text, data or parts".into());
                }
                let str_or = |k: &str, d: &str| s(art, k).unwrap_or(d).to_string();
                artifacts.as_array_mut().map(|v| {
                    v.push(json!({
                        "artifactId": uuid(), "name": str_or("name", "result"),
                        "description": str_or("description", ""), "parts": ap, "createdAt": ts,
                    }))
                });
            }
            let new_state = state.unwrap_or(if row.state == "submitted" { "working" } else { &row.state });
            Self::db_err(self.conn.execute(
                "UPDATE tasks SET state=?, status_message=?, history=?, artifacts=?, updated_at=?, claimed_by=COALESCE(claimed_by, ?),
                   version=version+1, completed_at=CASE WHEN ? THEN ? END WHERE id=? AND version=?",
                params![new_state, status_message, history.to_string(), artifacts.to_string(), ts, agent,
                        TERMINAL.contains(&new_state), ts, id, row.version],
            ))?;
            db::record_event(&self.conn, Some(id), Some(&row.context_id), "state_change", Some(agent), Some(new_state));
            Ok(row_to_task(&self.row(id)?, 10))
        })
    }

    /// Atomic claim: of any number of agents asking for the same task, exactly one wins. The task
    /// must still be `submitted` and addressed to the caller or to `any`.
    fn claim_task(&self, a: &Args) -> Result<Value, String> {
        let id = need(a, "id")?;
        let agent = need(a, "agent")?;
        self.tx(|| {
            let row = self.row(id)?;
            Self::db_err(db::ensure_agent(&self.conn, agent))?;
            let ts = now();
            let n = Self::db_err(self.conn.execute(
                "UPDATE tasks SET to_agent=?, claimed_by=?, state='working', version=version+1, updated_at=?
                 WHERE id=? AND state='submitted' AND claimed_by IS NULL AND (to_agent=? OR to_agent='any')",
                params![agent, agent, ts, id, agent],
            ))?;
            if n != 1 {
                return Err(match row.claimed_by.as_deref() {
                    Some(owner) => format!("task {id} is already claimed by '{owner}'"),
                    None if row.to_agent != "any" && row.to_agent != agent => {
                        format!("task {id} is addressed to '{}', not '{agent}'", row.to_agent)
                    }
                    None => format!("task {id} cannot be claimed (state {})", row.state),
                });
            }
            db::record_event(&self.conn, Some(id), Some(&row.context_id), "task_claimed", Some(agent), Some(&row.to_agent));
            Ok(row_to_task(&self.row(id)?, 10))
        })
    }

    fn cancel_task(&self, a: &Args) -> Result<Value, String> {
        let id = need(a, "id")?;
        let agent = need(a, "agent")?;
        self.tx(|| {
            let row = self.row(id)?;
            if TERMINAL.contains(&row.state.as_str()) {
                return Err(format!("task {id} is already {}", row.state));
            }
            if agent != row.from_agent && agent != row.to_agent {
                return Err("only the requester or assignee may cancel".into());
            }
            let msg = mk_message(
                "agent",
                agent,
                vec![json!({ "text": s(a, "reason").unwrap_or("canceled") })],
                vec![("taskId", json!(id))],
            );
            let mut history = parse(&row.history);
            history.as_array_mut().map(|h| h.push(msg.clone()));
            Self::db_err(self.conn.execute(
                "UPDATE tasks SET state='canceled', status_message=?, history=?, updated_at=?, completed_at=?, version=version+1 WHERE id=?",
                params![msg.to_string(), history.to_string(), now(), now(), id],
            ))?;
            db::record_event(&self.conn, Some(id), Some(&row.context_id), "state_change", Some(agent), Some("canceled"));
            Ok(row_to_task(&self.row(id)?, 10))
        })
    }
}

/// Every `agent/*.md` becomes an AgentCard (description + mode from the frontmatter).
fn seed_agent_cards(conn: &Connection, dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let fence = Regex::new(r"(?m)^---\s*$").unwrap();
    let desc_re = Regex::new(r"(?m)^description:\s*(.*)$").unwrap();
    let mode_re = Regex::new(r"(?m)^mode:\s*(.*)$").unwrap();
    let quotes = Regex::new(r#"^['"]|['"]$"#).unwrap();
    let mut names: Vec<(String, std::path::PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            Some((n.strip_suffix(".md")?.to_string(), e.path()))
        })
        .collect();
    names.sort();
    for (name, path) in names {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let head = fence.split(&text).nth(1).unwrap_or("");
        let cap = |re: &Regex| re.captures(head).map(|c| c[1].to_string()).unwrap_or_default();
        let description = quotes.replace_all(&cap(&desc_re), "").into_owned();
        let card = json!({
            "name": name, "description": description, "mode": cap(&mode_re).trim(),
            "skills": [], "source": "agent-file",
        });
        let _ = conn.execute(
            "INSERT OR IGNORE INTO agents (name, card, status, created_at, updated_at) VALUES (?, ?, 'active', ?, ?)",
            params![name, card.to_string(), now(), now()],
        );
    }
}

fn str_prop(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn tools() -> Value {
    let states = json!(STATES);
    let part_arr = json!({ "type": "array", "items": { "type": "object" } });
    json!([
        {
            "name": "register_agent",
            "description": "Register or update your AgentCard (name, description, skills) so other agents can discover you.",
            "inputSchema": { "type": "object", "required": ["name"], "properties": {
                "name": str_prop("your agent name"), "description": str_prop("what you do"),
                "skills": { "type": "array", "items": { "type": "string" } },
                "tags": { "type": "array", "items": { "type": "string" } },
                "sessionId": str_prop("your OpenCode session id, if known"),
                "directory": str_prop("project folder (cwd)"),
            }},
        },
        {
            "name": "list_agents",
            "description": "List every agent opencode knows (built-in, your own, project agents, plugin agents) plus agents registered here. Optional text filter. Any listed agent can be a task assignee.",
            "inputSchema": { "type": "object", "properties": {
                "query": str_prop("filter text"),
                "directory": str_prop("project folder, to include that project's own agents"),
                "includeHidden": { "type": "boolean", "description": "also list hidden/internal agents" },
            }},
        },
        {
            "name": "send_message",
            "description": "Send work or a reply. Without taskId: creates a new Task (state submitted) for agent 'to' ('any' = first agent to claim it). With taskId: appends a message; a requester reply re-opens the task for the assignee. Use contextId to group related tasks, referenceTaskIds to link earlier ones.",
            "inputSchema": { "type": "object", "required": ["from"], "properties": {
                "from": str_prop("your agent name"),
                "to": str_prop("target agent name, or 'any'"),
                "text": str_prop("message text"),
                "data": { "description": "structured JSON payload" },
                "parts": { "type": "array", "items": { "type": "object" }, "description": "raw A2A parts ({text}|{data}|{url,filename,mediaType})" },
                "taskId": str_prop("existing task to reply to"),
                "contextId": str_prop("group id for related tasks"),
                "referenceTaskIds": { "type": "array", "items": { "type": "string" } },
                "sessionId": str_prop("your OpenCode session id, if you know it (auto-detected otherwise)"),
                "directory": str_prop("TARGET project folder: only agents in that project (same git root) see the task"),
                "fromDirectory": str_prop("YOUR OWN cwd. Pass it when directory is a different project than yours, so the result wakes you in the right tab. Omit if same project (stored as unknown)"),
            }},
        },
        {
            "name": "inbox",
            "description": "Check for messages. as='assignee' (default): tasks waiting for you (submitted/working). as='requester': updates on tasks you sent (input-required, completed, failed...); marks them seen unless peek=true. Call this at the start of work and after delegating.",
            "inputSchema": { "type": "object", "required": ["agent"], "properties": {
                "agent": str_prop("your agent name"),
                "as": { "type": "string", "enum": ["assignee", "requester"] },
                "limit": { "type": "number" }, "peek": { "type": "boolean" },
                "directory": str_prop("your project folder (cwd): only tasks for that folder (or without one) are returned"),
            }},
        },
        {
            "name": "get_task",
            "description": "Get a task with status, history and artifacts.",
            "inputSchema": { "type": "object", "required": ["id"], "properties": {
                "id": str_prop("task id"),
                "historyLength": { "type": "number", "description": "last N messages (default 10, 0 = none)" },
            }},
        },
        {
            "name": "list_tasks",
            "description": "List tasks with optional filters (contextId, state, from, to).",
            "inputSchema": { "type": "object", "properties": {
                "contextId": str_prop(""), "state": { "type": "string", "enum": states },
                "from": str_prop(""), "to": str_prop(""), "limit": { "type": "number" },
            }},
        },
        {
            "name": "update_task",
            "description": "Assignee only: claim (state working), ask a question (input-required), or finish (completed/failed/rejected). Attach a status message and/or an artifact (the deliverable: name + text/data/url).",
            "inputSchema": { "type": "object", "required": ["id", "agent"], "properties": {
                "id": str_prop("task id"), "agent": str_prop("your agent name"),
                "state": { "type": "string", "enum": states }, "text": str_prop("status message"),
                "expectedVersion": { "type": "integer", "description": "optional: fail if the task changed since you read this version" },
                "data": {}, "parts": part_arr,
                "artifact": { "type": "object", "properties": {
                    "name": { "type": "string" }, "description": { "type": "string" },
                    "text": { "type": "string" }, "data": {}, "parts": part_arr,
                }},
            }},
        },
        {
            "name": "claim_task",
            "description": "Take a task for yourself before working on it. Required for tasks addressed to 'any'; atomic, so when several agents ask, exactly one succeeds and the others get an error and must stop. A task sent to you by name is yours without claiming.",
            "inputSchema": { "type": "object", "required": ["id", "agent"], "properties": {
                "id": str_prop("task id"), "agent": str_prop("your agent name"),
            }},
        },
        {
            "name": "cancel_task",
            "description": "Cancel a non-terminal task (requester or assignee).",
            "inputSchema": { "type": "object", "required": ["id", "agent"], "properties": {
                "id": str_prop("task id"), "agent": str_prop("your agent name"), "reason": str_prop("why"),
            }},
        },
    ])
}

fn reply(out: &mut impl Write, id: &Value, result: Value) {
    let _ = writeln!(out, "{}", json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    let _ = out.flush();
}

fn fail(out: &mut impl Write, id: &Value, code: i64, message: &str) {
    let _ = writeln!(out, "{}", json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }));
    let _ = out.flush();
}

fn handle(hub: &Hub, msg: &Value, out: &mut impl Write) {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let is_notification = id.is_null();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    match msg.get("method").and_then(Value::as_str).unwrap_or("") {
        "initialize" => reply(
            out,
            &id,
            json!({
                "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "a2a-hub", "version": "1.0.0" },
            }),
        ),
        "ping" => reply(out, &id, json!({})),
        "tools/list" => reply(out, &id, json!({ "tools": tools() })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").and_then(Value::as_object).cloned().unwrap_or_default();
            match hub.call(name, &args) {
                None => reply(out, &id, json!({ "isError": true, "content": [{ "type": "text", "text": format!("unknown tool: {name}") }] })),
                Some(Ok(v)) => reply(out, &id, json!({
                    "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }],
                    "structuredContent": v,
                })),
                Some(Err(e)) => reply(out, &id, json!({ "isError": true, "content": [{ "type": "text", "text": e }] })),
            }
        }
        other => {
            if !is_notification {
                fail(out, &id, -32601, &format!("method not found: {other}"));
            }
        }
    }
}

/// Serve MCP JSON-RPC over stdio until stdin closes.
pub fn run() -> i32 {
    let hub = match Hub::open(&crate::paths::db_path(), &crate::paths::agent_dir()) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("a2a-hub: {e}");
            return 1;
        }
    };
    // Loading this MCP server is what opencode does on startup, so use it to bring up the
    // dispatcher too. stdout is the MCP channel: report problems on stderr only.
    if std::env::var("A2A_NO_DISPATCHER").as_deref() != Ok("1") {
        if let Err(err) = crate::dispatcher::dispatcher::ensure_running() {
            eprintln!("a2a-hub: could not start the dispatcher: {err}");
        }
        if let Err(err) = crate::dispatcher::dispatcher::ensure_status_window() {
            eprintln!("a2a-hub: could not open the tmux status window: {err}");
        }
        // Keep it alive for as long as opencode keeps this hub: if the dispatcher dies, start it
        // again. Single-instance via its pidfile, so several hubs do not pile up dispatchers.
        let every = std::time::Duration::from_secs(crate::paths::env_u64("A2A_SUPERVISE_SECS", 15));
        std::thread::spawn(move || loop {
            std::thread::sleep(every);
            match crate::dispatcher::dispatcher::ensure_running_supervised() {
                Ok(Some(pid)) => eprintln!("a2a-hub: dispatcher was not running; restarted (pid {pid})"),
                Ok(None) => {}
                Err(err) => eprintln!("a2a-hub: could not restart the dispatcher: {err}"),
            }
        });
    }
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&hub, &msg, &mut out),
            Err(_) => fail(&mut out, &Value::Null, -32700, "parse error"),
        }
    }
    0
}
