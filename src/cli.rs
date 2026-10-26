//! `help` and `init` commands.
use include_dir::{Dir, include_dir};
use std::path::{Path, PathBuf};

/// Agent templates, embedded at build time so install does not depend on the repo path.
pub(crate) static AGENTS: Dir = include_dir!("$CARGO_MANIFEST_DIR/agents");
pub(crate) static COMMANDS: Dir = include_dir!("$CARGO_MANIFEST_DIR/commands");
static PLUGINS: Dir = include_dir!("$CARGO_MANIFEST_DIR/plugins");
/// The Claude Code pane mod (lives under `plugins/` but belongs to Claude Code, never to opencode).
pub(crate) static CLAUDE_MOD: Dir = include_dir!("$CARGO_MANIFEST_DIR/plugins/claude-a2a");
pub(crate) const CLAUDE_MOD_DIR: &str = "claude-a2a";
static POLICY: &str = include_str!("../model-policy.json");
static PROTOCOL: &str = include_str!("../a2a/PROTOCOL.md");

pub const HELP: &str = "\
codetypera2a - agent-to-agent hub for opencode and Claude Code

USAGE:
  codetypera2a <command> [options]

COMMANDS:
  version        Print the version (also --version, -V)
  init           Set up the tools you pick (a screen on a terminal; --claude / --opencode / --all skip it).
                 opencode: install agents, slash commands, the tabs plugin, PROTOCOL.md + model policy
                 into the opencode config dir and register the
                 MCP servers a2a, codegraph, context7, engram, playwright in opencode.jsonc
                 (opencode v2: mcp.servers; servers already configured are left alone)
                 Claude Code: install the same agents and commands, converted to its format, in
                 ~/.claude and register the same MCP servers in ~/.claude.json
  models         Choose each agent's model from the models opencode has (terminal UI);
                 --recommended applies the suggested ones, --show prints the table
  uninstall      Delete everything codetypera2a put on this machine, for good (no backups): the agent and command files
                 `init` installed in ~/.claude and the opencode config dir (never files you wrote), our MCP/plugin/hook/status-line entries, the shell
                 block, the runtime homes and databases (also the legacy ones, only when they are ours), our dispatchers and
                 the binary when cargo or brew owns it (and an oc link to it). Dry run unless --yes; --keep-binary keeps it
  reinstall      Same clean-up but keeps this binary, then runs init --yes and verifies: no dispatcher, no legacy
                 database, empty database. Dry run unless --yes. Update the binary first: cargo install --path . --force
  hub            Run the A2A hub (MCP server over stdio; register it in opencode.jsonc)
  dispatcher     Run the dispatcher daemon that wakes sessions when the hub has work
  oc [cmd]       Run opencode in this pane with the dispatcher alongside: (none) | dispatch
                 (foreground, same pane) | agent <name> | status | stop | continue <contextId>.
                 Also available by running the binary as `oc` (init links it)
  check-models   Lint configured models against model-policy.json
  e2e-test       Run the hub end-to-end test
  eval           Replay deterministic scenarios (routing, hop cap, no duplicate nudges, seeded-bug
                 reviewer) against a fake opencode; --list names them, <name> runs one; exit 0 = no FAIL
  help           Show this help

