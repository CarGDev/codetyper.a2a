//! Claude Code `settings.json`: the footer status line and the SubagentStart / SubagentStop hooks that
//! feed it (`oc statusline`), the UserPromptSubmit hook that hands finished a2a results to the prompt and
//! the SessionStart hook that starts the dispatcher and the tmux status window. Only what is absent is added: a status line or hook the user has stays,
//! the file is backed up before its first change, and our own entries are found by their command.
use serde_json::{Value, json};
use std::path::Path;

const STATUSLINE_MARK: &str = "oc statusline";
const HOOK_MARK: &str = "oc statusline --hook";
/// (event, hook argument)
const HOOKS: [(&str, &str); 4] = [("SubagentStart", "start"), ("SubagentStop", "stop"), ("UserPromptSubmit", "inbox"), ("SessionStart", "ensure")];

/// `'...'` safe for `sh -c`; the settings command runs through a shell.
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

fn command_of(entry: &Value) -> &str {
    entry["command"].as_str().unwrap_or_default()
}

/// The executable a settings command starts with (`'/a b/x' oc ...` or `/x oc ...`).
fn exe_of(command: &str) -> &str {
    match command.strip_prefix('\'') {
        Some(rest) => rest.split('\'').next().unwrap_or_default(),
        None => command.split_whitespace().next().unwrap_or_default(),
    }
}

/// Our entry whose executable is gone (the binary moved) gets the current command; one whose binary still
/// exists, or any entry that is not ours, is left as it is. Returns whether it changed.
fn repair(entry: &mut Value, want: &str) -> bool {
    let old = command_of(entry);
    if old == want || Path::new(exe_of(old)).is_file() {
        return false;
    }
    entry["command"] = json!(want);
    true
}

/// Our hook entries of one event, mutable: found by their command, wherever the user put them.
fn ours(groups: &mut [Value]) -> Vec<&mut Value> {
    groups.iter_mut().filter_map(|g| g["hooks"].as_array_mut()).flatten().filter(|h| command_of(h).contains(HOOK_MARK)).collect()
}

