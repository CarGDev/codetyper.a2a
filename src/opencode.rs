//! The slice of the OpenCode v2 server API the hub needs (via `opencode api`, operation ids
//! `session.active`, `session.get`, `session.list`, `session.switchAgent`, `session.prompt`).
//! All agents share the sessions the user already has open; nothing here creates a session.
use crate::sys;
use serde_json::{Value, json};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub id: String,
    pub description: String,
    pub mode: String,
    pub hidden: bool,
    /// The model opencode resolved for this agent (frontmatter or `agents.<name>.model` in the config):
    /// `{id, providerID, variant?}`. A session stores its own model, so it has to be passed on create.
    pub model: Option<Value>,
}

type AgentCache = std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, Vec<AgentInfo>)>>;
static AGENT_CACHE: std::sync::LazyLock<AgentCache> = std::sync::LazyLock::new(Default::default);

/// Every agent opencode knows (`agent.list`): built-ins, the user's own, project agents of `dir`,
/// plugin agents. Cached for 20 s per folder.
pub fn agents(dir: Option<&str>) -> Result<Vec<AgentInfo>, String> {
    let key = dir.unwrap_or("").to_string();
    if let Some((at, v)) = AGENT_CACHE.lock().unwrap().get(&key) {
        if at.elapsed() < std::time::Duration::from_secs(20) {
            return Ok(v.clone());
        }
    }
    let route = match dir {
        Some(d) => format!("/api/agent?location%5Bdirectory%5D={}", sys::url_encode(d)),
        None => "/api/agent".to_string(),
    };
    // The first request for a folder opencode has not loaded yet answers with an empty list
    // (later ones return everything), so an empty answer is "not ready": retry, never cache it.
    let mut list = Vec::new();
    for attempt in 0..5 {
        let r = sys::api(&bin(), "get", &route, None)?;
        list = r["data"]
            .as_array()
            .ok_or_else(|| "agent.list returned no data".to_string())?
            .iter()
            .filter_map(|a| {
                Some(AgentInfo {
                    id: a["id"].as_str()?.to_string(),
                    description: a["description"].as_str().unwrap_or("").to_string(),
                    mode: a["mode"].as_str().unwrap_or("").to_string(),
                    hidden: a["hidden"].as_bool().unwrap_or(false),
                    model: a.get("model").filter(|m| m.is_object()).cloned(),
                })
            })
            .collect();
        if !list.is_empty() || attempt == 4 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    if list.is_empty() {
        return Err("opencode returned no agents (still loading?)".into());
    }
    AGENT_CACHE.lock().unwrap().insert(key, (std::time::Instant::now(), list.clone()));
    Ok(list)
}

pub fn bin() -> String {
    std::env::var("OPENCODE_BIN").unwrap_or_else(|_| "opencode".into())
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub agent: Option<String>,
    pub directory: String,
    pub updated: i64,
    pub is_child: bool,
}

fn parse_info(v: &Value) -> Option<SessionInfo> {
    Some(SessionInfo {
        id: v["id"].as_str()?.to_string(),
        agent: v["agent"].as_str().map(String::from),
        directory: v["location"]["directory"].as_str().unwrap_or("").to_string(),
        updated: v["time"]["updated"].as_i64().unwrap_or(0),
        is_child: v["parentID"].is_string(),
    })
}

/// Cumulative token use and cost of one session, as opencode reports it (`tokens`, `cost`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    pub input: i64,
    pub output: i64,
    pub reasoning: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    /// What opencode charged; 0 on subscription providers.
    pub cost: f64,
    /// `provider/model` of the session.
    pub model: Option<String>,
}

impl Usage {
    /// What was used between `prev` and `self` (counters only grow; a reset counts from zero).
    pub fn since(&self, prev: &Usage) -> Usage {
        let d = |a: i64, b: i64| if a >= b { a - b } else { a };
        Usage {
            input: d(self.input, prev.input),
            output: d(self.output, prev.output),
            reasoning: d(self.reasoning, prev.reasoning),
            cache_read: d(self.cache_read, prev.cache_read),
            cache_write: d(self.cache_write, prev.cache_write),
            cost: if self.cost >= prev.cost { self.cost - prev.cost } else { self.cost },
            model: self.model.clone(),
        }
    }

