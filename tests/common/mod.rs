//! Fixtures shared by several integration-test crates; each crate uses only some of them.
#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub const BIN: &str = env!("CARGO_BIN_EXE_codetypera2a");

pub struct Sandbox {
    pub root: PathBuf,
}

impl Sandbox {
    /// `with_claude` / `with_opencode`: which fake tools are on PATH (and so "detected").
    pub fn new(name: &str, with_claude: bool, with_opencode: bool) -> Sandbox {
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
        let claude = format!(
            "#!/bin/sh\necho \"$*\" >> {}\ncase \"$*\" in *list*) echo '[]';; *) echo ok;; esac\n",
            log.display()
        );
        with_claude.then(|| script("claude", claude));
        with_opencode.then(|| script("opencode", "#!/bin/sh\nexit 0\n".into()));
        Sandbox { root }
    }

    /// Run `init <args>` with its own Claude and opencode config dirs.
    pub fn init(&self, config: &str, args: &[&str]) -> std::process::Output {
        Command::new(BIN)
            .arg("init")
            .args(args)
            .args(["--no-models", "--no-link"])
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("CLAUDE_BIN", self.root.join("bin/claude"))
            .env("CLAUDE_CONFIG_DIR", self.root.join(config).join("claude"))
            .env(
                "OPENCODE_CONFIG_DIR",
                self.root.join(config).join("opencode"),
            )
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    pub fn claude_calls(&self) -> String {
        std::fs::read_to_string(self.root.join("claude-calls.log")).unwrap_or_default()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn sandbox(name: &str) -> PathBuf {
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

/// Runs the binary with nothing inherited but what a clean machine has: the real temp dir, `OLLAMA_HOST`,
/// every `A2A_*` and config override stay out, and `TMPDIR` points inside the sandbox.
pub fn run(home: &Path, args: &[&str]) -> Output {
    let tmp = home.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    Command::new(BIN)
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("SHELL", "/bin/zsh")
        .env("OPENCODE_BIN", "/usr/bin/false")
        .env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", &tmp)
        .output()
        .unwrap()
}

pub fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}
