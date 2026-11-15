//! `init --claude` and `init --opencode` install `CLAUDE.md` and the `rdd-review` skill under the guarded
//! policy: nothing of the user's is replaced without `--force` and a backup, and a symlink never is.
mod common;

use common::Sandbox;
use std::path::{Path, PathBuf};

fn embedded_claude_md() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/claude/CLAUDE.user.md"),
    )
    .unwrap()
}

fn claude_md(sb: &Sandbox, config: &str) -> PathBuf {
    sb.root.join(config).join("claude/CLAUDE.md")
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn backups(sb: &Sandbox, config: &str) -> Vec<PathBuf> {
    let dir = sb.root.join(config).join("claude");
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("CLAUDE.md.bak-")
        })
        .collect()
}

#[test]
fn fresh_init_writes_claude_md_and_manifest_entry() {
    let sb = Sandbox::new("md-fresh", true, false);
    let out = sb.init("c", &["--claude"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(claude_md(&sb, "c")).unwrap(),
        embedded_claude_md()
    );
    let manifest = std::fs::read_to_string(sb.root.join("c/claude/.a2a-manifest.json")).unwrap();
    assert!(manifest.contains("\"CLAUDE.md\""), "{manifest}");
}

#[test]
fn rerun_is_idempotent_and_reports_up_to_date() {
    let sb = Sandbox::new("md-rerun", true, false);
    sb.init("c", &["--claude"]);
    let before = std::fs::metadata(claude_md(&sb, "c"))
        .unwrap()
        .modified()
        .unwrap();
    let out = sb.init("c", &["--claude"]);
    assert!(
        stdout_of(&out).contains("CLAUDE.md: up to date"),
        "{}",
        stdout_of(&out)
    );
    assert_eq!(
        std::fs::metadata(claude_md(&sb, "c"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
}

#[test]
fn edited_file_is_kept_with_a_kept_line() {
    let sb = Sandbox::new("md-edited", true, false);
    sb.init("c", &["--claude"]);
    std::fs::write(claude_md(&sb, "c"), "my edit\n").unwrap();
    let out = sb.init("c", &["--claude"]);
    assert!(
        stdout_of(&out).contains("CLAUDE.md: kept"),
        "{}",
        stdout_of(&out)
    );
    assert_eq!(
        std::fs::read_to_string(claude_md(&sb, "c")).unwrap(),
        "my edit\n"
    );
    assert!(backups(&sb, "c").is_empty());
}

#[test]
fn force_backs_up_then_replaces() {
    let sb = Sandbox::new("md-force", true, false);
    sb.init("c", &["--claude"]);
    std::fs::write(claude_md(&sb, "c"), "my edit\n").unwrap();
    let out = sb.init("c", &["--claude", "--force"]);
    assert!(out.status.success());
    assert_eq!(
        std::fs::read_to_string(claude_md(&sb, "c")).unwrap(),
        embedded_claude_md()
    );
    let found = backups(&sb, "c");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(std::fs::read_to_string(&found[0]).unwrap(), "my edit\n");
}

#[test]
fn preexisting_user_file_without_manifest_is_kept() {
    let sb = Sandbox::new("md-user", true, false);
    std::fs::create_dir_all(sb.root.join("c/claude")).unwrap();
    std::fs::write(claude_md(&sb, "c"), "my own rules\n").unwrap();
    let out = sb.init("c", &["--claude"]);
    assert!(
        stdout_of(&out).contains("CLAUDE.md: kept"),
        "{}",
        stdout_of(&out)
    );
    assert_eq!(
        std::fs::read_to_string(claude_md(&sb, "c")).unwrap(),
        "my own rules\n"
    );
}

#[test]
fn gentle_ai_marked_file_is_kept_and_mentions_gentle_ai() {
    let sb = Sandbox::new("md-gentle", true, false);
    std::fs::create_dir_all(sb.root.join("c/claude")).unwrap();
    std::fs::write(claude_md(&sb, "c"), "<!-- gentle-ai:persona -->\nrules\n").unwrap();
    let out = sb.init("c", &["--claude"]);
    assert!(stdout_of(&out).contains("gentle-ai"), "{}", stdout_of(&out));
    assert!(
        std::fs::read_to_string(claude_md(&sb, "c"))
            .unwrap()
            .contains("gentle-ai:persona")
    );
}

#[cfg(unix)]
#[test]
fn symlink_claude_md_is_kept_even_with_force() {
    let sb = Sandbox::new("md-link", true, false);
    std::fs::create_dir_all(sb.root.join("c/claude")).unwrap();
    let target = sb.root.join("dotfiles-claude.md");
    std::fs::write(&target, "dotfiles\n").unwrap();
    std::os::unix::fs::symlink(&target, claude_md(&sb, "c")).unwrap();
    let out = sb.init("c", &["--claude", "--force"]);
    assert!(out.status.success());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "dotfiles\n");
    assert!(
        std::fs::symlink_metadata(claude_md(&sb, "c"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn md_only_writes_it_on_a_fresh_dir() {
    let sb = Sandbox::new("md-only", true, false);
    let out = sb.init("c", &["--claude", "--md-only"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(claude_md(&sb, "c")).unwrap(),
        embedded_claude_md()
    );
}

#[test]
fn opencode_only_init_writes_no_claude_md() {
    let sb = Sandbox::new("md-oc", false, true);
    sb.init("c", &["--opencode"]);
    assert!(!sb.root.join("c/claude").exists());
    assert!(!sb.root.join("c/opencode/CLAUDE.md").exists());
}

const SKILL: &str = "skills/rdd-review/SKILL.md";

fn embedded_skill() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("templates")
            .join(SKILL),
    )
    .unwrap()
}

fn skill_backups(dir: &Path) -> Vec<PathBuf> {
    let folder = dir.join("skills/rdd-review");
    std::fs::read_dir(folder)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("SKILL.md.bak-")
        })
        .collect()
}

#[test]
fn claude_fresh_install_writes_skill() {
    let sb = Sandbox::new("skill-fresh", true, false);
    let out = sb.init("c", &["--claude"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let installed = sb.root.join("c/claude").join(SKILL);
    assert_eq!(
        std::fs::read_to_string(installed).unwrap(),
        embedded_skill()
    );
    let manifest = std::fs::read_to_string(sb.root.join("c/claude/.a2a-manifest.json")).unwrap();
    assert!(manifest.contains(SKILL), "{manifest}");
}

#[test]
fn edited_skill_is_kept() {
    let sb = Sandbox::new("skill-edited", true, false);
    sb.init("c", &["--claude"]);
    let installed = sb.root.join("c/claude").join(SKILL);
    std::fs::write(&installed, "my skill\n").unwrap();
    let out = sb.init("c", &["--claude"]);
    assert!(
        stdout_of(&out).contains("SKILL.md: kept"),
        "{}",
        stdout_of(&out)
    );
    assert_eq!(std::fs::read_to_string(installed).unwrap(), "my skill\n");
}

#[test]
fn force_backs_up_and_replaces_skill() {
    let sb = Sandbox::new("skill-force", true, false);
    sb.init("c", &["--claude"]);
    let installed = sb.root.join("c/claude").join(SKILL);
    std::fs::write(&installed, "my skill\n").unwrap();
    let out = sb.init("c", &["--claude", "--force"]);
    assert!(out.status.success());
    assert_eq!(
        std::fs::read_to_string(&installed).unwrap(),
        embedded_skill()
    );
    let found = skill_backups(&sb.root.join("c/claude"));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(std::fs::read_to_string(&found[0]).unwrap(), "my skill\n");
}

#[cfg(unix)]
#[test]
fn symlinked_skill_is_never_replaced() {
    let sb = Sandbox::new("skill-link", true, false);
    let folder = sb.root.join("c/claude/skills/rdd-review");
    std::fs::create_dir_all(&folder).unwrap();
    let target = sb.root.join("elsewhere.md");
    std::fs::write(&target, "dotfiles skill\n").unwrap();
    std::os::unix::fs::symlink(&target, folder.join("SKILL.md")).unwrap();
    let out = sb.init("c", &["--claude", "--force"]);
    assert!(out.status.success());
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "dotfiles skill\n"
    );
    assert!(
        std::fs::symlink_metadata(folder.join("SKILL.md"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn opencode_install_writes_skill_when_supported() {
    let sb = Sandbox::new("skill-oc", false, true);
    let out = sb.init("c", &["--opencode"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let installed = sb.root.join("c/opencode").join(SKILL);
    assert_eq!(
        std::fs::read_to_string(installed).unwrap(),
        embedded_skill()
    );
}

#[test]
fn opencode_existing_different_skill_is_kept_and_force_backs_up() {
    let sb = Sandbox::new("skill-oc-keep", false, true);
    sb.init("c", &["--opencode"]);
    let installed = sb.root.join("c/opencode").join(SKILL);
    std::fs::write(&installed, "my skill\n").unwrap();
    let kept = sb.init("c", &["--opencode"]);
    assert!(
        stdout_of(&kept).contains("SKILL.md: kept"),
        "{}",
        stdout_of(&kept)
    );
    assert_eq!(std::fs::read_to_string(&installed).unwrap(), "my skill\n");
    let forced = sb.init("c", &["--opencode", "--force"]);
    assert!(forced.status.success());
    assert_eq!(
        std::fs::read_to_string(&installed).unwrap(),
        embedded_skill()
    );
    let found = skill_backups(&sb.root.join("c/opencode"));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(std::fs::read_to_string(&found[0]).unwrap(), "my skill\n");
}