    pub fn is_zero(&self) -> bool {
        self.input + self.output + self.reasoning + self.cache_read + self.cache_write == 0
    }

    /// USD: opencode's own figure when it charges, else an estimate from the published price table
    /// (cache reads at 10% of the input price, cache writes at the input price). The bool says
    /// whether it is an estimate. None when the model has no known price.
    pub fn cost_usd(&self) -> Option<(f64, bool)> {
        if self.cost > 0.0 {
            return Some((self.cost, false));
        }
        let (pin, pout) = crate::models::price(self.model.as_deref()?)?;
        let usd = (self.input as f64 * pin
            + self.cache_write as f64 * pin
            + self.cache_read as f64 * pin * 0.1
            + (self.output + self.reasoning) as f64 * pout)
            / 1_000_000.0;
        Some((usd, true))
    }

    pub fn to_json(&self) -> Value {
        json!({ "input": self.input, "output": self.output, "reasoning": self.reasoning,
                "cache_read": self.cache_read, "cache_write": self.cache_write, "cost": self.cost })
    }

    pub fn from_json(v: &Value, model: Option<String>) -> Usage {
        let n = |k: &str| v[k].as_i64().unwrap_or(0);
        Usage {
            input: n("input"),
            output: n("output"),
            reasoning: n("reasoning"),
            cache_read: n("cache_read"),
            cache_write: n("cache_write"),
            cost: v["cost"].as_f64().unwrap_or(0.0),
            model,
        }
    }
}

/// `{"tokens":{input,output,reasoning,cache:{read,write}},"cost":..,"model":{id,providerID}}`
pub fn parse_usage(v: &Value) -> Option<Usage> {
    let t = v.get("tokens").filter(|t| t.is_object())?;
    let n = |x: &Value| x.as_i64().unwrap_or(0);
    let model = match (v["model"]["providerID"].as_str(), v["model"]["id"].as_str()) {
        (Some(p), Some(m)) => Some(format!("{p}/{m}")),
        (None, Some(m)) => Some(m.to_string()),
        _ => None,
    };
    Some(Usage {
        input: n(&t["input"]),
        output: n(&t["output"]),
        reasoning: n(&t["reasoning"]),
        cache_read: n(&t["cache"]["read"]),
        cache_write: n(&t["cache"]["write"]),
        cost: v["cost"].as_f64().unwrap_or(0.0),
        model,
    })
}

/// Cumulative usage of a session; None when opencode does not report it.
pub fn usage(id: &str) -> Option<Usage> {
    let r = sys::api(&bin(), "get", &format!("/api/session/{id}"), None).ok()?;
    parse_usage(&r["data"])
}