INIT OPTIONS:
  --claude       Set up Claude Code (agents, commands, MCP servers)
  --opencode     Set up opencode
  --all          Set up every tool init supports
  --yes          Do not show the screen: set up the tools it finds (opencode when none)
  --claude-dir <path>  Target Claude Code config dir
                 (default: $CLAUDE_CONFIG_DIR, else ~/.claude; its MCP file is <dir>/.claude.json
                 when CLAUDE_CONFIG_DIR is set, else ~/.claude.json)
  --dir <path>   Target opencode config dir
                 (default: $OPENCODE_CONFIG_DIR, else ~/.config/opencode)
  --shell        Add alias opencode='oc' to your shell rc (zsh/bash) so a bare `opencode`
                 starts the dispatcher and, in tmux, the live status window
  --tools        First run the official opencode setup of the tools you already have
                 (`engram setup opencode`, `codegraph install`); interactive
  --no-models    Skip the screen that sets each agent's model (run `codetypera2a models` later)
  --no-link      Do not create the `oc` symlink next to the binary
  --no-default-agent  Do not make the orchestrator Claude Code's main agent (`agent` in settings.json;
                 an agent you already chose is never replaced)
  --ollama-url <url>  Also look for Ollama models on this host (repeatable, e.g. http://100.99.194.92) and add
                 them to opencode.jsonc as provider ollama-<host>; remembered in <dir>/a2a/ollama.json.
                 The local Ollama (127.0.0.1:11434) and $OLLAMA_HOST are always checked
  --no-ollama    Skip the Ollama discovery
  --force        Overwrite files that already exist and replace the a2a MCP entry
  --md-only      Refresh only the agent/command/PROTOCOL markdown (keeps opencode.jsonc, MCP entries,
                 plugin and models; saved models are put back into the agent files). Also: `update`
                 (default: existing files are skipped)

ENV:
  A2A_DB, A2A_AGENT_DIR, A2A_POLICY, A2A_DISPATCHER_PID, OPENCODE_BIN, OC_MODEL, OC_MODEL_FORCE,
  DISPATCH_POLL_MS, DISPATCH_MAX_HOPS, DISPATCH_STALL_SECS, DISPATCH_DRY_RUN
  DISPATCH_BACKEND, DISPATCH_BACKEND_<AGENT>  run all / one agent on claude or opencode (default opencode)
  DISPATCH_ONLY  claude | opencode: never build, ask or wake the other runtime (optional; a runtime with
                 no open work is not contacted anyway)
  A2A_CREDENTIAL_FILE  one-shot 0600 credential the trusted runtime hands the hub; used only when
                 <runtime home>/auth/trusted-keys exists (then only the signed orchestrator can create tasks)
  A2A_CLAUDE_AUTH  auto | subscription | api: which credentials headless Claude turns use when both a
                 subscription login and ANTHROPIC_API_KEY exist (auto = the subscription)
  CODETYPERA2A_HOME  runtime home (default ~/.codetypera2a)

CHECK-MODELS OPTIONS:
  --config <f>   opencode.jsonc to read
  --policy <f>   model-policy.json to read
  --agents <d>   directory of agent .md files
  --strict       Also fail on agents without a model table entry
";

pub fn help() -> i32 {
    print!("{HELP}");
    0
}

fn config_dir(args: &[String]) -> Result<PathBuf, String> {
    match args.iter().position(|a| a == "--dir") {
        Some(i) => args
            .get(i + 1)
            .map(PathBuf::from)
            .ok_or_else(|| "--dir needs a path".to_string()),
        None => Ok(crate::paths::config_dir()),
    }
}

/// Every embedded file under `dir`, with its path relative to the embedded root.
fn embedded_files<'a>(dir: &'a Dir<'a>, out: &mut Vec<(PathBuf, &'a [u8])>) {
    out.extend(dir.files().map(|f| (f.path().to_path_buf(), f.contents())));
    for sub in dir.dirs() {
        embedded_files(sub, out);
    }
}

/// Write `contents` to `target` unless it exists (and `force` is off). Returns whether it wrote.
pub(crate) fn install(target: &Path, contents: &[u8], force: bool) -> Result<bool, String> {
    if target.exists() && !force {
        return Ok(false);
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(target, contents)
        .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
    Ok(true)
}

/// The path to register in opencode.jsonc: the one the user invoked (e.g. /opt/homebrew/bin/...),
/// not the resolved target, so it survives `brew upgrade` (which replaces the versioned Cellar dir).
pub(crate) fn stable_exe() -> PathBuf {
    let invoked = std::env::args_os().next().map(PathBuf::from);
    let abs = invoked.and_then(|p| {
        if p.components().count() > 1 {
            if p.is_absolute() {
                Some(p)
            } else {
                std::env::current_dir().ok().map(|d| d.join(p))
            }
        } else {
            which(&p.to_string_lossy())
        }
    });
    abs.filter(|p| p.is_file())
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("codetypera2a"))
}

/// First `name` found on PATH.
pub(crate) fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// The other MCP servers the setup expects next to `a2a` (opencode v2 shape). Only added when
/// missing; local commands are resolved on PATH so a Homebrew or cargo install both work.
pub(crate) fn default_mcp_servers() -> Vec<(&'static str, serde_json::Value)> {
    use serde_json::json;
    let bin = |name: &str| {
        which(name)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| name.to_string())
    };
    vec![
        (
            "codegraph",
            json!({ "type": "local", "command": ["codegraph", "serve", "--mcp"], "codemode": false }),
        ),
        (
            "context7",
            json!({ "type": "remote", "url": "https://mcp.context7.com/mcp" }),
        ),
        (
            "engram",
            json!({ "type": "local", "command": [bin("engram"), "mcp", "--tools=agent"] }),
        ),
        (
            "playwright",
            json!({ "type": "local", "command": ["npx", "-y", "@playwright/mcp@latest"] }),
        ),
    ]
}

