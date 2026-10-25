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

/// Model tiers by substring of the model id; anything else (gpt-*, ...) follows the session's model.
const TIERS: [&str; 3] = ["haiku", "sonnet", "opus"];

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
        .unwrap_or("inherit")
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
pub fn agent(name: &str, text: &str) -> Result<String, String> {
    let (entries, body) = parse(text)?;
    let mut front = vec![
        format!("name: {name}"),
        format!("description: {}", description(&entries)?),
    ];
    front.extend(
        get(&entries, "tools").map(|e| format!("tools: {}", tools(e, get(&entries, "permission")))),
    );
    front.extend(get(&entries, "model").map(|e| format!("model: {}", tier(&scalar(&e.value)))));
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
mod tests {
    use super::*;

    const ORCH: &str = "---\ndescription: The agent you talk to: it sends tasks, then reports.\nmode: primary\nmodel: github-copilot/claude-sonnet-5.5\nsteps: 8\ntools:\n  \"a2a_*\": true\n  \"playwright_*\": false\n  bash: true\n  read: true\n  write: false\n  task: true\npermission:\n  task:\n    \"*\": deny\n    summarizer: allow\ncolor: \"#bb9af7\"\n---\n\nBody line 1.\nBody `model: keep`.\n";

    fn front(out: &str) -> Vec<String> {
        let rest = out.strip_prefix("---\n").expect("opens with ---");
        let end = rest.find("\n---\n").expect("closes with ---");
        rest[..end].lines().map(String::from).collect()
    }

    fn line<'a>(f: &'a [String], key: &str) -> Option<&'a str> {
        f.iter().find_map(|l| l.strip_prefix(&format!("{key}: ")))
    }

    /// The tools line split on commas outside parentheses (`Agent(a, b), Read`).
    fn split_tools(s: &str) -> Vec<&str> {
        let (mut out, mut depth, mut start) = (Vec::new(), 0, 0);
        for (i, c) in s.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    out.push(s[start..i].trim());
                    start = i + 1;
                }
                _ => {}
            }
        }
        out.push(s[start..].trim());
        out
    }

    #[test]
    fn parse_splits_keys_blocks_and_body() {
        let (entries, body) = parse(ORCH).unwrap();
        let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "description",
                "mode",
                "model",
                "steps",
                "tools",
                "permission",
                "color"
            ]
        );
        let tools = entries.iter().find(|e| e.key == "tools").unwrap();
        assert_eq!(tools.block.len(), 6);
        let perm = entries.iter().find(|e| e.key == "permission").unwrap();
        assert_eq!(perm.block.len(), 3);
        assert!(body.starts_with("\nBody line 1."));
        assert!(parse("no frontmatter").is_err());
        assert!(parse("---\nkey: x\nnever closed").is_err());
    }

    #[test]
    fn agent_gets_name_quoted_description_and_drops_opencode_keys() {
        let out = agent("orchestrator", ORCH).unwrap();
        let f = front(&out);
        assert_eq!(line(&f, "name"), Some("orchestrator"));
        assert_eq!(
            line(&f, "description"),
            Some("\"The agent you talk to: it sends tasks, then reports.\"")
        );
        for gone in ["mode", "steps", "permission"] {
            assert!(
                f.iter().all(|l| !l.starts_with(&format!("{gone}:"))),
                "{gone} kept: {f:?}"
            );
        }
        assert!(out.ends_with("\nBody line 1.\nBody `model: keep`.\n"));
    }

    #[test]
    fn agent_model_is_a_tier() {
        let with =
            |m: &str| agent("x", &format!("---\ndescription: d\nmodel: {m}\n---\nb\n")).unwrap();
        for (model, tier) in [
            ("github-copilot/claude-haiku-5.5", "haiku"),
            ("github-copilot/claude-sonnet-5.5", "sonnet"),
            ("github-copilot/claude-opus-5.5", "opus"),
            ("github-copilot/gpt-6.1-sol", "inherit"),
            ("OPENAI/Claude-Opus-5", "opus"),
        ] {
            assert_eq!(line(&front(&with(model)), "model"), Some(tier), "{model}");
        }
        let none = agent("x", "---\ndescription: d\n---\nb\n").unwrap();
        assert_eq!(line(&front(&none), "model"), None);
    }

    #[test]
    fn agent_tools_keep_only_enabled_ones_and_add_companions() {
        let f = front(&agent("orchestrator", ORCH).unwrap());
        let tools: Vec<&str> = line(&f, "tools").unwrap().split(", ").collect();
        assert_eq!(
            &tools[..4],
            ["mcp__a2a__*", "Bash", "Read", "Agent(summarizer)"]
        );
        assert!(
            tools.contains(&"mcp__engram__*")
                && tools.contains(&"mcp__codegraph__*")
                && tools.contains(&"mcp__context7__*")
        );
        assert!(
            !tools
                .iter()
                .any(|t| t.contains("playwright") || *t == "Write"),
            "{tools:?}"
        );
        let pw = agent("x", "---\ndescription: d\ntools:\n  \"playwright_*\": true\n  question: true\n  todowrite: true\n---\nb\n").unwrap();
        let t = line(&front(&pw), "tools").unwrap().to_string();
        assert!(
            t.starts_with("mcp__playwright__*, AskUserQuestion, TodoWrite"),
            "{t}"
        );
        let bare = agent("x", "---\ndescription: d\n---\nb\n").unwrap();
        assert_eq!(
            line(&front(&bare), "tools"),
            None,
            "no tools map means inherit everything"
        );
    }

    #[test]
    fn task_permissions_become_an_agent_allow_list() {
        let with = |perm: &str, tools: &str| {
            agent(
                "x",
                &format!("---\ndescription: d\ntools:\n{tools}permission:\n{perm}---\nb\n"),
            )
            .unwrap()
        };
        let allow = "  task:\n    \"*\": deny\n    code-reviewer: allow\n    doc-lookup: allow\n    test-runner: deny\n";
        let t = |out: String| line(&front(&out), "tools").unwrap().to_string();
        assert!(
            t(with(allow, "  read: true\n")).contains("Agent(code-reviewer, doc-lookup), "),
            "allow-list without the task tool"
        );
        assert!(
            t(with(allow, "  task: true\n")).starts_with("Agent(code-reviewer, doc-lookup), "),
            "replaces the plain Agent"
        );
        assert!(
            t(with("  task:\n    \"*\": allow\n", "  read: true\n")).starts_with("Agent, "),
            "everything allowed"
        );
        assert!(
            !t(with("  task:\n    \"*\": deny\n", "  read: true\n")).contains("Agent"),
            "nothing allowed"
        );
        assert!(
            !t(with("  edit:\n    \"*\": allow\n", "  read: true\n")).contains("Agent"),
            "only the task block counts"
        );
    }

    #[test]
    fn agent_color_is_a_claude_color_or_dropped() {
        let with =
            |c: &str| agent("x", &format!("---\ndescription: d\ncolor: {c}\n---\nb\n")).unwrap();
        for (given, want) in [
            ("\"#bb9af7\"", Some("purple")),
            ("\"#2ac3de\"", Some("cyan")),
            ("\"#7aa2f7\"", Some("blue")),
            ("\"#9ece6a\"", Some("green")),
            ("\"#f7768e\"", Some("red")),
            ("\"#ff9e64\"", Some("orange")),
            ("red", Some("red")),
            ("accent", None),
            ("\"#888888\"", None),
        ] {
            assert_eq!(line(&front(&with(given)), "color"), want, "{given}");
        }
    }

    #[test]
    fn description_already_quoted_is_not_double_quoted() {
        let out = agent("x", "---\ndescription: \"say \\\"hi\\\"\"\n---\nb\n").unwrap();
        assert_eq!(
            line(&front(&out), "description"),
            Some("\"say \\\"hi\\\"\"")
        );
        let single = agent("x", "---\ndescription: 'it''s'\n---\nb\n").unwrap();
        assert_eq!(line(&front(&single), "description"), Some("\"it's\""));
        assert!(
            agent("x", "---\nmode: primary\n---\nb\n").is_err(),
            "description is required"
        );
    }

    #[test]
    fn command_delegates_to_the_named_agent_and_hints_arguments() {
        let src = "---\ndescription: Start a plan: explore first.\nagent: plan\n---\n\nStart.\n\nArguments: `$ARGUMENTS`\n";
        let out = command(src).unwrap();
        let f = front(&out);
        assert_eq!(
            line(&f, "description"),
            Some("\"Start a plan: explore first.\"")
        );
        assert_eq!(line(&f, "argument-hint"), Some("\"<arguments>\""));
        assert!(f.iter().all(|l| !l.starts_with("agent:")));
        let body = out.split("\n---\n").nth(1).unwrap();
        assert!(
            body.trim_start()
                .starts_with("Delegate this to the `plan` subagent"),
            "{body}"
        );
        assert!(body.contains("Arguments: `$ARGUMENTS`"));
        let plain = command("---\ndescription: d\n---\nno args here\n").unwrap();
        assert_eq!(line(&front(&plain), "argument-hint"), None);
        assert!(!plain.contains("Delegate"));
    }

    #[test]
    fn every_embedded_agent_and_command_converts_to_valid_frontmatter() {
        const COLORS: [&str; 8] = [
            "red", "blue", "green", "yellow", "purple", "orange", "pink", "cyan",
        ];
        const BUILTIN: [&str; 10] = [
            "Read",
            "Grep",
            "Glob",
            "Bash",
            "Write",
            "Edit",
            "WebFetch",
            "TodoWrite",
            "Agent",
            "AskUserQuestion",
        ];
        let (mut agents, mut commands) = (0, 0);
        for file in crate::cli::AGENTS.files() {
            let name = file.path().file_stem().unwrap().to_str().unwrap();
            let out = agent(name, file.contents_utf8().unwrap())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let f = front(&out);
            assert_eq!(line(&f, "name"), Some(name));
            assert!(
                line(&f, "description").is_some_and(|d| d.starts_with('"') && d.ends_with('"')),
                "{name}"
            );
            assert!(
                ["sonnet", "haiku", "opus", "inherit"]
                    .contains(&line(&f, "model").unwrap_or("inherit")),
                "{name}"
            );
            assert!(
                line(&f, "color").is_none_or(|c| COLORS.contains(&c)),
                "{name}"
            );
            for t in split_tools(line(&f, "tools").unwrap_or("Read")) {
                let t = t.split('(').next().unwrap();
                assert!(
                    BUILTIN.contains(&t) || (t.starts_with("mcp__") && t.ends_with("__*")),
                    "{name}: {t}"
                );
            }
            assert!(
                f.iter().all(|l| !l.starts_with("mode:")
                    && !l.starts_with("permission:")
                    && !l.starts_with("steps:")),
                "{name}"
            );
            agents += 1;
        }
        for file in crate::cli::COMMANDS.files() {
            let out = command(file.contents_utf8().unwrap())
                .unwrap_or_else(|e| panic!("{:?}: {e}", file.path()));
            assert!(line(&front(&out), "description").is_some());
            commands += 1;
        }
        assert!(
            agents >= 21 && commands >= 3,
            "{agents} agents, {commands} commands"
        );
    }
}
