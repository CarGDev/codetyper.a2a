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