/// Ids of sessions currently running a turn.
pub fn active_ids() -> Result<HashSet<String>, String> {
    let r = sys::api(&bin(), "get", "/api/session/active", None)?;
    Ok(r["data"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default())
}

pub fn get(id: &str) -> Option<SessionInfo> {
    let r = sys::api(&bin(), "get", &format!("/api/session/{id}"), None).ok()?;
    parse_info(&r["data"])
}

/// Sessions whose folder is `dir`, newest first (sub-sessions excluded).
pub fn list(dir: &str) -> Vec<SessionInfo> {
    let route = format!("/api/session?directory={}&limit=25", sys::url_encode(dir));
    let Ok(r) = sys::api(&bin(), "get", &route, None) else { return vec![] };
    let mut v: Vec<SessionInfo> = r["data"].as_array().map(|a| a.iter().filter_map(parse_info).collect()).unwrap_or_default();
    v.retain(|s| !s.is_child);
    v.sort_by_key(|s| std::cmp::Reverse(s.updated));
    v
}

/// The session a tool call comes from: it is the one running a turn right now. Prefer a running
/// session in `dir` whose agent is `agent`; accept a lone running session in `dir`.
pub fn detect(dir: &str, agent: &str) -> Option<String> {
    let running = active_ids().ok()?;
    let here: Vec<SessionInfo> = running
        .iter()
        .filter_map(|id| get(id))
        .filter(|s| s.directory == dir || crate::hub::project::same_project(dir, &s.directory))
        .collect();
    let by_agent: Vec<&SessionInfo> = here.iter().filter(|s| s.agent.as_deref() == Some(agent)).collect();
    match (by_agent.as_slice(), here.as_slice()) {
        ([one], _) => Some(one.id.clone()),
        ([], [one]) => Some(one.id.clone()),
        _ => None,
    }
}

pub fn switch_agent(id: &str, agent: &str) -> Result<(), String> {
    sys::api(&bin(), "post", &format!("/api/session/{id}/agent"), Some(&json!({ "agent": agent }))).map(|_| ())
}

/// Stop the turn a session is running right now.
pub fn interrupt(id: &str) -> Result<(), String> {
    sys::api(&bin(), "post", &format!("/api/session/{id}/interrupt"), None).map(|_| ())
}

pub fn prompt(id: &str, text: &str) -> Result<(), String> {
    sys::api(&bin(), "post", &format!("/api/session/{id}/prompt"), Some(&json!({ "text": text, "delivery": "queue" }))).map(|_| ())
}

/// Create a session (`session.create`); returns its id.
pub fn create_session(body: &Value) -> Result<String, String> {
    let r = sys::api(&bin(), "post", "/api/session", Some(body))?;
    r["data"]["id"].as_str().map(String::from).ok_or_else(|| "session create returned no id".into())
}

/// File the dispatcher appends to and the TUI plugin tails to open session tabs.
pub fn tabs_file() -> std::path::PathBuf {
    crate::paths::db_path().with_file_name("tabs.jsonl")
}

/// Every `provider/model` opencode can use right now (`opencode models`).
pub fn models() -> Result<Vec<String>, String> {
    let out = sys::run(&bin(), &["models"])?;
    let mut v: Vec<String> = out
        .lines()
        .map(str::trim)
        .filter(|l| l.contains('/') && !l.contains(' '))
        .map(String::from)
        .collect();
    v.sort();
    v.dedup();
    if v.is_empty() {
        return Err("`opencode models` listed no models".into());
    }
    Ok(v)
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn parses_the_session_totals_and_diffs_them() {
        let v = json!({"cost":0,"model":{"id":"claude-haiku-5.5","providerID":"github-copilot"},
            "tokens":{"input":1000,"output":200,"reasoning":50,"cache":{"read":4000,"write":10}}});
        let a = parse_usage(&v).unwrap();
        assert_eq!((a.input, a.output, a.reasoning, a.cache_read, a.cache_write), (1000, 200, 50, 4000, 10));
        assert_eq!(a.model.as_deref(), Some("github-copilot/claude-haiku-5.5"));
        let b = Usage { input: 1500, output: 260, ..a.clone() };
        let d = b.since(&a);
        assert_eq!((d.input, d.output, d.cache_read), (500, 60, 0));
        assert!(parse_usage(&json!({"id":"x"})).is_none());
    }

    #[test]
    fn cost_uses_opencode_when_it_charges_else_the_price_table() {
        let mut u = Usage { input: 1_000_000, output: 1_000_000, model: Some("github-copilot/claude-haiku-5.5".into()), ..Default::default() };
        let (usd, est) = u.cost_usd().unwrap();
        assert!(est && (usd - 0.60).abs() < 1e-9, "{usd}"); // $0.10 in + $0.50 out
        u.cost = 0.25;
        assert_eq!(u.cost_usd(), Some((0.25, false)));
        let unknown = Usage { input: 5, model: Some("x/unknown".into()), ..Default::default() };
        assert_eq!(unknown.cost_usd(), None);
    }
}
