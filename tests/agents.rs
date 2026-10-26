//! Read-only agents must keep a locked-down shell: `bash: true` (so they can run `git diff`) with a
//! `permission: bash` block that denies everything first and then allows only read-only commands.
use std::path::Path;

fn frontmatter(name: &str) -> String {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("agents")
            .join(format!("{name}.md")),
    )
    .unwrap();
    text.splitn(3, "---\n").nth(1).unwrap().to_string()
}

#[test]
fn read_only_agents_have_a_restricted_shell() {
    for name in ["code-reviewer", "security-scanner", "git-historian"] {
        let fm = frontmatter(name);
        assert!(
            fm.contains("  bash: true"),
            "{name}: bash must be enabled so git diff works"
        );
        assert!(
            fm.contains("  edit: false") && fm.contains("  write: false"),
            "{name} must not edit"
        );
        let bash = fm
            .split("  bash:\n")
            .nth(1)
            .unwrap_or_else(|| panic!("{name}: no permission.bash block"));
        let rules: Vec<&str> = bash.lines().take_while(|l| l.starts_with("    ")).collect();
        assert_eq!(
            rules.first().map(|r| r.trim()),
            Some("\"*\": deny"),
            "{name}: deny-all must come first"
        );
        assert!(
            rules
                .iter()
                .any(|r| r.contains("git diff*") && r.contains("allow")),
            "{name}: git diff must be allowed"
        );
        for risky in ["\"*|*\": deny", "\"*>*\": deny", "\"*;*\": deny"] {
            assert!(
                rules.iter().any(|r| r.trim() == risky),
                "{name}: missing {risky}"
            );
        }
        assert!(
            !rules
                .iter()
                .any(|r| r.contains("rm") || r.contains("git commit") || r.contains("git push")),
            "{name}: write command allowed"
        );
    }
}

#[test]
fn only_browser_agents_may_use_playwright() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("agents");
    let browser = [
        "playwright-tester",
        "pr-reviewer",
        "pr-commenter",
        "code-reviewer",
    ];
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let fm = frontmatter(&name);
        let denied = fm.contains("\"playwright_*\": false");
        if browser.contains(&name.as_str()) {
            assert!(
                !denied,
                "{name} drives a browser, Playwright must stay allowed"
            );
        } else {
            assert!(
                denied,
                "{name}: deny Playwright (\"playwright_*\": false) unless the agent needs a browser"
            );
        }
        checked += 1;
    }
    assert!(checked >= 20, "expected all agent templates, saw {checked}");
}

#[test]
fn code_reviewer_may_use_the_browser_only_for_web_development() {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("agents/code-reviewer.md"),
    )
    .unwrap();
    let section = text
        .split("## Browser (Playwright)")
        .nth(1)
        .expect("missing browser section");
    assert!(
        section.contains("web development"),
        "must say it is for web development"
    );
    assert!(
        section.contains("Never open a browser for anything else"),
        "must forbid it for other code"
    );
    assert!(section.contains("view-only"), "must be view-only");
}

#[test]
fn planners_may_write_plans_to_the_global_plans_folder() {
    for name in ["plan", "architect"] {
        let fm = frontmatter(name);
        assert_eq!(
            fm.matches("\"~/.config/opencode/plans/*\": allow").count(),
            3,
            "{name}: edit, write and external_directory"
        );
    }
}

#[test]
fn orchestrator_names_the_plans_folder_and_the_chunk_limit() {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("agents")
            .join("orchestrator.md"),
    )
    .unwrap();
    assert!(text.contains("~/.config/opencode/plans/<project>/<number>/"));
    assert!(text.contains("at most 400 changed lines"));
}

const RULES_START: &str = "<!-- engineering-rules:start";
const RULES_END: &str = "<!-- engineering-rules:end -->";
const CHUNK_MARKER: &str = "<!-- rule:";

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The rules source split at its `<!-- rule:ID -->` markers: ids in document order, and each chunk's text.
fn rule_chunks() -> Vec<(String, String)> {
    let source = std::fs::read_to_string(root().join("docs/engineering-rules.md")).unwrap();
    source
        .split(CHUNK_MARKER)
        .skip(1)
        .map(|piece| {
            let (id, body) = piece.split_once(" -->\n").expect("a closed marker");
            (id.to_string(), body.trim().to_string())
        })
        .collect()
}

#[derive(Default)]
struct Profiles {
    members: std::collections::HashMap<String, Vec<String>>,
    of_agent: std::collections::HashMap<String, Vec<String>>,
}