/// Official opencode setup commands of the companion tools (from their READMEs):
/// engram: https://github.com/Gentleman-Programming/engram, codegraph: https://github.com/colbymchenry/codegraph
const TOOL_SETUPS: [(&str, &[&str]); 2] = [
    ("engram", &["setup", "opencode"]),
    ("codegraph", &["install"]),
];

/// Run each installed tool's own setup (interactive, output shown). Opt-in via `--tools`.
fn run_tool_setups() {
    for (bin, args) in TOOL_SETUPS {
        let Some(path) = which(bin) else {
            println!(
                "init: skipping `{bin} {}` ({bin} is not installed, see the steps below)",
                args.join(" ")
            );
            continue;
        };
        println!("init: running `{bin} {}`", args.join(" "));
        match std::process::Command::new(path).args(args).status() {
            Ok(st) if st.success() => {}
            Ok(st) => eprintln!("init: `{bin} {}` exited with {st}", args.join(" ")),
            Err(e) => eprintln!("init: cannot run {bin}: {e}"),
        }
    }
}

/// What is still left to do for the companion MCP servers, as exact commands.
fn tool_next_steps() -> Vec<String> {
    let mut steps = Vec::new();
    if which("engram").is_none() {
        steps.push(
            "engram: brew install gentleman-programming/tap/engram && engram setup opencode"
                .to_string(),
        );
    }
    if which("codegraph").is_none() {
        steps.push("codegraph: curl -fsSL https://raw.githubusercontent.com/colbymchenry/codegraph/main/install.sh | sh   (or: npm i -g @colbymchenry/codegraph), then `codegraph install`".to_string());
    }
    steps.push("codegraph: run `codegraph init` once in each project you want indexed".to_string());
    steps.push(
        "context7: works as configured; for an API key + skill run `npx ctx7 setup --opencode`"
            .to_string(),
    );
    steps
}

/// If the a2a entry's `command[0]` is a path to a file that no longer exists, point `command` at
/// `[exe, "hub"]` (other keys kept). Returns the old path when it repaired something.
fn repair_command(entry: &mut serde_json::Value, exe: &Path) -> Option<String> {
    let old = entry.get("command")?.get(0)?.as_str()?.to_string();
    if !old.contains('/') || Path::new(&old).is_file() {
        return None;
    }
    entry["command"] = serde_json::json!([exe.display().to_string(), "hub"]);
    Some(old)
}

/// Self-heal after a reinstall moved the binary: repair a stale a2a MCP command in
/// `opencode.jsonc` and a stale `a2a/codetypera2a.path`. Returns a message only when something
/// changed or a manual hint is needed; IO errors are swallowed (`None`).
pub fn heal(root: &Path, exe: &Path) -> Option<String> {
    let mut msgs = Vec::new();
    let cfg_path = root.join("opencode.jsonc");
    if let Ok(text) = std::fs::read_to_string(&cfg_path) {
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(mut cfg) => {
                let old = cfg
                    .pointer_mut("/mcp/servers/a2a")
                    .and_then(|s| repair_command(s, exe));
                if let Some(old) = old {
                    let bak = root.join("opencode.jsonc.bak-a2a");
                    let out = serde_json::to_string_pretty(&cfg).ok()?;
                    if std::fs::copy(&cfg_path, &bak).is_ok()
                        && std::fs::write(&cfg_path, out + "\n").is_ok()
                    {
                        msgs.push(format!(
                            "mcp server a2a path repaired ({old} -> {})",
                            exe.display()
                        ));
                    }
                }
            }
            Err(_) => {
                if has_stale_binary_path(&text) {
                    msgs.push(format!(
                        "{} has a stale a2a binary path; set mcp.servers.a2a.command to [\"{}\", \"hub\"]",
                        cfg_path.display(),
                        exe.display()
                    ));
                }
            }
        }
    }
    let path_file = root.join("a2a").join("codetypera2a.path");
    if let Ok(recorded) = std::fs::read_to_string(&path_file) {
        let recorded = recorded.trim();
        if !recorded.is_empty()
            && !Path::new(recorded).exists()
            && std::fs::write(&path_file, exe.display().to_string()).is_ok()
        {
            msgs.push(format!(
                "codetypera2a.path repaired ({recorded} -> {})",
                exe.display()
            ));
        }
    }
    if msgs.is_empty() {
        None
    } else {
        Some(msgs.join("; "))
    }
}

/// Crude scan of commented JSONC: any quoted absolute path ending in `codetypera2a` that is gone.
fn has_stale_binary_path(text: &str) -> bool {
    text.match_indices("codetypera2a\"").any(|(i, _)| {
        let head = &text[..i + "codetypera2a".len()];
        head.rfind('"').is_some_and(|q| {
            let p = &head[q + 1..];
            p.starts_with('/') && !Path::new(p).is_file()
        })
    })
}

