use super::*;
use crate::cleanup::procs::{Proc, Stale, StaleReason};
use std::time::UNIX_EPOCH;

fn env(keep_binary: bool) -> Env {
    Env {
        home: "/h".into(),
        claude_root: "/h/.claude".into(),
        claude_json: "/h/.claude.json".into(),
        opencode_dir: "/h/.config/opencode".into(),
        runtime_homes: vec!["/h/.codetypera2a".into()],
        temp_dir: "/t".into(),
        dispatchers: Scoped {
            ours: vec![(
                Proc {
                    pid: 7,
                    command: "/h/.cargo/bin/oc dispatch".into(),
                    started: UNIX_EPOCH,
                },
                UNIX_EPOCH,
            )],
            stale: vec![],
            foreign: 0,
        },
        guarded_ours: vec![],
        exe: Some("/h/.cargo/bin/codetypera2a".into()),
        keep_binary,
        agent_files: vec!["brain.md".into(), "mentor.md".into()],
        command_files: vec!["plan-new.md".into()],
    }
}

fn fake(dir: &Path) -> Vec<PathBuf> {
    let names: &[&str] = match dir.to_str().unwrap() {
        "/h/.claude/agents" => &["mine.md", "brain.md"],
        "/h/.config/opencode/commands" => &["plan-new.md"],
        "/h/.config/opencode" => &[
            "opencode.jsonc",
            "opencode.jsonc.bak-a2a",
            "opencode.jsonc.bak-a2a-models",
        ],
        "/t" => &["a2a-window-a2a-status.lock", "other"],
        _ => &[],
    };
    names.iter().map(|n| dir.join(n)).collect()
}

fn probe() -> Probe<'static> {
    Probe {
        list: &fake,
        exists: &|p| {
            [
                "/h/.claude/agents/mine.md",
                "/h/.claude/agents/brain.md",
                "/h/.claude/agents/brain.md.bak-a2a",
                "/h/.config/opencode/commands/plan-new.md",
            ]
            .contains(&p.to_str().unwrap())
        },
        links_to_exe: &|p| p == Path::new("/h/.cargo/bin/oc"),
    }
}

#[test]
fn a_folder_that_is_not_ours_is_never_deleted_and_a_foreign_oc_is_kept() {
    let mut stray = env(false);
    stray.runtime_homes = vec!["/h".into(), "/h/projects".into(), "/h/.codetypera2a".into()];
    let none = |_: &Path| false;
    let foreign = Probe {
        list: &fake,
        exists: &none,
        links_to_exe: &none,
    };
    let plan = build(&stray, &foreign);
    assert!(
        !plan.contains(&Action::Remove("/h".into()))
            && !plan.contains(&Action::Remove("/h/projects".into()))
    );
    assert!(plan.contains(&Action::Remove("/h/.codetypera2a".into())));
    assert!(
        !plan.contains(&Action::Remove("/h/.cargo/bin/oc".into())),
        "oc is not a link to us"
    );
    let marked = |p: &Path| p == Path::new("/h/work/hub.db");
    assert!(
        !looks_like_our_home(Path::new("/h/projects/a2a"), &none),
        "a project folder named a2a"
    );
    assert!(looks_like_our_home(Path::new("/h/.codetypera2a"), &none));
    assert!(looks_like_our_home(Path::new("/h/work"), &marked));
    assert!(!looks_like_our_home(Path::new("/h/work"), &none));
    let mut by_hand = env(false);
    by_hand.exe = Some("/usr/local/bin/codetypera2a".into());
    assert!(
        !build(&by_hand, &probe())
            .iter()
            .any(|a| matches!(a, Action::Remove(p) if p.starts_with("/usr/local")))
    );
}

#[test]
fn plan_removes_guarded_file_only_when_ours() {
    let target: PathBuf = "/h/.claude/CLAUDE.md".into();
    let without = build(&env(true), &probe());
    assert!(!without.contains(&Action::Remove(target.clone())));
    let mut world = env(true);
    world.guarded_ours = vec![target.clone()];
    let with = build(&world, &probe());
    let at = |action: &Action| with.iter().position(|a| a == action).unwrap();
    let manifest = Action::Remove("/h/.claude/.a2a-manifest.json".into());
    assert!(
        at(&Action::Remove(target)) < at(&manifest),
        "removed before the manifest"
    );
}

