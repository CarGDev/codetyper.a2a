//! Carrying out one cleanup action. Every failure is a one-line message; the caller goes on.
use super::jsonc;
use super::plan::{self, Action, JsonKind};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const STOP_WAIT: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(100);
const TMUX_WINDOW: &str = "=a2a-status";

fn quiet(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The command line of `pid` right now, to make sure the number still belongs to one of our dispatchers.
fn command_of(pid: u32) -> String {
    Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn stop(pid: u32) -> Result<String, String> {
    let current = command_of(pid);
    if current.is_empty() {
        return Ok(format!("process {pid} already gone"));
    }
    if !super::is_ours(&current) {
        return Err(format!(
            "process {pid} is no longer one of ours; left alone"
        ));
    }
    let id = pid.to_string();
    quiet("kill", &[&id]);
    let started = Instant::now();
    while crate::sys::pid_alive(pid) && started.elapsed() < STOP_WAIT {
        std::thread::sleep(POLL);
    }
    if crate::sys::pid_alive(pid) {
        quiet("kill", &["-9", &id]);
    }
    if crate::sys::pid_alive(pid) {
        return Err(format!("process {pid} would not stop"));
    }
    Ok(format!("stopped {pid}"))
}

fn remove(path: &Path) -> Result<String, String> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(format!("absent {}", path.display()));
    };
    let result = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    result
        .map(|_| format!("deleted {}", path.display()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Replace a file without ever leaving it half written: a temp file in the same folder, then a rename.
/// The mode is kept, and a symlink is followed so the real file is the one replaced.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let target = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let temp = target.with_extension(format!("a2a-tmp-{}", std::process::id()));
    let result =
        write_private_copy(&target, &temp, text).and_then(|_| std::fs::rename(&temp, &target));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(|e| format!("{}: {e}", target.display()))
}

/// The temp file is created exclusively and with the original's mode from the start, so a private file such
/// as `~/.claude.json` is never readable by others, not even for a moment.
fn write_private_copy(original: &Path, temp: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        options.mode(std::fs::metadata(original)?.mode() & 0o7777);
    }
    options.open(temp)?.write_all(text.as_bytes())
}

fn edit_json(path: &Path, kind: JsonKind) -> Result<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(format!("absent {}", path.display()));
    };
    let mut root = jsonc::parse(&text)
        .map_err(|e| format!("{}: cannot parse ({e}); edit it by hand", path.display()))?;
    let removed = match kind {
        JsonKind::Opencode => jsonc::clean_opencode(&mut root),
        JsonKind::ClaudeJson => jsonc::clean_claude_json(&mut root),
        JsonKind::ClaudeSettings => jsonc::clean_claude_settings(&mut root),
    };
    if removed.is_empty() {
        return Ok(format!("nothing of ours in {}", path.display()));
    }
    let body = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    write_atomic(
        path,
        &format!(
            "{body}
"
        ),
    )?;
    Ok(format!(
        "cleaned {} ({})",
        path.display(),
        removed.join(", ")
    ))
}

fn edit_rc(path: &Path) -> Result<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(format!("absent {}", path.display()));
    };
    let Some(cleaned) = plan::without_rc_block(&text) else {
        return Ok(format!("no a2a block in {}", path.display()));
    };
    write_atomic(path, &cleaned)?;
    Ok(format!("cleaned {}", path.display()))
}

pub fn run(action: &Action) -> Result<String, String> {
    match action {
        Action::Stop { pid, .. } => stop(*pid),
        Action::ClaudePlugin => {
            quiet(
                "claude",
                &[
                    "plugin",
                    "uninstall",
                    "a2a-pane@codetypera2a",
                    "--scope",
                    "user",
                ],
            );
            quiet(
                "claude",
                &["plugin", "marketplace", "remove", "codetypera2a"],
            );
            Ok("claude plugin removed (if it was there)".into())
        }
        Action::TmuxWindow => {
            quiet("tmux", &["kill-window", "-t", TMUX_WINDOW]);
            Ok("tmux window closed (if it was open)".into())
        }
        Action::Remove(path) => remove(path),
        Action::EditJson(path, kind) => edit_json(path, *kind),
        Action::EditRc(path) => edit_rc(path),
        Action::CargoUninstall => Ok(match quiet("cargo", &["uninstall", "codetypera2a"]) {
            true => "cargo uninstalled".into(),
            false => "cargo uninstall: nothing to do".into(),
        }),
        Action::BrewUninstall => Ok(match quiet("brew", &["uninstall", "codetypera2a"]) {
            true => "brew uninstalled".into(),
            false => "brew uninstall: nothing to do".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("a2a-apply-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn remove_deletes_files_dirs_and_links_and_tolerates_absence() {
        let dir = tmp("remove");
        std::fs::create_dir_all(dir.join("d/sub")).unwrap();
        std::fs::write(dir.join("d/sub/f"), "x").unwrap();
        std::fs::write(dir.join("file"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("file"), dir.join("link")).unwrap();
        for name in ["d", "file", "link", "missing"] {
            assert!(remove(&dir.join(name)).is_ok());
        }
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn json_edits_keep_other_keys_and_unparsable_files_are_reported_not_changed() {
        let dir = tmp("json");
        let file = dir.join("opencode.jsonc");
        std::fs::write(
            &file,
            "// c\n{\"model\":\"m\",\"default_agent\":\"orchestrator\",}",
        )
        .unwrap();
        assert!(
            edit_json(&file, JsonKind::Opencode)
                .unwrap()
                .contains("default_agent")
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap().trim(),
            "{\n  \"model\": \"m\"\n}"
        );
        let bad = dir.join("bad.json");
        std::fs::write(&bad, "{not json").unwrap();
        assert!(edit_json(&bad, JsonKind::ClaudeJson).is_err());
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), "{not json");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_shell_file_loses_only_the_block() {
        let dir = tmp("rc");
        let rc = dir.join(".zshrc");
        std::fs::write(
            &rc,
            "a\n# >>> a2a (codetypera2a) >>>\nx\n# <<< a2a (codetypera2a) <<<\nb\n",
        )
        .unwrap();
        edit_rc(&rc).unwrap();
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), "a\nb\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn an_atomic_write_keeps_the_mode_follows_a_symlink_and_leaves_no_temp_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp("atomic");
        let real = dir.join("real.json");
        std::fs::write(&real, "old").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&real, dir.join("link.json")).unwrap();
        write_atomic(&dir.join("link.json"), "new").unwrap();
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
        assert!(
            std::fs::symlink_metadata(dir.join("link.json"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            2,
            "no temp file left behind"
        );
        assert!(write_atomic(&dir.join("missing.json"), "x").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