/// Register the hub as an MCP server in `opencode.jsonc` (opencode v2: `mcp.servers.<name>`,
/// `codemode: false` so agents call `a2a_*` tools directly). Returns a status line.
fn register_mcp(root: &Path, exe: &Path, force: bool) -> Result<String, String> {
    use serde_json::{Value, json};
    let path = root.join("opencode.jsonc");
    let entry = json!({
        "type": "local",
        "command": [exe.display().to_string(), "hub"],
        "codemode": false,
    });
    let mut cfg: Value = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                return Ok(format!(
                    "{} has comments or trailing commas, left untouched; add this under mcp.servers.a2a:\n{}",
                    path.display(),
                    serde_json::to_string_pretty(&entry).unwrap_or_default()
                ));
            }
        }
    } else {
        json!({ "$schema": "https://opencode.ai/config.json" })
    };
    let before = cfg.clone();
    let obj = cfg
        .as_object_mut()
        .ok_or("opencode.jsonc is not a JSON object")?;
    // v1-style entries directly under `mcp` count as already configured.
    let legacy: Vec<String> = obj
        .get("mcp")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let servers = obj
        .entry("mcp")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("`mcp` in opencode.jsonc is not an object")?
        .entry("servers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("`mcp.servers` in opencode.jsonc is not an object")?;
    let mut notes = Vec::new();
    if servers.contains_key("a2a") && !force {
        match servers.get_mut("a2a").and_then(|s| repair_command(s, exe)) {
            Some(old) => notes.push(format!(
                "mcp server a2a path repaired ({old} -> {})",
                exe.display()
            )),
            None => {
                notes.push("mcp server a2a already registered (use --force to replace)".to_string())
            }
        }
    } else {
        servers.insert("a2a".into(), entry);
        notes.push(format!("mcp server a2a registered in {}", path.display()));
    }
    let mut added = Vec::new();
    let mut missing_bins = Vec::new();
    for (name, entry) in default_mcp_servers() {
        if servers.contains_key(name) || legacy.iter().any(|k| k == name) {
            continue;
        }
        if let Some(cmd) = entry["command"][0].as_str() {
            if which(cmd).is_none() && !Path::new(cmd).is_file() {
                missing_bins.push(cmd.to_string());
            }
        }
        servers.insert(name.into(), entry);
        added.push(name);
    }
    if !added.is_empty() {
        notes.push(format!("mcp servers added: {}", added.join(", ")));
    }
    if !missing_bins.is_empty() {
        notes.push(format!(
            "not found on PATH (install them to connect): {}",
            missing_bins.join(", ")
        ));
    }
    // The orchestrator is the agent you talk to, so opencode opens on it.
    let prev = obj
        .get("default_agent")
        .and_then(Value::as_str)
        .map(String::from);
    if prev.as_deref() != Some("orchestrator") {
        obj.insert("default_agent".into(), json!("orchestrator"));
        notes.push(match prev {
            Some(p) => format!("default_agent set to orchestrator (was {p})"),
            None => "default_agent set to orchestrator".to_string(),
        });
    }
    let unchanged = cfg == before;
    if !unchanged {
        if path.exists() {
            let bak = root.join("opencode.jsonc.bak-a2a");
            std::fs::copy(&path, &bak)
                .map_err(|e| format!("cannot back up {}: {e}", path.display()))?;
        }
        let out = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
        std::fs::write(&path, out + "\n")
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(notes.join("; "))
}

const ALIAS_BEGIN: &str = "# >>> a2a (codetypera2a) >>>";

/// Append `alias opencode='oc'` to a shell rc file, between markers. `Ok(false)` = already there.
fn append_alias(rc: &Path) -> Result<bool, String> {
    let text = std::fs::read_to_string(rc).unwrap_or_default();
    if text.contains(ALIAS_BEGIN) {
        return Ok(false);
    }
    let block = format!(
        "\n{ALIAS_BEGIN}\n# typing `opencode` runs `oc`: opencode here + the A2A dispatcher and live status\nalias opencode='oc'\n# <<< a2a (codetypera2a) <<<\n"
    );
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(rc)
        .map_err(|e| format!("cannot open {}: {e}", rc.display()))?;
    std::io::Write::write_all(&mut f, block.as_bytes()).map_err(|e| e.to_string())?;
    Ok(true)
}

/// The rc file of the user's login shell (zsh or bash).
fn shell_rc() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let shell = std::env::var("SHELL").unwrap_or_default();
    if shell.ends_with("zsh") {
        let dir = std::env::var_os("ZDOTDIR")
            .map(PathBuf::from)
            .unwrap_or(home);
        Ok(dir.join(".zshrc"))
    } else if shell.ends_with("bash") {
        Ok(home.join(".bashrc"))
    } else {
        Err(format!(
            "unsupported shell {shell:?}; add this yourself: alias opencode='oc'"
        ))
    }
}

