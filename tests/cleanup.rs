//! `uninstall` / `reinstall` in a throwaway HOME: everything is deleted for good and the reinstall verifies a clean database.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_codetypera2a");

fn sandbox(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("a2a-cleanup-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".config/opencode")).unwrap();
    std::fs::write(dir.join(".zshrc"), "export KEEP=1\n").unwrap();
    std::fs::write(
        dir.join(".config/opencode/opencode.jsonc"),
        "// mine\n{ \"model\": \"x\", }\n",
    )
    .unwrap();
    dir
}

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("OPENCODE_CONFIG_DIR")
        .env_remove("CODETYPERA2A_HOME")
        .env_remove("A2A_DB")
        .env_remove("A2A_DISPATCHER_PID")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("OPENCODE_BIN", "/usr/bin/false")
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn the_dry_run_changes_nothing() {
    let home = sandbox("dry");
    assert!(
        run(
            &home,
            &[
                "init",
                "--yes",
                "--all",
                "--no-models",
                "--no-link",
                "--shell"
            ]
        )
        .status
        .success()
    );
    let before = std::fs::read_dir(home.join(".claude/agents"))
        .unwrap()
        .count();
    let out = run(&home, &["uninstall", "--keep-binary"]);
    assert!(text(&out).contains("DRY RUN"), "{}", text(&out));
    assert_eq!(
        std::fs::read_dir(home.join(".claude/agents"))
            .unwrap()
            .count(),
        before
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn uninstall_deletes_everything_and_reinstall_ends_clean() {
    let home = sandbox("full");
    assert!(
        run(
            &home,
            &[
                "init",
                "--yes",
                "--all",
                "--no-models",
                "--no-link",
                "--shell"
            ]
        )
        .status
        .success()
    );
    std::fs::write(home.join(".claude/agents/mine.md"), "mine").unwrap();
    assert!(
        std::fs::read_to_string(home.join(".zshrc"))
            .unwrap()
            .contains("codetypera2a")
    );

    let out = run(&home, &["uninstall", "--yes", "--keep-binary"]);
    assert!(out.status.success(), "{}", text(&out));
    let gone = |p: &str| !home.join(p).exists();
    assert!(
        gone(".codetypera2a") && gone(".config/opencode/a2a") && gone(".claude/.a2a-manifest.json")
    );
    assert!(gone(".claude/a2a-marketplace") && gone(".config/opencode/plugins/a2a"));
    let leftover = |dir: &str| {
        std::fs::read_dir(home.join(dir))
            .map(|d| d.count())
            .unwrap_or(0)
    };
    assert_eq!(
        std::fs::read_to_string(home.join(".claude/agents/mine.md")).unwrap(),
        "mine",
        "an agent the user wrote stays"
    );
    assert_eq!(
        (leftover(".claude/agents"), leftover(".claude/commands")),
        (1, 0),
        "only our templates are removed"
    );
    assert_eq!(
        (
            leftover(".config/opencode/agents"),
            leftover(".config/opencode/commands")
        ),
        (0, 0)
    );
    assert_eq!(
        std::fs::read_to_string(home.join(".zshrc")).unwrap(),
        "export KEEP=1\n"
    );
    assert!(
        !home.join("codetypera2a-uninstall").exists(),
        "no backup folder"
    );

    let out = run(&home, &["reinstall", "--yes"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("verified"), "{}", text(&out));
    assert!(
        !home.join(".config/opencode/a2a/hub.db").exists(),
        "no legacy database"
    );
    let _ = std::fs::remove_dir_all(home);
}
