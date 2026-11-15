//! `codetypera2a models` (also offered by `init`): choose the model of each agent from the models
//! opencode actually has, in a small terminal UI. Choices are saved as `agents.<name>.model`
//! (`"provider/model"`, optional `#variant`) in `opencode.jsonc`, which opencode applies to every
//! agent, including those defined by markdown files; the dispatcher passes it on when it opens a
//! session. Recommendations are by role: cheap and fast for light work and balanced for everyday coding;
//! deep (the strongest model) is only for agents the user opts in with `models deep`.
mod config;
mod deep;
mod tiers;
mod tui;

use config::repair_unavailable;
pub use config::{current_models, load_config, sync_agent_files};
pub use deep::{DeepList, effective as deep_effective};
pub use tiers::{claude_alias, effective_tier, price};

use crate::{opencode, paths};
use config::{apply_models, frontmatter_model, save_config};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use tiers::price_hint;
use tui::{AgentEntry, AgentRow, App, build_rows, run_tui};

/// Agents to configure: everything opencode lists (not hidden), else the installed templates.
fn agent_list(root: &Path) -> Vec<AgentEntry> {
    if let Ok(list) = opencode::agents(None) {
        let v: Vec<AgentEntry> = list
            .into_iter()
            .filter(|a| !a.hidden)
            .map(|a| (a.id, a.description, a.model.as_ref().map(model_string)))
            .collect();
        if !v.is_empty() {
            return v;
        }
    }
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root.join("agents")) {
        for e in rd.flatten() {
            if let Some(n) = e.file_name().to_string_lossy().strip_suffix(".md") {
                v.push((n.to_string(), String::new(), frontmatter_model(&e.path())));
            }
        }
    }
    v
}

/// `{providerID, id, variant?}` -> `provider/id[#variant]`.
fn model_string(m: &Value) -> String {
    let base = format!(
        "{}/{}",
        m["providerID"].as_str().unwrap_or(""),
        m["id"].as_str().unwrap_or("")
    );
    match m["variant"].as_str().filter(|v| !v.is_empty()) {
        Some(v) => format!("{base}#{v}"),
        None => base,
    }
}
/// After `init`: the installed agent files that name a model this opencode does not have. Best
/// effort (silent when opencode cannot be asked).
pub fn availability_note(root: &Path) -> Option<String> {
    let available = opencode::models().ok()?;
    let mut missing = Vec::new();
    for e in std::fs::read_dir(root.join("agents")).ok()?.flatten() {
        let name = e
            .file_name()
            .to_string_lossy()
            .strip_suffix(".md")?
            .to_string();
        if let Some(m) = frontmatter_model(&e.path()) {
            if !available
                .iter()
                .any(|a| *a == m.split('#').next().unwrap_or(&m))
            {
                missing.push(format!("{name} ({m})"));
            }
        }
    }
    if missing.is_empty() {
        return None;
    }
    missing.sort();
    Some(format!(
        "{} agent(s) name a model this opencode does not have: {}. Run `codetypera2a models --recommended` to pick from the models you have.",
        missing.len(),
        missing.join(", ")
    ))
}

fn print_table(rows: &[AgentRow]) {
    println!(
        "{:<30} {:<52} {:<40} {}",
        "agent", "model", "recommended", "$/1M in / out"
    );
    for r in rows {
        let mut m = r.choice.clone().unwrap_or_else(|| "(default)".into());
        if r.unavailable && r.choice == r.current {
            m.push_str(" (not available here)");
        }
        println!(
            "{:<30} {:<52} {:<40} {}",
            r.agent,
            m,
            r.rec.clone().unwrap_or_default(),
            r.choice.as_deref().map(price_hint).unwrap_or("")
        );
    }
}

fn commit(root: &Path, mut cfg: Value, strict: bool, rows: &[AgentRow]) -> Result<(), String> {
    let changes: BTreeMap<String, Option<String>> = rows
        .iter()
        .filter(|r| r.choice != r.current)
        .map(|r| (r.agent.clone(), r.choice.clone()))
        .collect();
    if changes.is_empty() {
        println!("models: nothing changed");
        return Ok(());
    }
    apply_models(&mut cfg, &changes);
    if !strict {
        // comments or trailing commas would be lost: show what to add instead
        let mut only = Value::Object(Default::default());
        apply_models(&mut only, &changes);
        println!(
            "models: opencode.jsonc has comments, so it was left untouched. Add this to it:\n{}",
            serde_json::to_string_pretty(&only).unwrap_or_default()
        );
        return Ok(());
    }
    let path = save_config(root, &cfg)?;
    let synced = sync_agent_files(root, &current_models(&cfg));
    println!(
        "models: {} agent model(s) saved in {} (backup: opencode.jsonc.bak-a2a-models)",
        changes.len(),
        path.display()
    );
    for note in policy_violations(root, &changes) {
        println!("models: note: {note}");
    }
    println!(
        "models: {synced} agent file(s) updated too (opencode reads the model from the agent file first)"
    );
    println!(
        "models: restart opencode (or `opencode reload`); new agent sessions use these models"
    );
    Ok(())
}

