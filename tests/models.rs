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
    std::fs::write(
        dir.join("opencode.jsonc"),
        r#"{"model":"x/y","agents":{"mentor":{"temperature":0.3}}}"#,
    )
    .unwrap();
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
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

#[test]
fn show_prints_recommendations_and_changes_nothing() {
    let (dir, stub) = setup("models-show");
    let before = std::fs::read_to_string(dir.join("opencode.jsonc")).unwrap();
    let (out, ok) = run(&dir, &stub, &["--show"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("architect") && out.contains("github-copilot/claude-sonnet-5.5"),
        "{out}"
    );
    assert!(out.contains("my-own-agent"), "own agents are listed: {out}");
    assert!(!out.contains("title"), "hidden agents are not: {out}");
    assert_eq!(
        std::fs::read_to_string(dir.join("opencode.jsonc")).unwrap(),
        before
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recommended_writes_models_by_role_and_keeps_the_rest() {
    let (dir, stub) = setup("models-rec");
    let (out, ok) = run(&dir, &stub, &["--recommended"]);
    assert!(ok, "{out}");
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("opencode.jsonc")).unwrap())
            .unwrap();
    assert_eq!(cfg["model"], "x/y");
    assert_eq!(cfg["agents"]["mentor"]["temperature"], 0.3);
    assert_eq!(
        cfg["agents"]["brain"]["model"],
        "github-copilot/claude-haiku-5.5"
    ); // cheap
    assert_eq!(
        cfg["agents"]["orchestrator"]["model"],
        "github-copilot/claude-sonnet-5.5"
    ); // balanced
    assert_eq!(
        cfg["agents"]["architect"]["model"],
        "github-copilot/claude-sonnet-5.5"
    ); // balanced, deep is opt-in
    assert_eq!(
        cfg["agents"]["code-reviewer"]["model"],
        "github-copilot/claude-sonnet-5.5"
    ); // balanced
    assert!(
        cfg["agents"].get("my-own-agent").is_none(),
        "no recommendation for someone's own agent"
    );
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

fn limited_stub(dir: &Path, models: &str) -> PathBuf {
    let stub = dir.join("opencode-limited");
    let agents = r#"{"data":[{"id":"build","description":"d","mode":"primary","hidden":false},{"id":"brain","description":"d","mode":"primary","hidden":false}]}"#;
    std::fs::write(
        &stub,
        format!("#!/bin/sh\ncase \"$1\" in\n  models) printf '{models}';;\n  api) echo '{agents}';;\nesac\n"),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    stub
}

fn seed_agent_files(dir: &Path) {
    std::fs::create_dir_all(dir.join("agents")).unwrap();
    for name in ["build", "brain"] {
        std::fs::write(
            dir.join("agents").join(format!("{name}.md")),
            "---\ndescription: d\nmodel: github-copilot/claude-sonnet-5.5\n---\nbody\n",
        )
        .unwrap();
    }
}

fn file_model(dir: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("agents").join(format!("{name}.md"))).unwrap();
    text.lines()
        .find_map(|l| l.strip_prefix("model: "))
        .map(String::from)
}

#[test]
fn recommended_without_github_copilot_leaves_only_installed_models() {
    let (dir, _) = setup("models-nocopilot");
    seed_agent_files(&dir);
    let stub = limited_stub(&dir, "x/gpt-6-luna\\nx/grok-4.5\\nx/kimi-k3\\n");
    let (out, ok) = run(&dir, &stub, &["--recommended"]);
    assert!(ok, "{out}");
    let installed = ["x/gpt-6-luna", "x/grok-4.5", "x/kimi-k3"];
    for name in ["build", "brain"] {
        let model = file_model(&dir, name).expect("a model fits");
        assert!(installed.contains(&model.as_str()), "{name}: {model}");
    }
    let cfg = std::fs::read_to_string(dir.join("opencode.jsonc")).unwrap();
    assert!(!cfg.contains("github-copilot"), "{cfg}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recommended_clears_the_model_line_when_no_installed_model_has_a_price() {
    let (dir, _) = setup("models-noprice");
    seed_agent_files(&dir);
    let stub = limited_stub(&dir, "x/unpriced\\n");
    let (out, ok) = run(&dir, &stub, &["--recommended"]);
    assert!(ok, "{out}");
    assert_eq!(file_model(&dir, "build"), None);
    assert!(out.contains("-> default"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn init_rewrites_installed_agent_files_to_installed_models_only() {
    let (dir, _) = setup("models-init");
    let stub = limited_stub(&dir, "x/gpt-6-luna\\nx/grok-4.5\\nx/kimi-k3\\n");
    let config = dir.join("oc");
    let out = Command::new(BIN)
        .args(["init", "--yes", "--opencode", "--no-link"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir.join("home"))
        .env("OPENCODE_CONFIG_DIR", &config)
        .env("CLAUDE_CONFIG_DIR", dir.join("claude"))
        .env("CODETYPERA2A_HOME", dir.join("runtime"))
        .env("A2A_DB", dir.join("hub.db"))
        .env("A2A_NO_DISPATCHER", "1")
        .env("OPENCODE_BIN", &stub)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("init: brain: github-copilot/claude-haiku-5.5 -> x/gpt-6-luna"),
        "{text}"
    );
    let installed = ["x/gpt-6-luna", "x/grok-4.5", "x/kimi-k3"];
    let mut checked = 0;
    for entry in std::fs::read_dir(config.join("agents")).unwrap().flatten() {
        let body = std::fs::read_to_string(entry.path()).unwrap();
        if let Some(model) = body.lines().find_map(|l| l.strip_prefix("model: ")) {
            assert!(installed.contains(&model), "{:?}: {model}", entry.path());
            checked += 1;
        }
    }
    assert!(checked >= 22, "only {checked} agent files carry a model");
    let _ = std::fs::remove_dir_all(&dir);
}

fn deep_run(home: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    Command::new(BIN)
        .arg("models")
        .arg("deep")
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("CODETYPERA2A_HOME", home.join("runtime"))
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

fn deep_sandbox(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("a2a-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("runtime")).unwrap();
    dir
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn deep_add_and_remove_persist_and_are_idempotent() {
    let home = deep_sandbox("deep-persist");
    let file = home.join("runtime/deep-agents.json");
    for _ in 0..2 {
        let out = deep_run(&home, &["add", "architect", "plan"], &[]);
        assert_eq!(out.status.code(), Some(0), "{}", stdout_of(&out));
        assert!(
            stdout_of(&out).contains("applies at the next"),
            "{}",
            stdout_of(&out)
        );
    }
    let stored = std::fs::read_to_string(&file).unwrap();
    assert_eq!(
        stored.replace(char::is_whitespace, ""),
        r#"["architect","plan"]"#
    );
    let listed = stdout_of(&deep_run(&home, &["list"], &[]));
    assert!(
        listed.contains("architect") && listed.contains("plan"),
        "{listed}"
    );
    for _ in 0..2 {
        assert_eq!(
            deep_run(&home, &["remove", "plan"], &[]).status.code(),
            Some(0)
        );
    }
    let stored = std::fs::read_to_string(&file).unwrap();
    assert_eq!(stored.replace(char::is_whitespace, ""), r#"["architect"]"#);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn deep_rejects_bad_usage_and_bad_names_without_writing() {
    let home = deep_sandbox("deep-usage");
    let file = home.join("runtime/deep-agents.json");
    for args in [&["frobnicate"][..], &["add"], &["remove"], &[]] {
        let out = deep_run(&home, args, &[]);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("usage"),
            "{args:?}"
        );
    }
    for bad in ["../x", "Architect", "nobody"] {
        let out = deep_run(&home, &["add", "architect", bad], &[]);
        assert_eq!(out.status.code(), Some(1), "{bad}");
    }
    assert!(!file.exists(), "a rejected add must write nothing");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn deep_never_overwrites_a_corrupt_file() {
    let home = deep_sandbox("deep-corrupt");
    let file = home.join("runtime/deep-agents.json");
    std::fs::write(&file, "{ not json").unwrap();
    for args in [&["add", "plan"][..], &["remove", "plan"]] {
        let out = deep_run(&home, args, &[]);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{ not json");
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("not json"),
            "contents leaked"
        );
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_env_only_deep_agent_is_listed_tagged_as_env() {
    let home = deep_sandbox("deep-env");
    deep_run(&home, &["add", "architect"], &[]);
    let listed = stdout_of(&deep_run(&home, &["list"], &[("A2A_DEEP_AGENTS", "plan")]));
    assert!(
        listed
            .lines()
            .any(|l| l.contains("plan") && l.contains("env")),
        "{listed}"
    );
    assert!(
        listed
            .lines()
            .any(|l| l.contains("architect") && l.contains("file")),
        "{listed}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
