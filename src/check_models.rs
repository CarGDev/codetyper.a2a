//! check-models: lint models against model-policy.json.
//!   check-models [--config opencode.jsonc] [--policy model-policy.json] [--agents agents/] [--strict]
//! Reads: opencode.jsonc (model, small_model, agent.<n>.model), agents/*.md `model:` frontmatter, OC_MODEL.
//! Exit 0 clean | 1 violations | 2 policy/config unreadable. --strict also fails on agents without a table entry.
use crate::model_guard::{self as guard, Policy};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

fn opt(args: &[String], name: &str, def: PathBuf) -> PathBuf {
    match args.iter().position(|a| a == name) {
        Some(i) if args.get(i + 1).is_some() => PathBuf::from(&args[i + 1]),
        _ => def,
    }
}

pub fn run(args: &[String]) -> i32 {
        let config_path = opt(args, "--config", guard::config_path());
    let policy_path = opt(args, "--policy", guard::policy_path());
    let agents_dir = opt(args, "--agents", crate::paths::agent_dir());
    let strict = args.iter().any(|a| a == "--strict");

    let policy: Policy = match guard::load_policy(&policy_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("check-models: policy unreadable: {e}");
            return 2;
        }
    };
    let config = match guard::read_jsonc(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("check-models: config unreadable: {e}");
            return 2;
        }
    };

    let mut violations: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let verify = |violations: &mut Vec<String>, who: &str, model: &str| {
        let c = guard::check(model, Some(&policy));
        if !c.ok {
            violations.push(format!(
                "{who}: {model} -> {}",
                c.reason.clone().unwrap_or_default()
            ));
        }
        c
    };

    for key in ["model", "small_model"] {
        if let Some(m) = config
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            verify(&mut violations, &format!("opencode.jsonc {key}"), m);
        }
    }
    let mut table: BTreeMap<String, String> = BTreeMap::new();
    if let Some(agents) = config.get("agents").or_else(|| config.get("agent")).and_then(|a| a.as_object()) {
        for (name, v) in agents {
            if let Some(m) = v
                .get("model")
                .and_then(|m| m.as_str())
                .filter(|s| !s.is_empty())
            {
                table.insert(name.clone(), m.to_string());
                verify(&mut violations, &format!("agent.{name}.model"), m);
            }
        }
    }

    let mut files: Vec<String> = match std::fs::read_dir(&agents_dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|f| f.ends_with(".md"))
            .collect(),
        Err(e) => {
            eprintln!("check-models: agent dir unreadable: {e}");
            return 2;
        }
    };
    files.sort();

    let fm_re = Regex::new(r"(?s)\A---\n(.*?)\n---").unwrap();
    let model_re = Regex::new(r"(?m)^model:\s*(\S+)\s*$").unwrap();
    let mut front: BTreeMap<String, String> = BTreeMap::new();
    for f in &files {
        let name = f.trim_end_matches(".md").to_string();
        let text = std::fs::read_to_string(agents_dir.join(f)).unwrap_or_default();
        let Some(m) = fm_re
            .captures(&text)
            .and_then(|fm| model_re.captures(&fm[1]).map(|c| c[1].to_string()))
        else {
            continue;
        };
        let m = m.trim_matches(|c| c == '"' || c == '\'').to_string();
        verify(&mut violations, &format!("agent/{f} model"), &m);
        if let Some(t) = table.get(&name).filter(|t| **t != m) {
            violations.push(format!(
                "agent.{name}: {t} -> contradicts agent/{f} model {m}"
            ));
        }
        front.insert(name, m);
    }

    if let Ok(m) = std::env::var("OC_MODEL") {
        if !m.is_empty() {
            verify(&mut violations, "env OC_MODEL", &m);
        }
    }

    let cfg_model = config
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let effective = |name: &str| -> String {
        table
            .get(name)
            .or_else(|| front.get(name))
            .or_else(|| policy.agent_defaults.get(name))
            .map(String::as_str)
            .or(cfg_model)
            .unwrap_or(&policy.default)
            .to_string()
    };
    for name in &policy.read_only_agents {
        let m = effective(name);
        let c = guard::check(&m, Some(&policy));
        let tier = c.tier.as_deref().unwrap_or("");
        if c.ok && tier != "cheap" {
            violations.push(format!(
                "agent {name} (read-only): {m} -> tier {tier} is above cheap"
            ));
        }
    }

    let known: BTreeSet<String> = files
        .iter()
        .map(|f| f.trim_end_matches(".md").to_string())
        .collect();
    for name in table.keys() {
        if !known.contains(name)
            && !["plan", "build", "general", "explore"].contains(&name.as_str())
        {
            warnings.push(format!("agent.{name}: no agent/{name}.md"));
        }
    }
    for name in &known {
        if !table.contains_key(name) && !front.contains_key(name) {
            let msg = format!("agent {name}: no model table entry in opencode.jsonc");
            if strict {
                violations.push(msg)
            } else {
                warnings.push(msg)
            }
        }
    }

    for w in &warnings {
        println!("warn: {w}");
    }
    if !violations.is_empty() {
        for v in &violations {
            eprintln!("VIOLATION {v}");
        }
        eprintln!("check-models: {} violation(s)", violations.len());
        return 1;
    }
    println!(
        "check-models: OK ({} table entries, {} frontmatter models)",
        table.len(),
        front.len()
    );
    0
}
