//! `oc` repairs a stale a2a binary path left behind by a reinstall, except for side-effect-free help.
use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_codetypera2a");
const STALE: &str = "/gone/bin/codetypera2a";

fn setup(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("a2a-heal-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("a2a")).unwrap();
    std::fs::write(
        dir.join("opencode.jsonc"),
        format!(r#"{{"mcp":{{"servers":{{"a2a":{{"type":"local","command":["{STALE}","hub"],"codemode":false}}}}}}}}"#),
    )
    .unwrap();
    std::fs::write(dir.join("a2a/codetypera2a.path"), STALE).unwrap();
    dir
}

fn oc(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .arg("oc")
        .args(args)
        .env("OPENCODE_CONFIG_DIR", dir)
        .env("A2A_DB", dir.join("hub.db"))
        .env("A2A_DISPATCHER_PID", dir.join("d.pid"))
        .env("A2A_NO_DISPATCHER", "1")
        .env("OPENCODE_BIN", "/usr/bin/false")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .expect("run")
}

#[test]
fn oc_repairs_stale_mcp_path() {
    let dir = setup("fix");
    let out = oc(&dir, &["status"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("oc: mcp server a2a path repaired"), "{err}");
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("opencode.jsonc")).unwrap())
            .unwrap();
    let cmd = cfg["mcp"]["servers"]["a2a"]["command"][0].as_str().unwrap();
    assert_ne!(cmd, STALE);
    assert!(Path::new(cmd).is_file(), "{cmd}");
    assert_eq!(cfg["mcp"]["servers"]["a2a"]["codemode"], false);
    assert!(dir.join("opencode.jsonc.bak-a2a").exists());
    assert_ne!(
        std::fs::read_to_string(dir.join("a2a/codetypera2a.path")).unwrap(),
        STALE
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn oc_help_and_version_do_not_heal() {
    let dir = setup("help");
    let before = std::fs::read_to_string(dir.join("opencode.jsonc")).unwrap();
    for flag in ["--help", "-h", "help", "version", "--version", "-V"] {
        oc(&dir, &[flag]);
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("opencode.jsonc")).unwrap(),
        before
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("a2a/codetypera2a.path")).unwrap(),
        STALE
    );
    assert!(!dir.join("opencode.jsonc.bak-a2a").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
