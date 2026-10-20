//! `codetypera2a models` without a terminal: `--show` prints, `--recommended` writes agents.<name>.model.
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_codetypera2a");

fn setup(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("a2a-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let stub = dir.join("opencode");
    let agents = r#"{"data":[{"id":"orchestrator","description":"d","mode":"primary","hidden":false},{"id":"brain","description":"d","mode":"primary","hidden":false},{"id":"architect","description":"d","mode":"primary","hidden":false},{"id":"code-reviewer","description":"d","mode":"subagent","hidden":false},{"id":"my-own-agent","description":"mine","mode":"primary","hidden":false},{"id":"title","description":"x","mode":"primary","hidden":true}]}"#;
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  models) printf 'github-copilot/claude-haiku-5.5\\ngithub-copilot/claude-sonnet-5.5\\ngithub-copilot/claude-opus-5.5\\ngithub-copilot/gpt-6.1-sol\\n';;\n  api) echo '{agents}';;\nesac\n"
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(dir.join("opencode.jsonc"), r#"{"model":"x/y","agents":{"mentor":{"temperature":0.3}}}"#).unwrap();
    (dir.clone(), stub)
}

fn run(dir: &Path, stub: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(BIN)
        .arg("models")
        .args(args)
        .arg("--dir")
        .arg(dir)
        .env("OPENCODE_BIN", stub)
        .output()
        .unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.success())
}

#[test]
fn show_prints_recommendations_and_changes_nothing() {
    let (dir, stub) = setup("models-show");
    let before = std::fs::read_to_string(dir.join("opencode.jsonc")).unwrap();
    let (out, ok) = run(&dir, &stub, &["--show"]);
    assert!(ok, "{out}");
    assert!(out.contains("architect") && out.contains("github-copilot/claude-opus-5.5"), "{out}");
    assert!(out.contains("my-own-agent"), "own agents are listed: {out}");
    assert!(!out.contains("title"), "hidden agents are not: {out}");
    assert_eq!(std::fs::read_to_string(dir.join("opencode.jsonc")).unwrap(), before);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recommended_writes_models_by_role_and_keeps_the_rest() {
    let (dir, stub) = setup("models-rec");
    let (out, ok) = run(&dir, &stub, &["--recommended"]);
    assert!(ok, "{out}");
    let cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("opencode.jsonc")).unwrap()).unwrap();
    assert_eq!(cfg["model"], "x/y");
    assert_eq!(cfg["agents"]["mentor"]["temperature"], 0.3);
    assert_eq!(cfg["agents"]["brain"]["model"], "github-copilot/claude-haiku-5.5"); // cheap
    assert_eq!(cfg["agents"]["orchestrator"]["model"], "github-copilot/claude-sonnet-5.5"); // balanced
    assert_eq!(cfg["agents"]["architect"]["model"], "github-copilot/claude-opus-5.5"); // deep
    assert_eq!(cfg["agents"]["code-reviewer"]["model"], "github-copilot/gpt-6.1-sol"); // other family
    assert!(cfg["agents"].get("my-own-agent").is_none(), "no recommendation for someone's own agent");
    assert!(dir.join("opencode.jsonc.bak-a2a-models").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn without_a_terminal_it_only_explains() {
    let (dir, stub) = setup("models-notty");
    let (out, ok) = run(&dir, &stub, &[]);
    assert!(ok);
    assert!(out.contains("not a terminal"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
