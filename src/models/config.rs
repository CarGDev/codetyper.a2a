use super::deep::DeepList;
use super::tiers::{effective_tier, recommend, tier_for};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The opencode config, plus whether it is plain JSON (a JSONC file with comments is not rewritten).
pub fn load_config(root: &Path) -> Result<(Value, bool), String> {
    let path = root.join("opencode.jsonc");
    if !path.exists() {
        return Ok((
            json!({ "$schema": "https://opencode.ai/config.json" }),
            true,
        ));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    match serde_json::from_str(&text) {
        Ok(v) => Ok((v, true)),
        Err(_) => Ok((
            crate::model_guard::read_jsonc(&path).unwrap_or(Value::Null),
            false,
        )),
    }
}

/// `agents.<name>.model` strings (v2 key `agents`, v1 key `agent`).
pub fn current_models(cfg: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for key in ["agent", "agents"] {
        if let Some(map) = cfg.get(key).and_then(Value::as_object) {
            for (name, v) in map {
                if let Some(m) = v.get("model").and_then(Value::as_str) {
                    out.insert(name.clone(), m.to_string());
                }
            }
        }
    }
    out
}

/// Set (`Some`) or remove (`None`) each agent's model, keeping everything else in its entry.
pub fn apply_models(cfg: &mut Value, changes: &BTreeMap<String, Option<String>>) {
    let key = if cfg.get("agents").is_some_and(Value::is_object)
        || !cfg.get("agent").is_some_and(Value::is_object)
    {
        "agents"
    } else {
        "agent"
    };
    let Some(obj) = cfg.as_object_mut() else {
        return;
    };
    let table = obj.entry(key).or_insert_with(|| json!({}));
    let Some(table) = table.as_object_mut() else {
        return;
    };
    for (name, model) in changes {
        match model {
            Some(m) => {
                let entry = table.entry(name.clone()).or_insert_with(|| json!({}));
                if let Some(e) = entry.as_object_mut() {
                    e.insert("model".into(), json!(m));
                }
            }
            None => {
                let empty = table.get_mut(name).and_then(Value::as_object_mut).map(|e| {
                    e.remove("model");
                    e.is_empty()
                });
                if empty == Some(true) {
                    table.remove(name);
                }
            }
        }
    }
    if table.is_empty() {
        obj.remove(key);
    }
}

/// `text` with the frontmatter `model:` line set to `model` (variant dropped); `None` when the file
/// has no frontmatter model line or already names it.
fn set_frontmatter_model(text: &str, model: &str) -> Option<String> {
    let base = model.split('#').next().unwrap_or(model);
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let (fm, tail) = rest.split_at(end);
    let line = fm.lines().find(|l| l.starts_with("model:"))?;
    let new = format!("model: {base}");
    if line.trim() == new {
        return None;
    }
    Some(format!("---\n{}{tail}", fm.replacen(line, &new, 1)))
}

/// opencode takes an agent's model from its file before `opencode.jsonc`, so a model chosen here
/// (or kept across `init --force`) has to be written into the installed agent file as well.
/// Returns how many files changed.
pub fn sync_agent_files(root: &Path, models: &BTreeMap<String, String>) -> usize {
    models
        .iter()
        .filter_map(|(name, model)| {
            let path = root.join("agents").join(format!("{name}.md"));
            let text = std::fs::read_to_string(&path).ok()?;
            let updated = set_frontmatter_model(&text, model)?;
            std::fs::write(&path, updated).ok()
        })
        .count()
}

/// `text` without its frontmatter `model:` line; `None` when there is no such line.
pub(super) fn clear_frontmatter_model(text: &str) -> Option<String> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let (front, tail) = rest.split_at(end);
    let mut lines = front.lines().collect::<Vec<_>>();
    let at = lines.iter().position(|l| l.starts_with("model:"))?;
    lines.remove(at);
    Some(format!("---\n{}{tail}", lines.join("\n")))
}