/// Put an `oc` symlink next to the binary so `oc` works from the shell.
fn link_oc(exe: &Path) -> String {
    let Some(dir) = exe.parent() else {
        return "oc link skipped".into();
    };
    let link = dir.join("oc");
    if link.exists() || link.symlink_metadata().is_ok() {
        return format!("oc already present at {}", link.display());
    }
    match std::os::unix::fs::symlink(exe, &link) {
        Ok(()) => format!("oc -> {} linked at {}", exe.display(), link.display()),
        Err(e) => format!(
            "could not link oc ({e}); run: ln -s {} <dir-on-PATH>/oc",
            exe.display()
        ),
    }
}

/// `init`: set up the chosen targets (the screen, the flags, or what is detected). The exit code is the worst one.
pub fn init(args: &[String]) -> i32 {
    use crate::targets::{self, Id, Plan, picker};
    use std::io::IsTerminal;
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let probe = targets::Probe::system();
    let ids = match targets::plan(
        &targets::Flags::from_args(args, tty),
        &targets::detected(&probe),
    ) {
        Plan::Run(ids) => ids,
        Plan::Ask => match picker::pick(&probe) {
            Ok(Some(ids)) => ids,
            Ok(None) => {
                println!("init: cancelled, nothing installed");
                return 0;
            }
            Err(e) => {
                eprintln!("init: {e}");
                return 1;
            }
        },
    };
    if ids.is_empty() {
        println!("init: nothing selected, nothing installed");
        return 0;
    }
    type Setup = fn(&[String]) -> i32;
    let runs: [(Id, Setup); 2] = [(Id::Opencode, init_opencode), (Id::Claude, init_claude)];
    runs.iter()
        .filter(|(id, _)| ids.contains(id))
        .map(|(_, run)| run(args))
        .max()
        .unwrap_or(0)
}

/// The Claude Code half of `init`: converted agents and commands, plus the MCP servers.
fn init_claude(args: &[String]) -> i32 {
    use crate::targets::claude;
    let dirs = match claude::dirs_from(args) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("init: claude: {e}");
            return 2;
        }
    };
    let opts = claude::Opts {
        force: args.iter().any(|a| a == "--force"),
        md_only: args.iter().any(|a| a == "--md-only"),
        default_agent: !args.iter().any(|a| a == "--no-default-agent"),
    };
    match claude::run(&dirs, &stable_exe(), &opts, &claude::real_runner(&dirs)) {
        Ok(lines) => lines.iter().for_each(|l| println!("init: claude: {l}")),
        Err(e) => {
            eprintln!("init: claude: {e}");
            return 1;
        }
    }
    let custom_dir =
        args.iter().any(|a| a == "--claude-dir") && std::env::var_os("CLAUDE_CONFIG_DIR").is_none();
    if custom_dir {
        println!(
            "init: claude: Claude Code only reads {} when you start it with CLAUDE_CONFIG_DIR={}",
            dirs.root.display(),
            dirs.root.display()
        );
    }
    let start = if opts.default_agent {
        "Claude Code starts on the orchestrator (`claude --agent <name>` picks another)"
    } else {
        "talk to an agent with `claude --agent orchestrator`"
    };
    println!(
        "init: claude: restart Claude Code; {start}; commands: /plan-new, /plan-continue, /plan-status"
    );
    println!(
        "init: claude: the dispatcher does not wake Claude Code sessions yet, so agents that wait for another agent's result need opencode for now"
    );
    0
}