/// Add the status line and hooks to `file` where absent. Returns the notes for the user; empty when
/// everything was already in place.
pub fn install(file: &Path, exe: &Path) -> Result<Vec<String>, String> {
    let mut cfg: Value = match std::fs::read_to_string(file) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{} is not valid JSON ({e}); left untouched", file.display()))?,
        Err(_) => json!({}),
    };
    let before = cfg.clone();
    let root = cfg.as_object_mut().ok_or_else(|| format!("{} is not a JSON object", file.display()))?;
    let cmd = format!("{} {STATUSLINE_MARK}", shell_quote(exe));
    let mut notes = Vec::new();
    match root.get_mut("statusLine") {
        None => {
            root.insert("statusLine".into(), json!({ "type": "command", "command": cmd, "refreshInterval": 5 }));
        }
        Some(existing) if command_of(existing).contains(STATUSLINE_MARK) => {
            repair(existing, &cmd);
        }
        Some(_) => notes.push(format!("{} already has a status line; left as is (the a2a one is `{cmd}`)", file.display())),
    }
    let hooks = root.entry("hooks").or_insert_with(|| json!({})).as_object_mut().ok_or_else(|| format!("`hooks` in {} is not an object", file.display()))?;
    for (event, arg) in HOOKS {
        let groups = hooks.entry(event).or_insert_with(|| json!([])).as_array_mut().ok_or_else(|| format!("`hooks.{event}` in {} is not an array", file.display()))?;
        let want = format!("{cmd} --hook {arg}");
        let mut present = ours(groups);
        if !present.is_empty() {
            present.iter_mut().for_each(|h| {
                repair(h, &want);
            });
            continue;
        }
        groups.push(json!({ "hooks": [{ "type": "command", "command": format!("{cmd} --hook {arg}") }] }));
    }
    if cfg == before {
        return Ok(notes);
    }
    if file.exists() {
        let mut bak = file.as_os_str().to_owned();
        bak.push(".bak-a2a");
        std::fs::copy(file, &bak).map_err(|e| format!("cannot back up {}: {e}", file.display()))?;
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let out = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    std::fs::write(file, out + "\n").map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    notes.insert(0, format!("status line and hooks added to {} (disable: remove `statusLine` and the four `oc statusline` hooks)", file.display()));
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("a2a-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read(p: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn installs_the_status_line_and_both_hooks_once() {
        let dir = tmp("fresh");
        let file = dir.join("settings.json");
        let notes = install(&file, Path::new("/x/my bin/codetypera2a")).unwrap();
        assert!(notes.len() == 1 && notes[0].contains("status line"), "{notes:?}");
        let cfg = read(&file);
        let cmd = "'/x/my bin/codetypera2a' oc statusline";
        assert_eq!(cfg["statusLine"], json!({ "type": "command", "command": cmd, "refreshInterval": 5 }));
        assert_eq!(cfg["hooks"]["SubagentStart"][0]["hooks"][0], json!({ "type": "command", "command": format!("{cmd} --hook start") }));
        assert_eq!(cfg["hooks"]["SubagentStop"][0]["hooks"][0]["command"], format!("{cmd} --hook stop"));
        assert!(!dir.join("settings.json.bak-a2a").exists(), "nothing to back up on a new file");
        let before = std::fs::read_to_string(&file).unwrap();
        assert!(install(&file, Path::new("/x/my bin/codetypera2a")).unwrap().is_empty(), "second run reports nothing");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "second run changes nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_session_start_hook_runs_ensure_beside_the_users_own_and_only_once() {
        let dir = tmp("ensure");
        let file = dir.join("settings.json");
        std::fs::write(&file, r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo mine"}]}]}}"#).unwrap();
        install(&file, Path::new("/x/a")).unwrap();
        let groups = read(&file)["hooks"]["SessionStart"].as_array().unwrap().clone();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["hooks"][0]["command"], "echo mine");
        assert_eq!(groups[1]["hooks"][0]["command"], "'/x/a' oc statusline --hook ensure");
        let before = std::fs::read_to_string(&file).unwrap();
        assert!(install(&file, Path::new("/x/a")).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "second run is byte-identical");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_inbox_hook_is_added_beside_the_users_prompt_hook_and_only_once() {
        let dir = tmp("inbox");
        let file = dir.join("settings.json");
        std::fs::write(&file, r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"echo mine"}]}]}}"#).unwrap();
        install(&file, Path::new("/x/a")).unwrap();
        let cfg = read(&file);
        let groups = cfg["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["hooks"][0]["command"], "echo mine");
        assert_eq!(groups[1]["hooks"][0]["command"], "'/x/a' oc statusline --hook inbox");
        let before = std::fs::read_to_string(&file).unwrap();
        assert!(install(&file, Path::new("/x/a")).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "second run is byte-identical");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeps_the_users_status_line_and_hooks_and_backs_up() {
        let dir = tmp("mine");
        let file = dir.join("settings.json");
        let mine = json!({
            "theme": "dark",
            "statusLine": { "type": "command", "command": "~/mine.sh" },
            "hooks": { "SubagentStart": [{ "hooks": [{ "type": "command", "command": "echo mine" }] }], "Stop": [] }
        });
        std::fs::write(&file, mine.to_string()).unwrap();
        let notes = install(&file, Path::new("/x/a")).unwrap();
        assert!(notes.iter().any(|n| n.contains("already has a status line")), "{notes:?}");
        let cfg = read(&file);
        assert_eq!(cfg["theme"], "dark");
        assert_eq!(cfg["statusLine"]["command"], "~/mine.sh", "never overwritten");
        assert_eq!(cfg["hooks"]["SubagentStart"][0]["hooks"][0]["command"], "echo mine", "user hook kept first");
        assert_eq!(cfg["hooks"]["SubagentStart"].as_array().unwrap().len(), 2, "ours is appended");
        assert_eq!(cfg["hooks"]["SubagentStop"].as_array().unwrap().len(), 1);
        assert_eq!(read(&dir.join("settings.json.bak-a2a")), mine, "backup is the original");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_a2a_status_line_is_recognised_even_from_another_path() {
        let dir = tmp("ours");
        let file = dir.join("settings.json");
        let live = std::env::current_exe().unwrap();
        let ours = format!("{} oc statusline", live.display());
        std::fs::write(&file, json!({ "statusLine": { "type": "command", "command": ours } }).to_string()).unwrap();
        install(&file, Path::new("/new/codetypera2a")).unwrap();
        assert_eq!(read(&file)["statusLine"]["command"], ours, "its binary still exists: left alone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn our_entries_with_a_dead_binary_path_are_repaired_and_the_users_are_not() {
        let dir = tmp("repair");
        let file = dir.join("settings.json");
        let doc = json!({
            "statusLine": { "type": "command", "command": "'/gone/codetypera2a' oc statusline", "refreshInterval": 5 },
            "hooks": {
                "SessionStart": [{ "hooks": [{ "type": "command", "command": "/mine/hook.sh" }] }, { "hooks": [{ "type": "command", "command": "'/gone/codetypera2a' oc statusline --hook ensure" }] }],
                "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "/gone/oc statusline --hook inbox" }] }]
            }
        });
        std::fs::write(&file, doc.to_string()).unwrap();
        let notes = install(&file, Path::new("/new/bin/codetypera2a")).unwrap();
        assert!(notes[0].contains("added to"), "{notes:?}");
        let cfg = read(&file);
        assert_eq!(cfg["statusLine"]["command"], "'/new/bin/codetypera2a' oc statusline");
        assert_eq!(cfg["hooks"]["SessionStart"][0]["hooks"][0]["command"], "/mine/hook.sh");
        assert_eq!(cfg["hooks"]["SessionStart"][1]["hooks"][0]["command"], "'/new/bin/codetypera2a' oc statusline --hook ensure");
        assert_eq!(cfg["hooks"]["SessionStart"].as_array().unwrap().len(), 2, "repaired in place, not added");
        assert_eq!(cfg["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"], "'/new/bin/codetypera2a' oc statusline --hook inbox");
        let before = std::fs::read_to_string(&file).unwrap();
        assert!(install(&file, Path::new("/new/bin/codetypera2a")).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "a second run changes nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_file_is_left_alone() {
        let dir = tmp("broken");
        let file = dir.join("settings.json");
        std::fs::write(&file, "{ nope").unwrap();
        assert!(install(&file, Path::new("/x/a")).unwrap_err().contains("left untouched"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{ nope");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
