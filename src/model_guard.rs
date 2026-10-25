//! model-guard: validate and resolve the model for an agent (policy in `model-policy.json`).
//! Model/agent strings are regex-validated and only ever travel in JSON bodies.
//!
//! `resolve_model` order: OC_MODEL_FORCE=1 + OC_MODEL > per-agent table (opencode.jsonc
//! `agent.<n>.model`, then policy.agentDefaults) > OC_MODEL > policy default. A denied value is
//! logged and skipped. Policy file missing/corrupt: fail open to the hard-coded default with a
//! loud log.
use regex::Regex;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const HARD_DEFAULT: &str = "github-copilot/claude-sonnet-5.5";

static MODEL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._#:/-]+$").unwrap());
static AGENT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._-]+$").unwrap());
static TRAILING_COMMA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",(\s*[}\]])").unwrap());

pub fn policy_path() -> PathBuf {
    crate::paths::policy_path()
}

pub fn config_path() -> PathBuf {
    crate::paths::opencode_config_path()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Policy {
    pub default: String,
    #[serde(default)]
    pub deny_patterns: Vec<String>,
    #[serde(default)]
    pub blocked: Vec<String>,
    pub allow: BTreeMap<String, String>,
    #[serde(default)]
    pub agent_defaults: BTreeMap<String, String>,
    #[serde(default)]
    pub read_only_agents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub ok: bool,
    pub tier: Option<String>,
    pub reason: Option<String>,
}

impl Check {
    fn ok(tier: &str) -> Self {
        Check {
            ok: true,
            tier: Some(tier.to_string()),
            reason: None,
        }
    }
    fn deny(reason: impl Into<String>) -> Self {
        Check {
            ok: false,
            tier: None,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    pub provider_id: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub model: String,
    pub provider_id: String,
    pub id: String,
    pub source: &'static str,
    pub denied: Option<String>,
}

#[derive(Default)]
pub struct ResolveOpts<'a> {
    pub env: Option<HashMap<String, String>>,
    pub table: Option<HashMap<String, String>>,
    pub policy_path: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub log: Option<&'a dyn Fn(&str)>,
}

/// Strip `//` and `/* */` comments and trailing commas from JSONC text.
pub fn strip_jsonc(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let (mut in_str, mut esc) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let n = chars.get(i + 1).copied();
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if c == '/' && n == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            out.push('\n');
        } else if c == '/' && n == Some('*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 1;
        } else {
            out.push(c);
        }
        i += 1;
    }
    TRAILING_COMMA.replace_all(&out, "$1").into_owned()
}

pub fn read_jsonc(file: &Path) -> Result<serde_json::Value, String> {
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    serde_json::from_str(&strip_jsonc(&text)).map_err(|e| e.to_string())
}

pub fn load_policy(file: &Path) -> Result<Policy, String> {
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    let p: Policy = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if p.default.is_empty() {
        return Err("model-policy.json: missing default/allow".into());
    }
    Ok(p)
}

pub fn split_model(model: &str) -> Split {
    let mut it = model.split('/');
    let provider_id = it.next().unwrap_or("").to_string();
    Split {
        provider_id,
        id: it.collect::<Vec<_>>().join("/"),
    }
}

pub fn check(model: &str, policy: Option<&Policy>) -> Check {
    if model.is_empty() {
        return Check::deny("empty or non-string model");
    }
    if !MODEL_RE.is_match(model) {
        return Check::deny("invalid characters in model string");
    }
    if !model.contains('/') {
        return Check::deny("model must look like provider/model");
    }
    let loaded;
    let policy = match policy {
        Some(p) => p,
        None => match load_policy(&policy_path()) {
            Ok(p) => {
                loaded = p;
                &loaded
            }
            Err(e) => return Check::deny(format!("policy unreadable: {e}")),
        },
    };
    let base = model.split('#').next().unwrap_or(model);
    for pat in &policy.deny_patterns {
        match Regex::new(&format!("(?i){pat}")) {
            Ok(re) if re.is_match(base) => {
                return Check::deny(format!("denied by pattern /{pat}/"));
            }
            Ok(_) => {}
            Err(e) => return Check::deny(format!("policy unreadable: {e}")),
        }
    }
    if policy.blocked.iter().any(|b| b == base) {
        return Check::deny("blocked by decision (above sonnet tier or pending approval)");
    }
    if let Some(tier) = policy.allow.get(base) {
        return Check::ok(tier);
    }
    if LOCAL_PROVIDERS.iter().any(|p| base.starts_with(p)) {
        return Check::ok("cheap");
    }
    Check::deny("unknown model (not on the allowlist)")
}

/// Providers `init` writes for Ollama hosts (`ollama`, `ollama-<host>`): free and local, so the cheap tier.
const LOCAL_PROVIDERS: [&str; 2] = ["ollama/", "ollama-"];

/// Per-agent models from `agent.<name>.model` in opencode.jsonc; empty on any failure.
pub fn config_table(file: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Ok(v) = read_jsonc(file) {
        if let Some(agents) = v
            .get("agents")
            .or_else(|| v.get("agent"))
            .and_then(|a| a.as_object())
        {
            for (k, v) in agents {
                if let Some(m) = v.get("model").and_then(|m| m.as_str()) {
                    out.insert(k.clone(), m.to_string());
                }
            }
        }
    }
    out
}