/// Agent file names follow `^[a-z0-9][a-z0-9-]{0,62}$`.
pub(super) fn is_agent_name(name: &str) -> bool {
    let mut chars = name.chars();
    let starts_well = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    starts_well
        && name.len() <= MAX_AGENT_NAME_LEN
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

const MAX_AGENT_NAME_LEN: usize = 63;

/// The model `name`'s file should carry given what is installed: the recommendation, or none.
fn repaired_model(name: &str, available: &[String], deep: &DeepList) -> Option<String> {
    effective_tier(name, deep).and_then(|tier| recommend(tier, available))
}

/// `agent (model)` for each override in `opencode.jsonc` that names a model this opencode does not have.
pub fn stale_overrides(overrides: &BTreeMap<String, String>, available: &[String]) -> Vec<String> {
    overrides
        .iter()
        .filter(|(_, model)| {
            let base = model.split('#').next().unwrap_or(model);
            !available.iter().any(|m| m == base)
        })
        .map(|(agent, model)| format!("{agent} ({model})"))
        .collect()
}

/// Rewrites the installed agent files of shipped agents whose model this opencode does not have:
/// to the recommendation for the agent's tier, or the model line is removed when nothing fits.
/// A model the user chose in `opencode.jsonc` (`overrides`) is left alone while it is installed.
/// Only regular `<root>/agents/<name>.md` files with a valid name are touched; `opencode.jsonc` is
/// never written here. Returns one `name: old -> new|default` line per rewritten file.
pub fn repair_unavailable(
    root: &Path,
    available: &[String],
    overrides: &BTreeMap<String, String>,
    deep: &DeepList,
) -> Vec<String> {
    let is_installed = |model: &str| {
        let base = model.split('#').next().unwrap_or(model);
        available.iter().any(|m| m == base)
    };
    let Ok(entries) = std::fs::read_dir(root.join("agents")) else {
        return Vec::new();
    };
    let mut lines: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let name = file_name.strip_suffix(".md").filter(|n| is_agent_name(n))?;
            tier_for(name)?;
            let path = entry.path();
            if !std::fs::symlink_metadata(&path).ok()?.is_file() {
                return None;
            }
            let current = frontmatter_model(&path)?;
            let keeps_override = overrides.get(name).is_some_and(|m| is_installed(m));
            if keeps_override || is_installed(&current) {
                return None;
            }
            let text = std::fs::read_to_string(&path).ok()?;
            let replacement = repaired_model(name, available, deep);
            let updated = match &replacement {
                Some(model) => set_frontmatter_model(&text, model)?,
                None => clear_frontmatter_model(&text)?,
            };
            std::fs::write(&path, updated).ok()?;
            Some(format!(
                "{name}: {current} -> {}",
                replacement.as_deref().unwrap_or("default")
            ))
        })
        .collect();
    lines.sort();
    lines
}

