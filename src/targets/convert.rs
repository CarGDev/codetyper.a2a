//! opencode agent and command markdown -> Claude Code format. Pure: no IO, so every rule is unit tested.
//!
//! An opencode agent file has `mode`, `steps`, `permission`, a `tools` map of booleans and a
//! `provider/model` id; Claude Code wants `name`, `description`, a `tools` list, a model tier and one
//! of eight colors. Anything without an equivalent is dropped; the body is kept untouched.

/// One top-level frontmatter key: its inline value and the indented lines under it.
#[derive(Debug, PartialEq)]
struct Entry {
    key: String,
    value: String,
    block: Vec<String>,
}

/// opencode tool name -> Claude Code tool. A whole MCP server is `mcp__<server>__*`.
const TOOLS: [(&str, &str); 12] = [
    ("a2a_*", "mcp__a2a__*"),
    ("playwright_*", "mcp__playwright__*"),
    ("read", "Read"),
    ("grep", "Grep"),
    ("glob", "Glob"),
    ("bash", "Bash"),
    ("write", "Write"),
    ("edit", "Edit"),
    ("webfetch", "WebFetch"),
    ("todowrite", "TodoWrite"),
    ("task", "Agent"),
    ("question", "AskUserQuestion"),
];

/// opencode lets an agent use every tool its `tools` map does not name; Claude Code only the listed
/// ones. The companion servers `init` registers are therefore always listed, to keep that parity.
const COMPANIONS: [&str; 3] = ["mcp__codegraph__*", "mcp__engram__*", "mcp__context7__*"];

/// Model names Claude Code accepts for a subagent, matched by substring in a template's model id.
const TIERS: [&str; 3] = ["haiku", "sonnet", "opus"];

/// Used when neither the caller nor the template names a Claude tier; a subagent never inherits.
const FALLBACK_TIER: &str = "sonnet";

/// Colors Claude Code accepts.
const COLORS: [&str; 8] = [
    "red", "blue", "green", "yellow", "purple", "orange", "pink", "cyan",
];

/// Hue (degrees, exclusive upper bound) -> color name, for hex colors such as `#bb9af7`.
const HUES: [(f32, &str); 9] = [
    (15.0, "red"),
    (35.0, "orange"),
    (70.0, "yellow"),
    (165.0, "green"),
    (205.0, "cyan"),
    (255.0, "blue"),
    (300.0, "purple"),
    (330.0, "pink"),
    (360.0, "red"),
];

/// Greys have no hue worth naming.
const MIN_SATURATION: f32 = 0.15;

const ARGS_HINT: &str = "\"<arguments>\"";

fn parse(text: &str) -> Result<(Vec<Entry>, &str), String> {
    let rest = text.strip_prefix("---\n").ok_or("no frontmatter")?;
    let (end, body_at) = rest
        .find("\n---\n")
        .map(|i| (i, i + 5))
        .or_else(|| rest.strip_suffix("\n---").map(|r| (r.len(), rest.len())))
        .ok_or("frontmatter is not closed")?;
    let mut entries: Vec<Entry> = Vec::new();
    for line in rest[..end].lines().filter(|l| !l.trim().is_empty()) {
        let nested = line.starts_with(char::is_whitespace);
        if nested {
            entries
                .last_mut()
                .into_iter()
                .for_each(|e| e.block.push(line.to_string()));
            continue;
        }
        let Some((key, value)) = line.split_once(':').filter(|_| !line.starts_with('#')) else {
            continue;
        };
        entries.push(Entry {
            key: key.trim().to_string(),
            value: value.trim().to_string(),
            block: Vec::new(),
        });
    }
    Ok((entries, &rest[body_at..]))
}

fn get<'a>(entries: &'a [Entry], key: &str) -> Option<&'a Entry> {
    entries.iter().find(|e| e.key == key)
}

/// A YAML scalar as plain text: double quotes (JSON escapes) and single quotes removed.
fn scalar(v: &str) -> String {
    if let Some(s) = v
        .starts_with('"')
        .then(|| serde_json::from_str::<String>(v).ok())
        .flatten()
    {
        return s;
    }
    match v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        Some(inner) => inner.replace("''", "'"),
        None => v.to_string(),
    }
}

/// The description as a double-quoted YAML string (opencode accepts plain text with `: ` inside, YAML does not).
fn description(entries: &[Entry]) -> Result<String, String> {
    let e = get(entries, "description").ok_or("missing description")?;
    let folded = e
        .block
        .iter()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join(" ");
    let text = match e.value.as_str() {
        "" | ">" | "|" => folded,
        v => scalar(v),
    };
    if text.is_empty() {
        return Err("empty description".into());
    }
    serde_json::to_string(&text).map_err(|e| e.to_string())
}

