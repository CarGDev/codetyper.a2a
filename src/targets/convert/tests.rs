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
    let out = agent("orchestrator", ORCH, None).unwrap();
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
    let with = |m: &str| {
        agent(
            "x",
            &format!("---\ndescription: d\nmodel: {m}\n---\nb\n"),
            None,
        )
        .unwrap()
    };
    for (model, tier) in [
        ("github-copilot/claude-haiku-5.5", "haiku"),
        ("github-copilot/claude-sonnet-5.5", "sonnet"),
        ("github-copilot/claude-opus-5.5", "opus"),
        ("github-copilot/gpt-6.1-sol", "sonnet"),
        ("OPENAI/Claude-Opus-5", "opus"),
    ] {
        assert_eq!(line(&front(&with(model)), "model"), Some(tier), "{model}");
    }
    let none = agent("x", "---\ndescription: d\n---\nb\n", None).unwrap();
    assert_eq!(line(&front(&none), "model"), None);
}

#[test]
fn agent_tools_keep_only_enabled_ones_and_add_companions() {
    let f = front(&agent("orchestrator", ORCH, None).unwrap());
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
    let pw = agent("x", "---\ndescription: d\ntools:\n  \"playwright_*\": true\n  question: true\n  todowrite: true\n---\nb\n", None).unwrap();
    let t = line(&front(&pw), "tools").unwrap().to_string();
    assert!(
        t.starts_with("mcp__playwright__*, AskUserQuestion, TodoWrite"),
        "{t}"
    );
    let bare = agent("x", "---\ndescription: d\n---\nb\n", None).unwrap();
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
            None,
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
    let with = |c: &str| {
        agent(
            "x",
            &format!("---\ndescription: d\ncolor: {c}\n---\nb\n"),
            None,
        )
        .unwrap()
    };
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
    let out = agent("x", "---\ndescription: \"say \\\"hi\\\"\"\n---\nb\n", None).unwrap();
    assert_eq!(
        line(&front(&out), "description"),
        Some("\"say \\\"hi\\\"\"")
    );
    let single = agent("x", "---\ndescription: 'it''s'\n---\nb\n", None).unwrap();
    assert_eq!(line(&front(&single), "description"), Some("\"it's\""));
    assert!(
        agent("x", "---\nmode: primary\n---\nb\n", None).is_err(),
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
        let out = agent(name, file.contents_utf8().unwrap(), None)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let f = front(&out);
        assert_eq!(line(&f, "name"), Some(name));
        assert!(
            line(&f, "description").is_some_and(|d| d.starts_with('"') && d.ends_with('"')),
            "{name}"
        );
        assert!(
            ["sonnet", "haiku"].contains(&line(&f, "model").unwrap_or("none")),
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
        agents >= 22 && commands >= 3,
        "{agents} agents, {commands} commands"
    );
}

#[test]
fn an_alias_wins_over_the_template_model_and_is_validated() {
    let text = "---\ndescription: d\nmodel: github-copilot/gpt-6.1-sol\n---\nb\n";
    let model_of = |alias: Option<&str>| {
        agent("x", text, alias).map(|out| line(&front(&out), "model").map(String::from))
    };
    assert_eq!(model_of(Some("sonnet")), Ok(Some("sonnet".into())));
    assert_eq!(model_of(Some("haiku")), Ok(Some("haiku".into())));
    assert_eq!(model_of(None), Ok(Some("sonnet".into())));
    for bad in ["inherit", "gpt-6.1-sol", "", "__proto__", "Sonnet"] {
        assert!(model_of(Some(bad)).is_err(), "{bad}");
    }
}

#[test]
fn every_agent_converted_with_its_tier_is_haiku_or_sonnet() {
    for file in crate::cli::AGENTS.files() {
        let name = file.path().file_stem().unwrap().to_str().unwrap();
        let alias = crate::models::effective_tier(name, &crate::models::DeepList::default())
            .map(crate::models::claude_alias);
        let out = agent(name, file.contents_utf8().unwrap(), alias).unwrap();
        let front = front(&out);
        let model = line(&front, "model");
        assert!(
            matches!(model, Some("haiku" | "sonnet")),
            "{name}: {model:?}"
        );
    }
}