pub(super) fn save_config(root: &Path, cfg: &Value) -> Result<PathBuf, String> {
    let path = root.join("opencode.jsonc");
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    if path.exists() {
        std::fs::copy(&path, root.join("opencode.jsonc.bak-a2a-models"))
            .map_err(|e| format!("cannot back up {}: {e}", path.display()))?;
    }
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n")
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

/// The `model:` line of an agent file's frontmatter.
pub(super) fn frontmatter_model(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let fm = text.splitn(3, "---\n").nth(1)?;
    fm.lines()
        .find_map(|l| l.strip_prefix("model:"))
        .map(|m| m.trim().trim_matches('"').to_string())
        .filter(|m| !m.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::tui::build_rows;
    fn avail(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_override_naming_a_missing_model_is_reported_by_agent() {
        let overrides = BTreeMap::from([
            ("build".to_string(), "x/gone#max".to_string()),
            ("brain".to_string(), "x/here".to_string()),
            ("plan".to_string(), "y/gone".to_string()),
        ]);
        let stale = stale_overrides(&overrides, &avail(&["x/here"]));
        assert_eq!(stale, vec!["build (x/gone#max)", "plan (y/gone)"]);
        assert!(stale_overrides(&BTreeMap::new(), &avail(&["x/here"])).is_empty());
    }

    #[test]
    fn the_model_line_can_be_cleared() {
        let text = "---\ndescription: x\nmodel: a/b\nmode: subagent\n---\nbody model: keep\n";
        assert_eq!(
            clear_frontmatter_model(text).as_deref(),
            Some("---\ndescription: x\nmode: subagent\n---\nbody model: keep\n")
        );
        assert_eq!(
            clear_frontmatter_model("---\ndescription: x\n---\nb\n"),
            None
        );
        assert_eq!(clear_frontmatter_model("no frontmatter"), None);
    }

    fn repair_dir(tag: &str, agents: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("a2a-repair-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        for (name, model) in agents {
            let text = format!("---\ndescription: d\nmodel: {model}\n---\nbody\n");
            std::fs::write(dir.join("agents").join(format!("{name}.md")), text).unwrap();
        }
        dir
    }

    fn model_of(dir: &Path, name: &str) -> Option<String> {
        frontmatter_model(&dir.join("agents").join(format!("{name}.md")))
    }

    #[test]
    fn an_uninstalled_model_is_replaced_by_the_recommendation_or_cleared() {
        let dir = repair_dir(
            "set",
            &[
                ("build", "github-copilot/claude-sonnet-5.5"),
                ("brain", "github-copilot/claude-haiku-5.5"),
                ("plan", "github-copilot/claude-sonnet-5.5"),
                ("my-own-agent", "github-copilot/claude-sonnet-5.5"),
            ],
        );
        let installed = avail(&["x/gpt-6-luna", "x/grok-4.5", "x/kimi-k3"]);
        let lines = repair_unavailable(&dir, &installed, &BTreeMap::new(), &DeepList::default());
        assert_eq!(model_of(&dir, "brain").as_deref(), Some("x/gpt-6-luna"));
        let build = model_of(&dir, "build").unwrap();
        assert!(installed.contains(&build), "{build}");
        assert_eq!(
            model_of(&dir, "my-own-agent").as_deref(),
            Some("github-copilot/claude-sonnet-5.5"),
            "an agent without a tier is not ours to change"
        );
        assert!(lines.iter().any(|l| l.starts_with("brain: ")), "{lines:?}");
        assert!(
            lines.iter().all(|l| !l.contains("my-own-agent")),
            "{lines:?}"
        );
        let cleared = repair_unavailable(
            &dir,
            &avail(&["x/unpriced"]),
            &BTreeMap::new(),
            &DeepList::default(),
        );
        assert_eq!(model_of(&dir, "plan"), None);
        assert!(
            cleared.iter().any(|l| l.ends_with("-> default")),
            "{cleared:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn installed_models_and_installed_overrides_are_kept() {
        let dir = repair_dir(
            "keep",
            &[
                ("build", "github-copilot/claude-sonnet-5.5"),
                ("brain", "x/mine#max"),
            ],
        );
        let installed = avail(&["github-copilot/claude-sonnet-5.5", "x/mine"]);
        let overrides = BTreeMap::from([("brain".to_string(), "x/mine".to_string())]);
        assert!(repair_unavailable(&dir, &installed, &overrides, &DeepList::default()).is_empty());
        assert_eq!(
            model_of(&dir, "build").as_deref(),
            Some("github-copilot/claude-sonnet-5.5")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn agent_names_follow_the_name_rule() {
        let long = "a".repeat(64);
        let longest = "a".repeat(63);
        for ok in ["build", "a", "0day", "git-historian", longest.as_str()] {
            assert!(is_agent_name(ok), "{ok}");
        }
        for bad in [
            "",
            "Build",
            "-x",
            "a_b",
            "../x",
            "a.b",
            "a b",
            "__proto__",
            long.as_str(),
        ] {
            assert!(!is_agent_name(bad), "{bad}");
        }
    }

    #[test]
    fn only_valid_agent_names_in_regular_files_are_written() {
        let dir = repair_dir("names", &[("build", "github-copilot/x")]);
        let agents = dir.join("agents");
        std::fs::write(agents.join("..bad.md"), "---\nmodel: a/b\n---\n").unwrap();
        let outside = dir.join("outside.md");
        std::fs::write(&outside, "---\nmodel: github-copilot/x\n---\n").unwrap();
        std::os::unix::fs::symlink(&outside, agents.join("brain.md")).unwrap();
        repair_unavailable(
            &dir,
            &avail(&["x/gpt-6-luna"]),
            &BTreeMap::new(),
            &DeepList::default(),
        );
        assert_eq!(model_of(&dir, "build").as_deref(), Some("x/gpt-6-luna"));
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "---\nmodel: github-copilot/x\n---\n",
            "a symlinked agent file is left alone"
        );
        assert_eq!(model_of(&dir, "..bad").as_deref(), Some("a/b"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn frontmatter_model_is_replaced_in_place() {
        let text = "---\ndescription: x\nmodel: github-copilot/claude-sonnet-5.5\nmode: subagent\n---\nbody model: keep\n";
        let out = set_frontmatter_model(text, "openai/gpt-5.6-luna#max").unwrap();
        assert_eq!(
            out,
            "---\ndescription: x\nmodel: openai/gpt-5.6-luna\nmode: subagent\n---\nbody model: keep\n"
        );
        assert_eq!(set_frontmatter_model(&out, "openai/gpt-5.6-luna"), None);
        assert_eq!(set_frontmatter_model("no frontmatter", "a/b"), None);
    }

    #[test]
    fn config_changes_are_merged_not_replaced() {
        let mut cfg = json!({"model":"x/y","agents":{"mentor":{"model":"a/b","temperature":0.2},"plan":{"description":"d"}}});
        let mut ch = BTreeMap::new();
        ch.insert("mentor".to_string(), None); // remove model, keep temperature
        ch.insert(
            "plan".to_string(),
            Some("github-copilot/claude-sonnet-5.5".to_string()),
        );
        ch.insert(
            "brain".to_string(),
            Some("github-copilot/claude-haiku-5.5#high".to_string()),
        );
        apply_models(&mut cfg, &ch);
        assert_eq!(cfg["model"], "x/y");
        assert_eq!(cfg["agents"]["mentor"], json!({"temperature":0.2}));
        assert_eq!(
            cfg["agents"]["plan"],
            json!({"description":"d","model":"github-copilot/claude-sonnet-5.5"})
        );
        assert_eq!(
            cfg["agents"]["brain"]["model"],
            "github-copilot/claude-haiku-5.5#high"
        );
        assert_eq!(
            current_models(&cfg).get("brain").map(String::as_str),
            Some("github-copilot/claude-haiku-5.5#high")
        );
        // removing the last field removes the entry, and an empty table is dropped
        let mut only = json!({"agents":{"a":{"model":"x/y"}}});
        apply_models(&mut only, &BTreeMap::from([("a".to_string(), None)]));
        assert!(only.get("agents").is_none());
        // a v1 `agent` table is kept as is
        let mut v1 = json!({"agent":{"plan":{"model":"p/m"}}});
        apply_models(
            &mut v1,
            &BTreeMap::from([("brain".to_string(), Some("p/n".to_string()))]),
        );
        assert_eq!(v1["agent"]["brain"]["model"], "p/n");
        assert!(v1.get("agents").is_none());
    }

    #[test]
    fn current_model_comes_from_the_agent_file_unless_the_config_overrides_it() {
        let models = avail(&[
            "github-copilot/claude-haiku-5.5",
            "github-copilot/claude-sonnet-5.5",
        ]);
        let agents = vec![
            (
                "brain".to_string(),
                "".to_string(),
                Some("github-copilot/claude-haiku-5.5".to_string()),
            ),
            (
                "plan".to_string(),
                "".to_string(),
                Some("github-copilot/claude-opus-5.5".to_string()),
            ), // not available here
            (
                "mentor".to_string(),
                "".to_string(),
                Some("github-copilot/claude-haiku-5.5".to_string()),
            ),
        ];
        let cfg = BTreeMap::from([(
            "mentor".to_string(),
            "github-copilot/claude-sonnet-5.5#high".to_string(),
        )]);
        let rows = build_rows(agents, &cfg, &models, &DeepList::default());
        let by = |n: &str| rows.iter().find(|r| r.agent == n).unwrap();
        assert_eq!(
            by("brain").current.as_deref(),
            Some("github-copilot/claude-haiku-5.5")
        );
        assert!(!by("brain").unavailable);
        assert!(
            by("plan").unavailable,
            "a model this opencode lacks is flagged"
        );
        assert_eq!(
            by("mentor").current.as_deref(),
            Some("github-copilot/claude-sonnet-5.5#high"),
            "config wins"
        );
        assert!(
            !by("mentor").unavailable,
            "a #variant does not make it unavailable"
        );
    }
}