fn init_opencode(args: &[String]) -> i32 {
    let md_only = args.iter().any(|a| a == "--md-only");
    let force = md_only || args.iter().any(|a| a == "--force");
    let root = match config_dir(args) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("init: {e}");
            return 2;
        }
    };
    let exe = stable_exe();
    // The opencode plugin reads this to find the binary (it may not be on opencode's PATH).
    let path_file = root.join("a2a").join("codetypera2a.path");
    if let Err(e) = std::fs::create_dir_all(root.join("a2a")).and_then(|_| {
        (!md_only)
            .then(|| std::fs::write(&path_file, exe.display().to_string()))
            .unwrap_or(Ok(()))
    }) {
        eprintln!("init: cannot write {}: {e}", path_file.display());
    }
    let mut items: Vec<(PathBuf, &[u8])> = AGENTS
        .files()
        .filter_map(|f| {
            Some((
                root.join("agents").join(f.path().file_name()?),
                f.contents(),
            ))
        })
        .collect();
    items.extend(COMMANDS.files().filter_map(|f| {
        Some((
            root.join("commands").join(f.path().file_name()?),
            f.contents(),
        ))
    }));
    let mut plugin_files = Vec::new();
    embedded_files(&PLUGINS, &mut plugin_files);
    plugin_files.retain(|(rel, _)| !rel.starts_with(CLAUDE_MOD_DIR));
    items.extend(
        plugin_files
            .into_iter()
            .map(|(rel, bytes)| (root.join("plugins").join(rel), bytes)),
    );
    items.push((
        root.join("a2a").join("model-policy.json"),
        POLICY.as_bytes(),
    ));
    items.push((root.join("a2a").join("PROTOCOL.md"), PROTOCOL.as_bytes()));
    if md_only {
        let docs = [root.join("agents"), root.join("commands")];
        items.retain(|(t, _)| {
            t.extension().is_some_and(|e| e == "md")
                && (docs.iter().any(|d| t.starts_with(d)) || t.ends_with("PROTOCOL.md"))
        });
    }
    let (mut written, mut skipped) = (0, 0);
    for (target, contents) in items {
        match install(&target, contents, force) {
            Ok(true) => written += 1,
            Ok(false) => skipped += 1,
            Err(e) => {
                eprintln!("init: {e}");
                return 1;
            }
        }
    }
    println!(
        "init: {written} file(s) installed, {skipped} skipped (exist; use --force) -> {}",
        root.display()
    );
    if md_only {
        // the files carry the template models again: put back the ones saved in opencode.jsonc
        let kept = crate::models::load_config(&root).map_or(0, |(cfg, _)| {
            crate::models::sync_agent_files(&root, &crate::models::current_models(&cfg))
        });
        println!(
            "init: --md-only: only markdown refreshed; {kept} agent file(s) set back to the models saved in opencode.jsonc"
        );
        return 0;
    }
    if args.iter().any(|a| a == "--tools") {
        run_tool_setups();
    }
    match register_mcp(&root, &exe, force) {
        Ok(msg) => println!("init: {msg}"),
        Err(e) => {
            eprintln!("init: {e}");
            return 1;
        }
    }
    crate::ollama::setup(&root, args)
        .iter()
        .for_each(|l| println!("init: {l}"));
    if !args.iter().any(|a| a == "--no-link") {
        println!("init: {}", link_oc(&exe));
    }
    // --force put the template models back into the agent files: reapply the ones chosen before
    if let Ok((cfg, _)) = crate::models::load_config(&root) {
        let n = crate::models::sync_agent_files(&root, &crate::models::current_models(&cfg));
        if n > 0 {
            println!("init: {n} agent file(s) set to the models saved in opencode.jsonc");
        }
    }
    if !args.iter().any(|a| a == "--no-models") {
        if let Some(note) = crate::models::availability_note(&root) {
            println!("init: note: {note}");
        }
        // choose each agent's model from what opencode has (a terminal screen; skipped without a terminal)
        if let Err(e) = crate::models::configure(&root, false, false, true) {
            eprintln!("init: models: {e}");
        }
    }
    for step in tool_next_steps() {
        println!("init: next: {step}");
    }
    if args.iter().any(|a| a == "--shell") {
        match shell_rc().and_then(|rc| append_alias(&rc).map(|new| (rc, new))) {
            Ok((rc, true)) => println!(
                "init: alias opencode='oc' added to {} (open a new terminal or: source {})",
                rc.display(),
                rc.display()
            ),
            Ok((rc, false)) => println!("init: alias already present in {}", rc.display()),
            Err(e) => eprintln!("init: {e}"),
        }
    } else {
        println!(
            "init: tip: run `codetypera2a init --shell` so typing `opencode` runs `oc` (optional: the opencode plugin already starts the dispatcher and, in tmux, the status tab)"
        );
    }
    println!(
        "init: restart opencode (or run `opencode reload`); it opens on the orchestrator and the a2a MCP server starts the dispatcher by itself. `oc` is optional."
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("a2a-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a2a")).unwrap();
        dir
    }

    fn cmd0(dir: &Path) -> String {
        let cfg: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("opencode.jsonc")).unwrap())
                .unwrap();
        cfg["mcp"]["servers"]["a2a"]["command"][0]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn a2a_cfg(cmd: &str) -> String {
        format!(
            r#"{{"mcp":{{"servers":{{"a2a":{{"type":"local","command":["{cmd}","hub"],"codemode":false,"x":1}}}}}}}}"#
        )
    }

    #[test]
    fn heal_repairs_stale_path_and_keeps_keys() {
        let dir = tmp("heal-stale");
        let new = std::env::current_exe().unwrap();
        std::fs::write(dir.join("opencode.jsonc"), a2a_cfg("/gone/codetypera2a")).unwrap();
        let msg = heal(&dir, &new).unwrap();
        assert!(
            msg.contains(&format!(
                "path repaired (/gone/codetypera2a -> {})",
                new.display()
            )),
            "{msg}"
        );
        assert_eq!(cmd0(&dir), new.display().to_string());
        let cfg: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("opencode.jsonc")).unwrap())
                .unwrap();
        assert_eq!(cfg["mcp"]["servers"]["a2a"]["x"], 1);
        assert_eq!(cfg["mcp"]["servers"]["a2a"]["codemode"], false);
        assert!(dir.join("opencode.jsonc.bak-a2a").exists());
        assert!(heal(&dir, &new).is_none(), "second run must be a no-op");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn heal_leaves_valid_and_bare_commands() {
        let dir = tmp("heal-valid");
        let real = std::env::current_exe().unwrap();
        for cmd in [real.display().to_string(), "codetypera2a".to_string()] {
            std::fs::write(dir.join("opencode.jsonc"), a2a_cfg(&cmd)).unwrap();
            assert!(heal(&dir, Path::new("/new/codetypera2a")).is_none());
            assert_eq!(cmd0(&dir), cmd);
        }
        assert!(!dir.join("opencode.jsonc.bak-a2a").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn heal_hints_on_commented_jsonc_without_rewriting() {
        let dir = tmp("heal-jsonc");
        let text = "{\n // mine\n \"mcp\": {\"servers\": {\"a2a\": {\"command\": [\"/gone/codetypera2a\", \"hub\"]}}}\n}";
        std::fs::write(dir.join("opencode.jsonc"), text).unwrap();
        let msg = heal(&dir, Path::new("/new/codetypera2a")).unwrap();
        assert!(
            msg.contains("/new/codetypera2a") && !msg.contains('\n'),
            "{msg}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("opencode.jsonc")).unwrap(),
            text
        );
        assert!(!dir.join("opencode.jsonc.bak-a2a").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn heal_rewrites_path_file_only_when_stale() {
        let dir = tmp("heal-path");
        let pf = dir.join("a2a").join("codetypera2a.path");
        let real = std::env::current_exe().unwrap();
        std::fs::write(&pf, real.display().to_string()).unwrap();
        assert!(heal(&dir, Path::new("/new/codetypera2a")).is_none());
        assert_eq!(
            std::fs::read_to_string(&pf).unwrap(),
            real.display().to_string()
        );
        std::fs::write(&pf, "/gone/codetypera2a").unwrap();
        assert!(
            heal(&dir, Path::new("/new/codetypera2a"))
                .unwrap()
                .contains("codetypera2a.path repaired")
        );
        assert_eq!(std::fs::read_to_string(&pf).unwrap(), "/new/codetypera2a");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn register_mcp_repairs_stale_without_force() {
        let dir = tmp("reg-stale");
        let new = std::env::current_exe().unwrap();
        std::fs::write(dir.join("opencode.jsonc"), a2a_cfg("/gone/codetypera2a")).unwrap();
        let note = register_mcp(&dir, &new, false).unwrap();
        assert!(
            note.contains(&format!(
                "path repaired (/gone/codetypera2a -> {})",
                new.display()
            )),
            "{note}"
        );
        assert_eq!(cmd0(&dir), new.display().to_string());
        let note = register_mcp(&dir, Path::new("/other/codetypera2a"), false).unwrap();
        assert!(note.contains("already registered"), "{note}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_mcp_servers_are_kept() {
        let dir = std::env::temp_dir().join(format!("a2a-mcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("opencode.jsonc"),
            r#"{"mcp":{"servers":{"engram":{"type":"local","command":["/my/engram","mcp"]}},"playwright":{"type":"local","command":["pw"]}}}"#,
        )
        .unwrap();
        register_mcp(&dir, Path::new("/x/codetypera2a"), false).unwrap();
        let cfg: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("opencode.jsonc")).unwrap())
                .unwrap();
        assert_eq!(cfg["mcp"]["servers"]["engram"]["command"][0], "/my/engram");
        assert!(
            cfg["mcp"]["servers"]["playwright"].is_null(),
            "legacy top-level entry must count as present"
        );
        assert!(cfg["mcp"]["servers"]["codegraph"].is_object());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn alias_is_appended_once() {
        let rc = std::env::temp_dir().join(format!("a2a-rc-{}", std::process::id()));
        std::fs::write(&rc, "export A=1\n").unwrap();
        assert!(append_alias(&rc).unwrap());
        assert!(!append_alias(&rc).unwrap());
        let t = std::fs::read_to_string(&rc).unwrap();
        assert!(t.starts_with("export A=1") && t.matches("alias opencode='oc'").count() == 1);
        let _ = std::fs::remove_file(&rc);
    }

    #[test]
    fn init_installs_agents_and_registers_mcp() {
        let dir = std::env::temp_dir().join(format!("a2a-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("opencode.jsonc"),
            r#"{"model":"x/y","mcp":{"servers":{"other":{"type":"local","command":["o"]}}}}"#,
        )
        .unwrap();
        let a = vec![
            "--opencode".to_string(),
            "--dir".to_string(),
            dir.display().to_string(),
            "--no-link".to_string(),
            "--no-models".to_string(),
        ];
        assert_eq!(init(&a), 0);
        let f = dir.join("agents/orchestrator.md");
        assert!(f.exists());
        assert!(dir.join("a2a/model-policy.json").exists());
        assert!(dir.join("a2a/PROTOCOL.md").exists());
        assert!(dir.join("commands/plan-new.md").exists());
        assert!(dir.join("plugins/a2a/tui.ts").exists());
        assert!(
            dir.join("plugins/a2a/panels.tsx").exists()
                && dir.join("plugins/a2a/panel-logic.ts").exists()
        );
        assert!(dir.join("a2a/codetypera2a.path").exists());
        let cfg: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("opencode.jsonc")).unwrap())
                .unwrap();
        assert_eq!(cfg["model"], "x/y");
        assert_eq!(cfg["mcp"]["servers"]["other"]["command"][0], "o");
        assert_eq!(cfg["mcp"]["servers"]["a2a"]["command"][1], "hub");
        assert_eq!(cfg["mcp"]["servers"]["a2a"]["codemode"], false);
        assert_eq!(cfg["default_agent"], "orchestrator");
        for name in ["codegraph", "context7", "engram", "playwright"] {
            assert!(cfg["mcp"]["servers"][name].is_object(), "{name} missing");
        }
        assert_eq!(cfg["mcp"]["servers"]["context7"]["type"], "remote");
        assert!(dir.join("opencode.jsonc.bak-a2a").exists());
        // second run keeps user edits unless forced
        std::fs::write(&f, "mine").unwrap();
        assert_eq!(init(&a), 0);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "mine");
        let mut forced = a.clone();
        forced.push("--force".into());
        assert_eq!(init(&forced), 0);
        assert_ne!(std::fs::read_to_string(&f).unwrap(), "mine");
        // --md-only refreshes the markdown and leaves opencode.jsonc alone
        let jsonc = dir.join("opencode.jsonc");
        let before = std::fs::read_to_string(&jsonc).unwrap();
        std::fs::write(&f, "mine").unwrap();
        let mut md = a.clone();
        md.push("--md-only".into());
        assert_eq!(init(&md), 0);
        assert_ne!(std::fs::read_to_string(&f).unwrap(), "mine");
        assert_eq!(std::fs::read_to_string(&jsonc).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_sets_up_claude_code_in_its_own_dir_and_leaves_opencode_alone() {
        let dir = std::env::temp_dir().join(format!("a2a-init-claude-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (claude_dir, opencode_dir) = (dir.join("claude"), dir.join("opencode"));
        let a = vec![
            "--claude".to_string(),
            "--claude-dir".to_string(),
            claude_dir.display().to_string(),
            "--dir".to_string(),
            opencode_dir.display().to_string(),
        ];
        assert_eq!(init(&a), 0);
        assert!(claude_dir.join("agents/orchestrator.md").exists());
        assert!(claude_dir.join("commands/plan-new.md").exists());
        let cfg: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(claude_dir.join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(cfg["mcpServers"]["a2a"]["args"][0], "hub");
        assert_eq!(cfg["mcpServers"]["a2a"]["type"], "stdio");
        for name in ["codegraph", "context7", "engram", "playwright"] {
            assert!(cfg["mcpServers"][name].is_object(), "{name} missing");
        }
        assert!(
            !opencode_dir.exists(),
            "--claude must not touch the opencode dir"
        );
        // a second run keeps what is there
        let before = std::fs::read_to_string(claude_dir.join(".claude.json")).unwrap();
        assert_eq!(init(&a), 0);
        assert_eq!(
            std::fs::read_to_string(claude_dir.join(".claude.json")).unwrap(),
            before
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