fn resolved(model: &str, source: &'static str, denied: &[String]) -> Resolved {
    let s = split_model(model);
    Resolved {
        model: model.to_string(),
        provider_id: s.provider_id,
        id: s.id,
        source,
        denied: (!denied.is_empty()).then(|| denied.join("; ")),
    }
}

pub fn resolve_model(agent: &str, opts: ResolveOpts) -> Result<Resolved, String> {
    let env = opts.env.unwrap_or_else(|| std::env::vars().collect());
    let log = |m: &str| match opts.log {
        Some(f) => f(m),
        None => eprintln!("[model-guard] {m}"),
    };
    if !AGENT_RE.is_match(agent) {
        return Err(format!("agent name has invalid characters: {agent:?}"));
    }
    let pp = opts.policy_path.unwrap_or_else(policy_path);
    let policy = match load_policy(&pp) {
        Ok(p) => p,
        Err(e) => {
            log(&format!(
                "WARNING policy unavailable ({e}); FAILING OPEN to {HARD_DEFAULT}"
            ));
            return Ok(resolved(HARD_DEFAULT, "default-nopolicy", &[]));
        }
    };
    let table = opts
        .table
        .unwrap_or_else(|| config_table(&opts.config_path.unwrap_or_else(config_path)));
    let from_env = env.get("OC_MODEL").filter(|s| !s.is_empty());
    let force = env.get("OC_MODEL_FORCE").map(String::as_str) == Some("1");

    let mut candidates: Vec<(String, &'static str)> = Vec::new();
    if let (true, Some(m)) = (force, from_env) {
        candidates.push((m.clone(), "env-force"));
    }
    if let Some(m) = table.get(agent).filter(|s| !s.is_empty()) {
        candidates.push((m.clone(), "table"));
    }
    if let Some(m) = policy.agent_defaults.get(agent).filter(|s| !s.is_empty()) {
        candidates.push((m.clone(), "table"));
    }
    if let (false, Some(m)) = (force, from_env) {
        candidates.push((m.clone(), "env"));
    }
    candidates.push((policy.default.clone(), "default"));

    let mut denied = Vec::new();
    for (m, source) in candidates {
        let c = check(&m, Some(&policy));
        if c.ok {
            let base = m.split('#').next().unwrap_or(&m);
            return Ok(resolved(base, source, &denied));
        }
        let msg = format!(
            "{agent}: refused {source} model {m:?}: {}",
            c.reason.unwrap_or_default()
        );
        log(&msg);
        denied.push(msg);
    }
    log(&format!(
        "WARNING no allowed model for {agent}; FAILING OPEN to {HARD_DEFAULT}"
    ));
    Ok(resolved(HARD_DEFAULT, "default-nopolicy", &denied))
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "github-copilot/";
    fn m(s: &str) -> String {
        format!("{P}{s}")
    }
    fn repo_policy() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("model-policy.json")
    }
    fn chk(model: &str) -> Check {
        check(model, Some(&load_policy(&repo_policy()).unwrap()))
    }
    fn quiet(_: &str) {}

    #[test]
    fn ollama_models_are_the_cheap_tier_even_without_an_allowlist_entry() {
        for model in [
            "ollama/qwen3.8:27b-mtp-q4_K_M",
            "ollama-100-99-194-92/gemma4:26b",
        ] {
            let c = chk(model);
            assert!(c.ok, "{model}: {:?}", c.reason);
            assert_eq!(c.tier.as_deref(), Some("cheap"), "{model}");
        }
        assert!(!chk("ollamafake/x").ok, "only the two provider prefixes");
    }
    fn r(
        agent: &str,
        env: &[(&str, String)],
        table: Option<HashMap<String, String>>,
    ) -> Result<Resolved, String> {
        resolve_model(
            agent,
            ResolveOpts {
                env: Some(
                    env.iter()
                        .map(|(k, v)| (k.to_string(), v.clone()))
                        .collect(),
                ),
                table: Some(table.unwrap_or_default()),
                policy_path: Some(repo_policy()),
                log: Some(&quiet),
                ..Default::default()
            },
        )
    }

    #[test]
    fn denies_and_allows() {
        // not on the allowlist (unknown or pricey on purpose), or not even a valid model string
        let deny = [
            m("claude-fable-5.1"),
            m("gpt-6-astra"),
            m("gpt-5.6-sol"),
            m("gemini-4-argon"),
            m("gpt-9-foo"),
            m("claude-haiku-5.5; rm -rf /"),
            "../x".into(),
            "a b".into(),
            "$(id)".into(),
            "".into(),
            "noslash".into(),
        ];
        for d in &deny {
            assert!(!chk(d).ok, "should deny: {d:?}");
        }
        // every default the agent templates use, and the other models the screen can recommend
        let allow = [
            m("claude-haiku-5.5"),
            m("claude-haiku-4.5"),
            m("gpt-6-luna"),
            m("gpt-5.6-luna"),
            m("mai-code-1.1-flash"),
            m("gpt-5.4-mini"),
            m("gemini-3.7-flash"),
            m("gemini-3.8-flash"),
            m("claude-sonnet-5"),
            m("claude-sonnet-5.5"),
            m("gpt-5.6-terra"),
            m("gpt-6.1-sol"),
            m("gpt-6-sol"),
            m("gpt-5.3-codex"),
            m("claude-opus-5.5"),
            m("claude-opus-5"),
            "GITHUB-COPILOT/CLAUDE-OPUS-5".to_lowercase(),
            m("claude-haiku-5.5#high"),
        ];
        for a in &allow {
            assert!(chk(a).ok, "should allow: {a}");
        }
        assert_eq!(chk(&m("claude-haiku-5.5")).tier.as_deref(), Some("cheap"));
        assert_eq!(chk(&m("claude-sonnet-5.5")).tier.as_deref(), Some("sonnet"));
        assert_eq!(chk(&m("claude-opus-5.5")).tier.as_deref(), Some("deep"));
        assert!(chk(&m("gpt-9-foo")).reason.unwrap().contains("unknown"));
    }

    #[test]
    fn deny_patterns_and_blocked_still_work() {
        let policy: Policy = serde_json::from_str(
            r#"{"default":"p/ok","allow":{"p/ok":"cheap","p/opus-x":"deep","p/held":"cheap"},"denyPatterns":["opus"],"blocked":["p/held"]}"#,
        )
        .unwrap();
        assert!(check("p/ok", Some(&policy)).ok);
        assert!(
            check("P/OPUS-X", Some(&policy))
                .reason
                .unwrap()
                .contains("denied by pattern")
        );
        assert!(
            check("p/held", Some(&policy))
                .reason
                .unwrap()
                .contains("blocked")
        );
    }

    #[test]
    fn resolves_in_order() {
        assert_eq!(
            r("summarizer", &[], None).unwrap().model,
            m("claude-haiku-5.5")
        );
        assert_eq!(r("summarizer", &[], None).unwrap().source, "table");
        assert_eq!(
            r("code-debugger", &[], None).unwrap().model,
            m("claude-sonnet-5.5")
        );
        let n = r("nobody-agent", &[], None).unwrap();
        assert_eq!(
            (n.model.as_str(), n.source),
            (m("claude-sonnet-5.5").as_str(), "default")
        );
        let env = [("OC_MODEL", m("claude-haiku-4.5"))];
        let e = r("nobody-agent", &env, None).unwrap();
        assert_eq!(
            (e.model.as_str(), e.source),
            (m("claude-haiku-4.5").as_str(), "env")
        );
        assert_eq!(
            r("summarizer", &[("OC_MODEL", m("claude-sonnet-5"))], None)
                .unwrap()
                .model,
            m("claude-haiku-5.5")
        );
        let force = [
            ("OC_MODEL", m("claude-haiku-4.5")),
            ("OC_MODEL_FORCE", "1".into()),
        ];
        assert_eq!(r("summarizer", &force, None).unwrap().source, "env-force");
        let bad = [("OC_MODEL", m("gpt-9-foo")), ("OC_MODEL_FORCE", "1".into())];
        let d = r("summarizer", &bad, None).unwrap();
        assert_eq!(d.model, m("claude-haiku-5.5"));
        assert!(d.denied.unwrap().contains("gpt-9-foo"));
        assert_eq!(
            r("brain", &[("OC_MODEL", "github-copilot/x;y".into())], None)
                .unwrap()
                .model,
            m("claude-haiku-5.5")
        );
        let t = |v: String| Some(HashMap::from([("summarizer".to_string(), v)]));
        assert_eq!(
            r("summarizer", &[], t(m("gpt-9-foo"))).unwrap().model,
            m("claude-haiku-5.5")
        );
        assert_eq!(
            r("summarizer", &[], t(m("claude-haiku-4.5")))
                .unwrap()
                .model,
            m("claude-haiku-4.5")
        );
        assert!(
            r("bad agent;x", &[], None)
                .unwrap_err()
                .contains("agent name")
        );
    }

    #[test]
    fn fails_open_without_policy() {
        let res = resolve_model(
            "brain",
            ResolveOpts {
                env: Some(HashMap::new()),
                policy_path: Some("/nonexistent/policy.json".into()),
                log: Some(&quiet),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            (res.model.as_str(), res.source),
            (m("claude-sonnet-5.5").as_str(), "default-nopolicy")
        );
    }

    #[test]
    fn splits_and_strips() {
        assert_eq!(
            split_model(&m("claude-haiku-5.5")),
            Split {
                provider_id: "github-copilot".into(),
                id: "claude-haiku-5.5".into()
            }
        );
        assert_eq!(split_model("a/b/c").id, "b/c");
        let v: serde_json::Value = serde_json::from_str(&strip_jsonc(
            "{\"a\": \"x//y\", // c\n \"b\": [1,/* k */2,],}",
        ))
        .unwrap();
        assert_eq!(v["a"], "x//y");
        assert_eq!(v["b"][1], 2);
    }
}