/// The `Agent` tool for the subagents opencode's `permission.task` allows: `Agent(a, b)` limits an
/// agent started with `claude --agent` to those, `Agent` allows any, none allowed means no tool.
fn agent_tool(permission: &Entry) -> Option<String> {
    let indent = |l: &str| l.len() - l.trim_start().len();
    let start = permission.block.iter().position(|l| l.trim() == "task:")?;
    let base = indent(&permission.block[start]);
    let allowed: Vec<&str> = permission.block[start + 1..]
        .iter()
        .take_while(|l| indent(l) > base)
        .filter_map(|l| {
            l.trim()
                .split_once(':')
                .filter(|(_, rule)| rule.trim() == "allow")
                .map(|(name, _)| name.trim().trim_matches('"'))
        })
        .collect();
    match allowed.as_slice() {
        [] => None,
        names if names.contains(&"*") => Some("Agent".to_string()),
        names => Some(format!("Agent({})", names.join(", "))),
    }
}

fn tools(entry: &Entry, permission: Option<&Entry>) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in &entry.block {
        let Some((name, on)) = line.trim().split_once(':') else {
            continue;
        };
        let Some((_, mapped)) = TOOLS
            .iter()
            .find(|(k, _)| *k == name.trim().trim_matches('"'))
        else {
            continue;
        };
        if on.trim() == "true" && !out.iter().any(|t| t == mapped) {
            out.push(mapped.to_string());
        }
    }
    if let Some(spawn) = permission.and_then(agent_tool) {
        match out.iter().position(|t| t == "Agent") {
            Some(i) => out[i] = spawn,
            None => out.insert(0, spawn),
        }
    }
    out.extend(COMPANIONS.map(String::from));
    out.join(", ")
}

fn tier(model: &str) -> &'static str {
    let m = model.to_lowercase();
    TIERS
        .iter()
        .find(|t| m.contains(**t))
        .copied()
        .unwrap_or(FALLBACK_TIER)
}

/// The model line: the caller's `alias` (validated) wins, else the template's id mapped to a tier.
fn model_line(alias: Option<&str>, template: Option<&Entry>) -> Result<Option<String>, String> {
    let Some(alias) = alias else {
        return Ok(template.map(|e| format!("model: {}", tier(&scalar(&e.value)))));
    };
    if !TIERS.contains(&alias) {
        return Err(format!("unknown model alias: {alias}"));
    }
    Ok(Some(format!("model: {alias}")))
}

fn hue_name(r: f32, g: f32, b: f32) -> Option<&'static str> {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let delta = max - min;
    if max == 0.0 || delta / max < MIN_SATURATION {
        return None;
    }
    let sector = match max {
        m if m == r => ((g - b) / delta).rem_euclid(6.0),
        m if m == g => (b - r) / delta + 2.0,
        _ => (r - g) / delta + 4.0,
    };
    let hue = sector * 60.0;
    HUES.iter()
        .find(|(bound, _)| hue < *bound)
        .map(|(_, name)| *name)
}

fn color(value: &str) -> Option<&'static str> {
    let v = scalar(value).to_lowercase();
    if let Some(c) = COLORS.iter().find(|c| **c == v) {
        return Some(c);
    }
    let hex = v.strip_prefix('#').filter(|h| h.len() == 6)?;
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(f32::from);
    hue_name(channel(0)?, channel(2)?, channel(4)?)
}

fn assemble(front: &[String], body: &str) -> String {
    format!("---\n{}\n---\n{body}", front.join("\n"))
}

/// A Claude Code subagent file (`~/.claude/agents/<name>.md`) from an opencode agent file.
/// `model` is the Claude model name the caller chose for it (`haiku`, `sonnet` or `opus`).
pub fn agent(name: &str, text: &str, model: Option<&str>) -> Result<String, String> {
    let (entries, body) = parse(text)?;
    let mut front = vec![
        format!("name: {name}"),
        format!("description: {}", description(&entries)?),
    ];
    front.extend(
        get(&entries, "tools").map(|e| format!("tools: {}", tools(e, get(&entries, "permission")))),
    );
    front.extend(model_line(model, get(&entries, "model"))?);
    front.extend(
        get(&entries, "color")
            .and_then(|e| color(&e.value))
            .map(|c| format!("color: {c}")),
    );
    Ok(assemble(&front, body))
}

/// A Claude Code slash command (`~/.claude/commands/<name>.md`) from an opencode command file.
/// opencode's `agent:` runs the command in that agent; here the command asks Claude to delegate to it.
pub fn command(text: &str) -> Result<String, String> {
    let (entries, body) = parse(text)?;
    let mut front = vec![format!("description: {}", description(&entries)?)];
    if body.contains("$ARGUMENTS") {
        front.push(format!("argument-hint: {ARGS_HINT}"));
    }
    let Some(agent) = get(&entries, "agent").map(|e| scalar(&e.value)) else {
        return Ok(assemble(&front, body));
    };
    let preface = format!(
        "Delegate this to the `{agent}` subagent (Agent tool, subagent_type `{agent}`) and relay what it returns.\n\n"
    );
    Ok(assemble(
        &front,
        &format!("\n{preface}{}", body.trim_start()),
    ))
}

#[cfg(test)]
mod tests;