#[test]
fn a_skill_folder_is_removed_only_when_empty_after_its_file() {
    let file: PathBuf = "/h/.claude/skills/rdd-review/SKILL.md".into();
    let mut world = env(true);
    world.guarded_ours = vec![file.clone(), "/h/.claude/CLAUDE.md".into()];
    let plan = build(&world, &probe());
    let dir = Action::RemoveDirIfEmpty("/h/.claude/skills/rdd-review".into());
    let at = |action: &Action| plan.iter().position(|a| a == action).unwrap();
    assert!(at(&Action::Remove(file)) < at(&dir), "the file goes first");
    let dirs = plan
        .iter()
        .filter(|a| matches!(a, Action::RemoveDirIfEmpty(_)))
        .count();
    assert_eq!(dirs, 1, "CLAUDE.md has no folder to clean");
}

#[test]
fn foreign_and_stale_dispatchers_are_reported_and_never_stopped() {
    let mut world = env(true);
    world.dispatchers = Scoped {
        ours: vec![],
        stale: vec![
            Stale {
                file: "/h/.codetypera2a/dispatcher.pid".into(),
                pid: Some(7),
                reason: StaleReason::Gone,
            },
            Stale {
                file: "/h/.codetypera2a/bad.pid".into(),
                pid: None,
                reason: StaleReason::UnreadableClaim,
            },
        ],
        foreign: 2,
    };
    let plan = build(&world, &probe());
    assert!(!plan.iter().any(|a| matches!(a, Action::Stop { .. })));
    let notes: Vec<String> = plan
        .iter()
        .filter_map(|a| match a {
            Action::Note(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(notes.len(), 3, "{notes:?}");
    assert!(notes[0].starts_with("2 dispatcher(s) of other homes"));
    assert_eq!(notes[1], "stale dispatcher claim, pid 7: gone");
    assert_eq!(
        notes[2],
        "stale dispatcher claim, file bad.pid: unreadable claim"
    );
}

#[test]
fn the_plan_removes_everything_ours_and_the_binary() {
    let plan = build(&env(false), &probe());
    let has = |a: Action| plan.contains(&a);
    assert!(has(Action::Stop {
        pid: 7,
        command: "/h/.cargo/bin/oc dispatch".into(),
        claimed_at: UNIX_EPOCH
    }));
    assert!(
        has(Action::Remove("/h/.claude/agents/brain.md".into()))
            && has(Action::Remove("/h/.claude/agents/brain.md.bak-a2a".into())),
        "an installed template and its backup copy"
    );
    assert!(
        !has(Action::Remove("/h/.claude/agents/mine.md".into())),
        "an agent the user wrote is never removed"
    );
    assert!(has(Action::Remove(
        "/h/.config/opencode/commands/plan-new.md".into()
    )));
    assert!(has(Action::Remove(
        "/h/.config/opencode/opencode.jsonc.bak-a2a-models".into()
    )));
    assert!(
        !has(Action::Remove("/h/.config/opencode/opencode.jsonc".into())),
        "the config itself stays"
    );
    assert!(
        has(Action::Remove("/t/a2a-window-a2a-status.lock".into()))
            && !has(Action::Remove("/t/other".into()))
    );
    assert!(
        has(Action::Remove("/h/.codetypera2a".into()))
            && has(Action::Remove("/h/.codetypera2a".into()))
    );
    assert!(has(Action::EditRc("/h/.zshrc".into())) && has(Action::CargoUninstall));
    assert!(
        has(Action::Remove("/h/.cargo/bin/codetypera2a".into()))
            && has(Action::Remove("/h/.cargo/bin/oc".into()))
    );
}

#[test]
fn keeping_the_binary_leaves_it_alone() {
    let plan = build(&env(true), &probe());
    assert!(!plan.contains(&Action::CargoUninstall));
    assert!(!plan.contains(&Action::Remove("/h/.cargo/bin/codetypera2a".into())));
}

#[test]
fn the_shell_block_is_cut_out_and_nothing_else() {
    let text = "export A=1\n\n# >>> a2a (codetypera2a) >>>\nalias opencode='oc'\n# <<< a2a (codetypera2a) <<<\nexport B=2\n";
    assert_eq!(without_rc_block(text).unwrap(), "export A=1\nexport B=2\n");
    assert_eq!(without_rc_block("export A=1\n"), None);
    let only = "# >>> a2a (codetypera2a) >>>\nx\n# <<< a2a (codetypera2a) <<<\n";
    assert_eq!(without_rc_block(only).unwrap(), "");
}

#[test]
fn the_owner_is_read_from_the_install_path() {
    assert_eq!(
        owner(Path::new(
            "/opt/homebrew/Cellar/codetypera2a/2/bin/codetypera2a"
        )),
        Some(Action::BrewUninstall)
    );
    assert_eq!(
        owner(Path::new("/h/.cargo/bin/codetypera2a")),
        Some(Action::CargoUninstall)
    );
    assert_eq!(owner(Path::new("/usr/local/bin/codetypera2a")), None);
}
