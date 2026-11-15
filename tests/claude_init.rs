//! `codetypera2a init` (no flags) and `init --claude` must end with the same Claude Code setup, and
//! choosing only opencode must not touch the Claude dir. A fake `claude` on PATH stands in for the real
//! one, so these runs never touch `~/.claude`.
use std::collections::BTreeMap;
use std::path::Path;

mod common;

use common::Sandbox;

/// Every file under `dir` (relative path -> contents with the dir's own path blanked out).
fn tree(dir: &Path) -> BTreeMap<String, String> {
    fn walk(base: &Path, at: &Path, out: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(at).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
                continue;
            }
            let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap())
                .replace(&base.display().to_string(), "<D>");
            out.insert(path.strip_prefix(base).unwrap().display().to_string(), text);
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

#[test]
fn plain_init_and_init_claude_leave_the_same_claude_setup() {
    let sb = Sandbox::new("same", true, false);
    std::fs::create_dir_all(sb.root.join("home/.claude")).unwrap();
    let flagged = sb.init("a", &["--claude", "--yes"]);
    assert!(
        flagged.status.success(),
        "{}",
        String::from_utf8_lossy(&flagged.stderr)
    );
    let calls_flagged = sb.claude_calls();
    let plain = sb.init("b", &["--yes"]);
    assert!(
        plain.status.success(),
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    let (a, b) = (
        tree(&sb.root.join("a/claude")),
        tree(&sb.root.join("b/claude")),
    );
    assert!(
        a.contains_key("agents/orchestrator.md")
            && a.contains_key("settings.json")
            && a.contains_key(".a2a-manifest.json"),
        "{:?}",
        a.keys().collect::<Vec<_>>()
    );
    assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
    for (file, text) in &a {
        assert_eq!(
            text, &b[file],
            "{file} differs between `init --claude` and plain `init`"
        );
    }
    assert!(
        a["settings.json"].contains("--hook ensure"),
        "the SessionStart hook is installed"
    );
    assert!(
        calls_flagged.contains("plugin install a2a-pane@codetypera2a"),
        "{calls_flagged}"
    );
    assert_eq!(
        sb.claude_calls().replace(&calls_flagged, ""),
        calls_flagged.replace("/a/claude", "/b/claude"),
        "the same claude commands ran"
    );
    assert!(
        !sb.root.join("b/opencode").exists(),
        "plain init with only Claude Code detected does not touch opencode"
    );
}

#[test]
fn choosing_only_opencode_does_not_touch_the_claude_dir() {
    let sb = Sandbox::new("oc", false, true);
    let out = sb.init("c", &["--yes"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(sb.root.join("c/opencode/agents/orchestrator.md").exists());
    assert!(!sb.root.join("c/claude").exists(), "no Claude Code files");
    assert!(sb.claude_calls().is_empty());
    let flag = sb.init("d", &["--opencode"]);
    assert!(flag.status.success());
    assert!(!sb.root.join("d/claude").exists());
    assert!(!sb.root.join("c/opencode/CLAUDE.md").exists());
    assert!(!sb.root.join("d/opencode/CLAUDE.md").exists());
}

fn settings_of(sb: &Sandbox, config: &str) -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(sb.root.join(config).join("claude/settings.json")).unwrap(),
    )
    .unwrap()
}

#[test]
fn init_makes_the_orchestrator_the_main_agent_once_and_plain_init_agrees() {
    let sb = Sandbox::new("agent", true, false);
    assert!(sb.init("a", &["--claude", "--yes"]).status.success());
    assert_eq!(settings_of(&sb, "a")["agent"], "orchestrator");
    let first = std::fs::read_to_string(sb.root.join("a/claude/settings.json")).unwrap();
    assert!(sb.init("a", &["--claude", "--yes"]).status.success());
    assert_eq!(
        std::fs::read_to_string(sb.root.join("a/claude/settings.json")).unwrap(),
        first,
        "rerun changes nothing"
    );
    assert!(sb.init("b", &["--yes"]).status.success());
    assert_eq!(settings_of(&sb, "b"), settings_of(&sb, "a"));
}

#[test]
fn another_main_agent_is_kept_and_the_flag_skips_the_setting() {
    let sb = Sandbox::new("agentkeep", true, false);
    std::fs::create_dir_all(sb.root.join("c/claude")).unwrap();
    std::fs::write(
        sb.root.join("c/claude/settings.json"),
        r#"{"agent":"mine"}"#,
    )
    .unwrap();
    let out = sb.init("c", &["--claude", "--yes"]);
    assert!(out.status.success());
    assert_eq!(settings_of(&sb, "c")["agent"], "mine");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("orchestrator is not the default"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        sb.init("d", &["--claude", "--yes", "--no-default-agent"])
            .status
            .success()
    );
    assert!(settings_of(&sb, "d").get("agent").is_none());
}