fn profiles() -> Profiles {
    let text = std::fs::read_to_string(root().join("docs/rules-profiles.txt")).unwrap();
    let mut parsed = Profiles::default();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let (kind, rest) = line.split_once(' ').unwrap();
        let (name, value) = rest.split_once(':').unwrap();
        let words: Vec<String> = value.split_whitespace().map(String::from).collect();
        match kind {
            "profile" => {
                parsed.members.insert(name.trim().to_string(), words);
            }
            "agent" => {
                parsed.of_agent.insert(name.trim().to_string(), words);
            }
            other => panic!("unknown line kind: {other}"),
        }
    }
    parsed
}

fn expand(profiles: &Profiles, items: &[String], ids: &mut std::collections::HashSet<String>) {
    for item in items {
        match item.strip_prefix('@') {
            Some(name) => expand(profiles, &profiles.members[name], ids),
            None => {
                ids.insert(item.clone());
            }
        }
    }
}

fn rules_block_in(text: &str) -> Option<String> {
    let from = text.find(RULES_START)?;
    let body_from = from + text[from..].find("-->")? + "-->".len();
    let to = text.find(RULES_END)?;
    Some(text[body_from..to].trim().to_string())
}

/// What `rules-profiles.txt` prescribes for one agent: its chunks in document order.
fn expected_rules(name: &str, chunks: &[(String, String)], profiles: &Profiles) -> String {
    let mut ids = std::collections::HashSet::new();
    for profile in &profiles.of_agent[name] {
        expand(profiles, &profiles.members[profile], &mut ids);
    }
    for id in &ids {
        assert!(chunks.iter().any(|(known, _)| known == id), "{name}: unknown rule id {id}");
    }
    chunks.iter().filter(|(id, _)| ids.contains(id)).map(|(_, body)| body.as_str()).collect::<Vec<_>>().join("\n\n")
}

#[test]
fn every_agent_carries_exactly_the_rules_its_capabilities_call_for() {
    let (chunks, profiles) = (rule_chunks(), profiles());
    let mut checked = 0;
    for entry in std::fs::read_dir(root().join("agents")).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        assert!(profiles.of_agent.contains_key(&name), "{name}: no entry in docs/rules-profiles.txt");
        let wanted = expected_rules(&name, &chunks, &profiles);
        let found = rules_block_in(&std::fs::read_to_string(&path).unwrap());
        match (wanted.is_empty(), found) {
            (true, None) => {}
            (true, Some(_)) => panic!("{name} must carry no coding rules"),
            (false, None) => panic!("{name}: rules block missing"),
            (false, Some(found)) => assert_eq!(found, wanted, "{name} drifted from docs/engineering-rules.md"),
        }
        checked += 1;
    }
    assert!(checked >= 21, "expected every agent template, found {checked}");
}

#[test]
fn rules_match_what_each_agent_can_do() {
    let (chunks, profiles) = (rule_chunks(), profiles());
    let has = |name: &str, needle: &str| expected_rules(name, &chunks, &profiles).contains(needle);
    for name in ["brain", "mentor", "orchestrator", "summarizer", "doc-lookup", "git-historian", "pr-commenter"] {
        assert!(expected_rules(name, &chunks, &profiles).is_empty(), "{name} writes no code");
    }
    for name in ["architect", "software-architecture-expert", "plan"] {
        assert!(!has(name, "Prohibited:") && !has(name, "arrow functions"), "{name} never codes");
    }
    assert!(!has("test-runner", "Prohibited:"), "test-runner only runs tools");
    for name in ["react-frontend-expert", "typescript-mcp-expert", "playwright-tester"] {
        assert!(!has(name, "**Python**") && !has(name, "**Go**") && !has(name, "SQLite"), "{name} works in TypeScript only");
        assert!(has(name, "**TypeScript / JavaScript**"), "{name}");
    }
    for name in ["code-debugger", "code-reviewer", "api-design-architect"] {
        assert!(has(name, "**Rust**") && has(name, "**Python**"), "{name} works in every language");
    }
}

#[test]
fn the_repository_rules_for_agents_exist_and_are_not_confused_with_the_templates() {
    let agents_md = std::fs::read_to_string(root().join("AGENTS.md")).unwrap();
    let top: String = agents_md.lines().take(8).collect::<Vec<_>>().join(" ");
    assert!(top.contains("not an agent template"), "AGENTS.md must say it is not a template");
    for rule in ["Git is read-only", "Never run `git commit`", "PATCH", "`2.1.17`", "Going to production", "Print the git commands"] {
        assert!(agents_md.contains(rule) || agents_md.to_lowercase().contains(&rule.to_lowercase()), "AGENTS.md lacks: {rule}");
    }
    let claude_md = std::fs::read_to_string(root().join("CLAUDE.md")).unwrap();
    assert!(claude_md.lines().any(|line| line.trim() == "@AGENTS.md"), "CLAUDE.md must import AGENTS.md");
    for stray in ["AGENTS.md", "CLAUDE.md"] {
        assert!(!root().join("agents").join(stray).exists(), "{stray} must not live in agents/");
    }
}

