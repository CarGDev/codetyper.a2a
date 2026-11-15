//! `uninstall` removes only the guarded files (CLAUDE.md, skills) that are still exactly what `init` wrote.
mod common;

use common::{run, sandbox, text};
use std::path::Path;

fn install_claude(home: &Path) {
    let out = run(home, &["init", "--claude", "--no-models", "--no-link"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(home.join(".claude/CLAUDE.md").exists(), "{}", text(&out));
}

#[test]
fn uninstall_removes_our_unchanged_claude_md() {
    let home = sandbox("md-ours");
    install_claude(&home);
    let out = run(&home, &["uninstall", "--yes", "--keep-binary"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!home.join(".claude/CLAUDE.md").exists(), "{}", text(&out));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn uninstall_keeps_edited_claude_md_and_every_bak() {
    let home = sandbox("md-edited");
    install_claude(&home);
    std::fs::write(home.join(".claude/CLAUDE.md"), "my edit\n").unwrap();
    std::fs::write(home.join(".claude/CLAUDE.md.bak-20260101T000000Z"), "old\n").unwrap();
    let out = run(&home, &["uninstall", "--yes", "--keep-binary"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        std::fs::read_to_string(home.join(".claude/CLAUDE.md")).unwrap(),
        "my edit\n"
    );
    assert!(home.join(".claude/CLAUDE.md.bak-20260101T000000Z").exists());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn dry_run_lists_claude_md_and_deletes_nothing() {
    let home = sandbox("md-dry");
    install_claude(&home);
    let out = run(&home, &["uninstall", "--keep-binary"]);
    assert!(text(&out).contains("CLAUDE.md"), "{}", text(&out));
    assert!(home.join(".claude/CLAUDE.md").exists());
    let _ = std::fs::remove_dir_all(home);
}

const SKILL: &str = "skills/rdd-review/SKILL.md";

fn install_opencode(home: &Path) {
    let out = run(home, &["init", "--opencode", "--no-models", "--no-link"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        home.join(".config/opencode").join(SKILL).exists(),
        "{}",
        text(&out)
    );
}

#[test]
fn uninstall_removes_only_unchanged_skills_and_empty_dir() {
    let home = sandbox("skill-ours");
    install_claude(&home);
    install_opencode(&home);
    std::fs::create_dir_all(home.join(".claude/skills/mine")).unwrap();
    std::fs::write(home.join(".claude/skills/mine/SKILL.md"), "mine\n").unwrap();
    let out = run(&home, &["uninstall", "--yes", "--keep-binary"]);
    assert!(out.status.success(), "{}", text(&out));
    for root in [".claude", ".config/opencode"] {
        assert!(
            !home.join(root).join(SKILL).exists(),
            "{root}: {}",
            text(&out)
        );
        assert!(
            !home.join(root).join("skills/rdd-review").exists(),
            "{root}: empty dir stays"
        );
    }
    assert!(
        home.join(".claude/skills/mine/SKILL.md").exists(),
        "a user skill is never removed"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn user_skills_and_backups_untouched() {
    let home = sandbox("skill-user");
    install_claude(&home);
    install_opencode(&home);
    let edited = home.join(".claude").join(SKILL);
    std::fs::write(&edited, "edited skill\n").unwrap();
    let backup = home.join(".claude/skills/rdd-review/SKILL.md.bak-20260101T000000Z");
    std::fs::write(&backup, "old\n").unwrap();
    std::fs::write(home.join(".config/opencode").join(SKILL), "changed\n").unwrap();
    let out = run(&home, &["uninstall", "--yes", "--keep-binary"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(std::fs::read_to_string(&edited).unwrap(), "edited skill\n");
    assert!(backup.exists());
    assert_eq!(
        std::fs::read_to_string(home.join(".config/opencode").join(SKILL)).unwrap(),
        "changed\n"
    );
    let _ = std::fs::remove_dir_all(home);
}
