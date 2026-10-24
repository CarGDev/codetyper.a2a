//! `codetypera2a init` (no flags) and `init --claude` must end with the same Claude Code setup, and
//! choosing only opencode must not touch the Claude dir. A fake `claude` on PATH stands in for the real
//! one, so these runs never touch `~/.claude`.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_codetypera2a");

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// `with_claude` / `with_opencode`: which fake tools are on PATH (and so "detected").
    fn new(name: &str, with_claude: bool, with_opencode: bool) -> Sandbox {
        let root = std::env::temp_dir().join(format!("a2a-init-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        let script = |name: &str, body: String| {
            let path = root.join("bin").join(name);
            std::fs::write(&path, body).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let log = root.join("claude-calls.log");
        let claude = format!("#!/bin/sh\necho \"$*\" >> {}\ncase \"$*\" in *list*) echo '[]';; *) echo ok;; esac\n", log.display());
        with_claude.then(|| script("claude", claude));
        with_opencode.then(|| script("opencode", "#!/bin/sh\nexit 0\n".into()));
        Sandbox { root }
    }

    /// Run `init <args>` with its own Claude and opencode config dirs.
    fn init(&self, config: &str, args: &[&str]) -> std::process::Output {
        Command::new(BIN)
            .arg("init")
            .args(args)
            .args(["--no-models", "--no-link"])
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("PATH", format!("{}:/usr/bin:/bin", self.root.join("bin").display()))
            .env("CLAUDE_BIN", self.root.join("bin/claude"))
            .env("CLAUDE_CONFIG_DIR", self.root.join(config).join("claude"))
            .env("OPENCODE_CONFIG_DIR", self.root.join(config).join("opencode"))
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn claude_calls(&self) -> String {
        std::fs::read_to_string(self.root.join("claude-calls.log")).unwrap_or_default()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Every file under `dir` (relative path -> contents with the dir's own path blanked out).
fn tree(dir: &Path) -> BTreeMap<String, String> {
    fn walk(base: &Path, at: &Path, out: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(at).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
                continue;
            }
            let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).replace(&base.display().to_string(), "<D>");
            out.insert(path.strip_prefix(base).unwrap().display().to_string(), text);
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

#[test]
fn plain_init_and_init_claude_leave_the_same_claude_setup() {
    let sb = Sandbox::new("same", true, false);
    std::fs::create_dir_all(sb.root.join("home/.claude")).unwrap();
    let flagged = sb.init("a", &["--claude", "--yes"]);
    assert!(flagged.status.success(), "{}", String::from_utf8_lossy(&flagged.stderr));
    let calls_flagged = sb.claude_calls();
    let plain = sb.init("b", &["--yes"]);
    assert!(plain.status.success(), "{}", String::from_utf8_lossy(&plain.stderr));
    let (a, b) = (tree(&sb.root.join("a/claude")), tree(&sb.root.join("b/claude")));
    assert!(a.contains_key("agents/orchestrator.md") && a.contains_key("settings.json") && a.contains_key(".a2a-manifest.json"), "{:?}", a.keys().collect::<Vec<_>>());
    assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
    for (file, text) in &a {
        assert_eq!(text, &b[file], "{file} differs between `init --claude` and plain `init`");
    }
    assert!(a["settings.json"].contains("--hook ensure"), "the SessionStart hook is installed");
    assert!(calls_flagged.contains("plugin install a2a-pane@codetypera2a"), "{calls_flagged}");
    assert_eq!(sb.claude_calls().replace(&calls_flagged, ""), calls_flagged.replace("/a/claude", "/b/claude"), "the same claude commands ran");
    assert!(!sb.root.join("b/opencode").exists(), "plain init with only Claude Code detected does not touch opencode");
}

#[test]
fn choosing_only_opencode_does_not_touch_the_claude_dir() {
    let sb = Sandbox::new("oc", false, true);
    let out = sb.init("c", &["--yes"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(sb.root.join("c/opencode/agents/orchestrator.md").exists());
    assert!(!sb.root.join("c/claude").exists(), "no Claude Code files");
    assert!(sb.claude_calls().is_empty());
    let flag = sb.init("d", &["--opencode"]);
    assert!(flag.status.success());
    assert!(!sb.root.join("d/claude").exists());
}