/// Chosen models that the installed `a2a/model-policy.json` would flag in `check-models`.
fn policy_violations(root: &Path, changes: &BTreeMap<String, Option<String>>) -> Vec<String> {
    let Ok(policy) = crate::model_guard::load_policy(&root.join("a2a").join("model-policy.json"))
    else {
        return vec![];
    };
    changes
        .iter()
        .filter_map(|(agent, m)| {
            let m = m.as_ref()?;
            let c = crate::model_guard::check(m, Some(&policy));
            (!c.ok).then(|| format!("{agent}: {m} is not allowed by your model-policy.json ({}); `check-models` will flag it", c.reason.unwrap_or_default()))
        })
        .collect()
}

/// What `--recommended` sets for a row: its recommendation; a shipped agent whose model is not
/// installed and has no recommendation loses it; anything else is left as it is.
fn recommended_choice(row: &AgentRow) -> Option<String> {
    match (&row.rec, row.unavailable && row.tier.is_some()) {
        (Some(rec), _) => Some(rec.clone()),
        (None, true) => None,
        (None, false) => row.choice.clone(),
    }
}

/// Repairs the installed agent files against `available` and returns `<prefix>: <line>` per change.
pub fn repaired_files(
    root: &Path,
    available: &[String],
    prefix: &str,
    deep: &DeepList,
) -> Vec<String> {
    let overrides = load_config(root)
        .map(|(cfg, _)| current_models(&cfg))
        .unwrap_or_default();
    let mut lines: Vec<String> = repair_unavailable(root, available, &overrides, deep)
        .into_iter()
        .map(|line| format!("{prefix}: {line}"))
        .collect();
    let stale = config::stale_overrides(&overrides, available);
    if !stale.is_empty() {
        lines.push(format!(
            "{prefix}: note: opencode.jsonc names models this opencode does not have: {}; run `codetypera2a models` to choose again",
            stale.join(", ")
        ));
    }
    lines
}

/// Shared by `codetypera2a models` and `init`. `interactive`: show the UI when attached to a terminal.
pub fn configure(
    root: &Path,
    recommended: bool,
    show: bool,
    interactive: bool,
) -> Result<(), String> {
    let on_terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if !recommended && !show && !(interactive && on_terminal) {
        println!(
            "models: not a terminal; run `codetypera2a models` to choose models, or `codetypera2a models --recommended`"
        );
        return Ok(());
    }
    let available = opencode::models()?;
    let (cfg, strict) = load_config(root)?;
    let deep = deep_effective();
    let mut rows = build_rows(agent_list(root), &current_models(&cfg), &available, &deep);
    if rows.is_empty() {
        return Err("no agents found (run `codetypera2a init` first)".into());
    }
    if show {
        print_table(&rows);
        return Ok(());
    }
    if recommended {
        for row in rows.iter_mut() {
            row.choice = recommended_choice(row);
        }
        print_table(&rows);
        let committed = commit(root, cfg, strict, &rows);
        for line in repaired_files(root, &available, "models", &deep) {
            println!("{line}");
        }
        return committed;
    }
    let mut app = App::new(rows, available);
    run_tui(&mut app).map_err(|e| format!("terminal error: {e}"))?;
    if app.save {
        commit(root, cfg, strict, &app.rows)
    } else {
        println!("models: quit without saving");
        Ok(())
    }
}

fn config_root(args: &[String]) -> PathBuf {
    match args.iter().position(|a| a == "--dir") {
        Some(i) => args
            .get(i + 1)
            .map(PathBuf::from)
            .unwrap_or_else(paths::config_dir),
        None => paths::config_dir(),
    }
}

/// `codetypera2a models [--recommended] [--show] [--dir <path>]`
pub fn run(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("deep") {
        return deep::run(&args[1..]);
    }
    let root = config_root(args);
    let flag = |f: &str| args.iter().any(|a| a == f);
    match configure(&root, flag("--recommended"), flag("--show"), true) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("models: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn avail(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn policy_violations_are_reported_not_enforced() {
        let dir = std::env::temp_dir().join(format!("a2a-pol-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a2a")).unwrap();
        std::fs::write(
            dir.join("a2a/model-policy.json"),
            r#"{"default":"p/ok","allow":{"p/ok":"cheap"},"denyPatterns":["opus"]}"#,
        )
        .unwrap();
        let ch = BTreeMap::from([
            (
                "architect".to_string(),
                Some("p/claude-opus-5.5".to_string()),
            ),
            ("brain".to_string(), Some("p/ok".to_string())),
            ("x".to_string(), None),
        ]);
        let v = policy_violations(&dir, &ch);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(
            v[0].starts_with("architect:") && v[0].contains("opus"),
            "{v:?}"
        );
        assert!(
            policy_violations(&dir.join("nowhere"), &ch).is_empty(),
            "no policy file, no notes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
