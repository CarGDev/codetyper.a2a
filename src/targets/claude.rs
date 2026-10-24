//! The Claude Code target: the agents and commands `init` embeds, converted to Claude Code's format
//! (see `convert`), and the MCP servers in the user-scope config. Nothing here touches opencode.
use super::convert;
use crate::cli::{AGENTS, CLAUDE_MOD, COMMANDS, default_mcp_servers, install, which};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Where Claude Code keeps its files: the config dir (agents, commands) and the JSON that holds the
/// user-scope MCP servers. That file is `<dir>/.claude.json` when `CLAUDE_CONFIG_DIR` is set and
/// `~/.claude.json` otherwise (checked against `claude mcp add --scope user`).
#[derive(Debug, PartialEq)]
pub struct Dirs {
    pub root: PathBuf,
    pub mcp_file: PathBuf,
}

pub struct Opts {
    pub force: bool,
    /// `update`: refresh the markdown only, leave the MCP config alone.
    pub md_only: bool,
}

/// `--claude-dir` beats `CLAUDE_CONFIG_DIR`, which beats `~/.claude`.
pub fn resolve_dirs(flag: Option<PathBuf>, env: Option<PathBuf>, home: &Path) -> Dirs {
    let default = home.join(".claude");
    let env_set = env.is_some();
    let root = flag.or(env).unwrap_or_else(|| default.clone());
    let mcp_file = if root == default && !env_set { home.join(".claude.json") } else { root.join(".claude.json") };
    Dirs { root, mcp_file }
}

pub fn dirs_from(args: &[String]) -> Result<Dirs, String> {
    let flag = match args.iter().position(|a| a == "--claude-dir") {
        Some(i) => Some(args.get(i + 1).map(PathBuf::from).ok_or("--claude-dir needs a path")?),
        None => None,
    };
    let env = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()).map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is not set")?;
    Ok(resolve_dirs(flag, env, &home))
}

/// An opencode MCP entry (`type: local|remote`) as a Claude Code one (`type: stdio|http`).
fn claude_server(entry: &Value) -> Value {
    let remote = entry["type"] == "remote";
    if remote {
        return json!({ "type": "http", "url": entry["url"] });
    }
    let cmd: Vec<&str> = entry["command"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    json!({ "type": "stdio", "command": cmd.first().copied().unwrap_or_default(), "args": cmd.get(1..).unwrap_or_default(), "env": {} })
}

fn a2a_server(exe: &Path) -> Value {
    json!({ "type": "stdio", "command": exe.display().to_string(), "args": ["hub"], "env": {} })
}

/// If the entry's `command` is a path to a file that is gone, point it at the current binary
/// (other keys kept). Returns the old path when it repaired something.
fn repair(entry: &mut Value, exe: &Path) -> Option<String> {
    let old = entry.get("command")?.as_str()?.to_string();
    if !old.contains('/') || Path::new(&old).is_file() || Path::new(&old) == exe {
        return None;
    }
    entry["command"] = json!(exe.display().to_string());
    entry["args"] = json!(["hub"]);
    Some(old)
}

/// Register `a2a` and the companion servers under `mcpServers`, never replacing an entry that exists
/// (`force` replaces `a2a`; a dead binary path is repaired). The file is backed up before its first change.
pub fn register_mcp(file: &Path, exe: &Path, force: bool) -> Result<String, String> {
    let mut cfg: Value = match std::fs::read_to_string(file) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{} is not valid JSON ({e}); left untouched, add the servers with `claude mcp add`", file.display()))?,
        Err(_) => json!({}),
    };
    let before = cfg.clone();
    let servers = cfg
        .as_object_mut()
        .ok_or_else(|| format!("{} is not a JSON object", file.display()))?
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("`mcpServers` in {} is not an object", file.display()))?;
    let mut notes = Vec::new();
    match servers.get_mut("a2a") {
        Some(existing) if !force => match repair(existing, exe) {
            Some(old) => notes.push(format!("mcp server a2a path repaired ({old} -> {})", exe.display())),
            None => notes.push("mcp server a2a already registered (use --force to replace)".to_string()),
        },
        _ => {
            servers.insert("a2a".into(), a2a_server(exe));
            notes.push(format!("mcp server a2a registered in {}", file.display()));
        }
    }
    let (mut added, mut missing) = (Vec::new(), Vec::new());
    let absent: Vec<_> = default_mcp_servers().into_iter().filter(|(n, _)| !servers.contains_key(*n)).collect();
    for (name, entry) in absent {
        let server = claude_server(&entry);
        let cmd = server["command"].as_str().unwrap_or_default();
        if !cmd.is_empty() && which(cmd).is_none() && !Path::new(cmd).is_file() {
            missing.push(cmd.to_string());
        }
        servers.insert(name.into(), server);
        added.push(name);
    }
    if !added.is_empty() {
        notes.push(format!("mcp servers added: {}", added.join(", ")));
    }
    if !missing.is_empty() {
        notes.push(format!("not found on PATH (install them to connect): {}", missing.join(", ")));
    }
    if cfg == before {
        return Ok(notes.join("; "));
    }
    if file.exists() {
        let mut bak = file.as_os_str().to_owned();
        bak.push(".bak-a2a");
        std::fs::copy(file, &bak).map_err(|e| format!("cannot back up {}: {e}", file.display()))?;
    }
    let out = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    std::fs::write(file, out + "\n").map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    Ok(notes.join("; "))
}

/// FNV-1a 64: a small stable content hash (it only has to tell "unchanged" from "edited").
fn hash(bytes: &[u8]) -> String {
    let h = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3));
    format!("{h:016x}")
}

const MANIFEST: &str = ".a2a-manifest.json";

/// What `init` last wrote, per file relative to the Claude dir (`path -> content hash`). A file whose
/// current hash is the recorded one was never edited by the user, so a newer release may replace it.
#[derive(Default)]
pub struct Manifest {
    files: std::collections::BTreeMap<String, String>,
}

impl Manifest {
    /// A missing or unreadable manifest is an empty one: every differing file then counts as edited.
    pub fn load(root: &Path) -> Manifest {
        let text = std::fs::read_to_string(root.join(MANIFEST)).ok();
        let files = text.and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| serde_json::from_value(v["files"].clone()).ok());
        Manifest { files: files.unwrap_or_default() }
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&json!({ "files": self.files })).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
        std::fs::write(root.join(MANIFEST), text + "\n").map_err(|e| format!("cannot write {MANIFEST}: {e}"))
    }
}

/// What happened to the files of one install step.
#[derive(Default, Debug, PartialEq)]
pub struct Synced {
    pub written: usize,
    pub current: usize,
    /// Edited by the user (or unknown) and different from the new content: left alone.
    pub kept: Vec<String>,
}

impl Synced {
    fn add(&mut self, other: Synced) {
        self.written += other.written;
        self.current += other.current;
        self.kept.extend(other.kept);
    }
}

/// Write `contents` to `root/rel` when the file is missing, already equal, still as we wrote it, or
/// `force`d; otherwise keep it.
fn sync_file(root: &Path, rel: &str, contents: &[u8], manifest: &mut Manifest, force: bool) -> Result<Synced, String> {
    let target = root.join(rel);
    let new = hash(contents);
    let existing = std::fs::read(&target).ok().map(|b| hash(&b));
    let mut out = Synced::default();
    if existing.as_deref() == Some(new.as_str()) {
        manifest.files.insert(rel.to_string(), new);
        out.current = 1;
        return Ok(out);
    }
    let known = manifest.files.get(rel);
    let ours = existing.is_none() || known == existing.as_ref();
    // installed before the manifest existed: replace it, keeping the old copy next to it
    let legacy = !ours && known.is_none();
    if !ours && !legacy && !force {
        out.kept.push(rel.to_string());
        return Ok(out);
    }
    if legacy {
        std::fs::copy(&target, target.with_extension("md.bak-a2a")).map_err(|e| format!("{rel}: backup failed: {e}"))?;
    }
    install(&target, contents, true)?;
    manifest.files.insert(rel.to_string(), new);
    out.written = 1;
    Ok(out)
}

fn sync_all(root: &Path, items: Vec<(String, Vec<u8>)>, manifest: &mut Manifest, force: bool) -> Result<Synced, String> {
    let mut total = Synced::default();
    for (rel, contents) in items {
        total.add(sync_file(root, &rel, &contents, manifest, force)?);
    }
    Ok(total)
}

/// Convert and write every embedded agent and command.
fn install_markdown(root: &Path, manifest: &mut Manifest, force: bool) -> Result<Synced, String> {
    let mut items: Vec<(String, Vec<u8>)> = Vec::new();
    for file in AGENTS.files() {
        let name = file.path().file_stem().and_then(|s| s.to_str()).ok_or("agent file without a name")?;
        let text = file.contents_utf8().ok_or_else(|| format!("{name}: not UTF-8"))?;
        items.push((format!("agents/{name}.md"), convert::agent(name, text).map_err(|e| format!("{name}: {e}"))?.into_bytes()));
    }
    for file in COMMANDS.files() {
        let name = file.path().file_name().and_then(|n| n.to_str()).ok_or("command file without a name")?;
        let text = file.contents_utf8().ok_or_else(|| format!("{name}: not UTF-8"))?;
        items.push((format!("commands/{name}"), convert::command(text).map_err(|e| format!("{name}: {e}"))?.into_bytes()));
    }
    sync_all(root, items, manifest, force)
}

/// Marketplace name and plugin name the pane is registered under (`claude plugin install a2a-pane@codetypera2a`).
const MARKETPLACE: &str = "codetypera2a";
const MOD_NAME: &str = "a2a-pane";

/// A local marketplace folder inside the Claude config dir that holds the pane mod; Claude Code reads a
/// plugin whose marketplace is a folder from that folder itself (`claude plugin marketplace add <dir>`).
pub fn marketplace_dir(dirs: &Dirs) -> PathBuf {
    dirs.root.join("a2a-marketplace")
}

/// The install arguments with the binary path the mod calls (`userConfig.exe`); also what `init` prints
/// when it could not run the step itself.
pub fn install_command(exe: &Path) -> String {
    format!("{MOD_NAME}@{MARKETPLACE} --scope user --config exe='{}'", exe.display())
}

/// What a `claude` child printed and whether it exited 0.
pub struct Ran {
    pub ok: bool,
    pub out: String,
    pub err: String,
}

/// Runs `claude <args>`; injected so tests never start the real binary.
pub type Runner<'a> = &'a dyn Fn(&[String]) -> Result<Ran, String>;

/// The real runner: `CLAUDE_BIN` (default `claude`), with `CLAUDE_CONFIG_DIR` pointed at the dir `init`
/// writes to unless that is the default `~/.claude` (which Claude Code finds on its own).
pub fn real_runner(dirs: &Dirs) -> impl Fn(&[String]) -> Result<Ran, String> {
    let bin = std::env::var("CLAUDE_BIN").ok().filter(|b| !b.is_empty()).unwrap_or_else(|| "claude".into());
    let scoped = dirs.mcp_file.parent() == Some(dirs.root.as_path());
    let root = dirs.root.clone();
    move |args| {
        let mut cmd = std::process::Command::new(&bin);
        cmd.args(args).stdin(std::process::Stdio::null());
        if scoped {
            cmd.env("CLAUDE_CONFIG_DIR", &root);
        }
        let out = cmd.output().map_err(|e| format!("cannot run {bin}: {e}"))?;
        Ok(Ran { ok: out.status.success(), out: String::from_utf8_lossy(&out.stdout).into_owned(), err: String::from_utf8_lossy(&out.stderr).into_owned() })
    }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

fn call(run: Runner, args: &[&str]) -> Result<String, String> {
    let ran = run(&strings(args))?;
    if ran.ok {
        return Ok(ran.out);
    }
    let why = [&ran.err, &ran.out].into_iter().flat_map(|t| t.lines()).find(|l| !l.trim().is_empty()).unwrap_or("no output");
    Err(format!("`claude {}` failed: {why}", args.join(" ")))
}

fn json_of(run: Runner, args: &[&str]) -> Result<Value, String> {
    let out = call(run, args)?;
    serde_json::from_str(out.trim()).map_err(|e| format!("`claude {}` printed no JSON ({e})", args.join(" ")))
}

/// The version `plugin.json` declares, in the file on disk or in the embedded copy.
fn plugin_version(text: &str) -> Option<String> {
    serde_json::from_str::<Value>(text).ok()?["version"].as_str().map(String::from)
}

const PLUGIN_JSON: &str = ".claude-plugin/plugin.json";

fn installed_version(dirs: &Dirs) -> Option<String> {
    plugin_version(&std::fs::read_to_string(marketplace_dir(dirs).join(MOD_NAME).join(PLUGIN_JSON)).ok()?)
}

fn embedded_version() -> Option<String> {
    plugin_version(CLAUDE_MOD.get_file(PLUGIN_JSON)?.contents_utf8()?)
}

fn setup_steps(dirs: &Dirs, exe: &Path, run: Runner) -> Result<Vec<String>, String> {
    let market = marketplace_dir(dirs).display().to_string();
    let id = format!("{MOD_NAME}@{MARKETPLACE}");
    let mut lines = Vec::new();
    let markets = json_of(run, &["plugin", "marketplace", "list", "--json"])?;
    if !markets.as_array().into_iter().flatten().any(|m| m["name"] == MARKETPLACE) {
        call(run, &["plugin", "marketplace", "add", &market])?;
        lines.push(format!("pane: marketplace {MARKETPLACE} added ({market})"));
    }
    let plugins = json_of(run, &["plugin", "list", "--json"])?;
    let current = plugins.as_array().into_iter().flatten().find(|p| p["id"] == id.as_str());
    let want = installed_version(dirs);
    let installed = current.and_then(|p| p["version"].as_str().map(String::from));
    let Some(have) = installed else {
        let config = format!("exe={}", exe.display());
        call(run, &["plugin", "install", &id, "--scope", "user", "--config", &config])?;
        lines.push(format!("pane: {id} installed and enabled (restart Claude Code to see it)"));
        return Ok(lines);
    };
    if want.is_none_or(|w| w == have) {
        lines.push(format!("pane: {id} already installed ({have})"));
        return Ok(lines);
    }
    call(run, &["plugin", "marketplace", "update", MARKETPLACE])?;
    call(run, &["plugin", "update", &id, "--scope", "user"])?;
    lines.push(format!("pane: {id} updated from {have} (restart Claude Code to apply)"));
    Ok(lines)
}

/// Add the marketplace and install the pane through `claude plugin ...`, each step only when it is not
/// already done. A failure is a line for the user with the manual commands, never an error: the rest of
/// `init` stands without it.
pub fn setup_plugin(dirs: &Dirs, exe: &Path, run: Runner) -> Vec<String> {
    setup_steps(dirs, exe, run).unwrap_or_else(|why| {
        vec![format!("pane NOT enabled: {why}; run `claude plugin marketplace add {}` then `claude plugin install {}`", marketplace_dir(dirs).display(), install_command(exe))]
    })
}

fn marketplace_json() -> String {
    let doc = json!({
        "name": MARKETPLACE,
        "owner": { "name": "codetypera2a" },
        "plugins": [{ "name": MOD_NAME, "source": format!("./{MOD_NAME}") }],
    });
    serde_json::to_string_pretty(&doc).unwrap_or_default() + "\n"
}

/// Write the embedded pane mod (tests excluded) and its marketplace file under `a2a-marketplace/`,
/// by the same rules as `install_markdown`.
pub fn install_pane(dirs: &Dirs, manifest: &mut Manifest, force: bool) -> Result<Synced, String> {
    let base = PathBuf::from("a2a-marketplace");
    let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    collect(&CLAUDE_MOD, &base.join(MOD_NAME), &mut files);
    files.push((base.join(".claude-plugin/marketplace.json"), marketplace_json().into_bytes()));
    let items = files.into_iter().map(|(rel, bytes)| (rel.display().to_string(), bytes)).collect();
    sync_all(&dirs.root, items, manifest, force)
}

fn collect(dir: &include_dir::Dir, to: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
    let is_test = |p: &Path| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains(".test."));
    out.extend(dir.files().filter(|f| !is_test(f.path())).map(|f| (to.join(f.path().file_name().unwrap_or_default()), f.contents().to_vec())));
    for sub in dir.dirs() {
        collect(sub, &to.join(sub.path().file_name().unwrap_or_default()), out);
    }
}

fn sync_line(what: &str, s: &Synced, place: &Path) -> String {
    format!("{what}: {} written, {} up to date, {} kept -> {}", s.written, s.current, s.kept.len(), place.display())
}

/// The whole Claude Code install; the returned lines are for the user. Files we wrote before are
/// refreshed when a release changes them; files the user edited are kept (`force` replaces them).
pub fn run(dirs: &Dirs, exe: &Path, opts: &Opts, claude: Runner) -> Result<Vec<String>, String> {
    let mut manifest = Manifest::load(&dirs.root);
    let md = install_markdown(&dirs.root, &mut manifest, opts.force)?;
    let mut lines = vec![sync_line("agents and commands", &md, &dirs.root)];
    let mut kept = md.kept;
    if opts.md_only {
        manifest.save(&dirs.root)?;
        lines.push("--md-only: only markdown refreshed".into());
        lines.extend(kept_line(&kept));
        return Ok(lines);
    }
    let pane = install_pane(dirs, &mut manifest, opts.force)?;
    lines.push(sync_line("a2a pane mod", &pane, &marketplace_dir(dirs)));
    kept.extend(pane.kept);
    manifest.save(&dirs.root)?;
    lines.extend(kept_line(&kept));
    lines.push(register_mcp(&dirs.mcp_file, exe, opts.force)?);
    lines.extend(super::settings::install(&dirs.root.join("settings.json"), exe)?);
    lines.extend(setup_plugin(dirs, exe, claude));
    Ok(lines)
}

fn kept_line(kept: &[String]) -> Option<String> {
    (!kept.is_empty()).then(|| format!("kept {} file(s) that differ from this release (edited, or installed before updates were tracked); `init --claude --force` replaces them: {}", kept.len(), kept.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("a2a-claude-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read(p: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn the_pane_is_installed_once_without_tests_and_edits_survive() {
        let dir = tmp("pane");
        let dirs = Dirs { root: dir.clone(), mcp_file: dir.join(".claude.json") };
        let mut m = Manifest::default();
        let first = install_pane(&dirs, &mut m, false).unwrap();
        assert!(first.written >= 5 && first.current == 0 && first.kept.is_empty(), "{first:?}");
        let plugin = dir.join("a2a-marketplace/a2a-pane");
        assert!(plugin.join(".claude-plugin/plugin.json").exists() && plugin.join("hooks/register.tsx").exists());
        assert!(!plugin.join("a2a-pane.test.tsx").exists(), "tests are not shipped");
        let market = read(&dir.join("a2a-marketplace/.claude-plugin/marketplace.json"));
        assert_eq!(market["plugins"][0]["source"], "./a2a-pane");
        assert_eq!(install_pane(&dirs, &mut m, false).unwrap().written, 0, "an up-to-date install writes nothing");
        std::fs::write(plugin.join("hooks/register.tsx"), "mine").unwrap();
        let again = install_pane(&dirs, &mut m, false).unwrap();
        assert_eq!((again.written, again.kept.len()), (0, 1), "{again:?}");
        assert_eq!(std::fs::read_to_string(plugin.join("hooks/register.tsx")).unwrap(), "mine");
        assert_eq!(install_pane(&dirs, &mut m, true).unwrap().written, 1, "--force restores");
        assert_ne!(std::fs::read_to_string(plugin.join("hooks/register.tsx")).unwrap(), "mine");
        assert_eq!(install_command(Path::new("/x/oc")), "a2a-pane@codetypera2a --scope user --config exe='/x/oc'");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_we_wrote_are_refreshed_edited_or_unknown_ones_are_kept() {
        let dir = tmp("sync");
        let mut m = Manifest::default();
        let w = |m: &mut Manifest, rel: &str, c: &str, force: bool| sync_file(&dir, rel, c.as_bytes(), m, force).unwrap();
        assert_eq!(w(&mut m, "a.md", "v1", false).written, 1);
        assert_eq!(w(&mut m, "a.md", "v1", false).current, 1);
        assert_eq!(w(&mut m, "a.md", "v2", false).written, 1, "unedited since we wrote it: the new release replaces it");
        std::fs::write(dir.join("a.md"), "mine").unwrap();
        assert_eq!(w(&mut m, "a.md", "v3", false).kept, ["a.md"], "edited: kept");
        assert_eq!(std::fs::read_to_string(dir.join("a.md")).unwrap(), "mine");
        std::fs::write(dir.join("old.md"), "from before the manifest").unwrap();
        assert_eq!(w(&mut m, "old.md", "v1", false).written, 1, "no manifest entry and different: replaced");
        assert_eq!(std::fs::read_to_string(dir.join("old.md.bak-a2a")).unwrap(), "from before the manifest", "the old copy is kept");
        m.save(&dir).unwrap();
        let again = Manifest::load(&dir);
        assert_eq!(w(&mut { again }, "old.md", "v1", false).current, 1, "the manifest survives a save/load");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn no_claude(_: &[String]) -> Result<Ran, String> {
        Err("cannot run claude: not installed".into())
    }

    /// A scripted `claude`: records every call and answers the two list commands from the given state.
    struct Fake {
        calls: std::cell::RefCell<Vec<String>>,
        markets: &'static str,
        plugins: std::cell::RefCell<String>,
        fail_on: Option<&'static str>,
    }

    impl Fake {
        fn new(markets: &'static str, plugins: &str) -> Fake {
            Fake { calls: Default::default(), markets, plugins: plugins.to_string().into(), fail_on: None }
        }

        fn run(&self, args: &[String]) -> Result<Ran, String> {
            let line = args.join(" ");
            self.calls.borrow_mut().push(line.clone());
            let ran = |ok: bool, out: &str, err: &str| Ok(Ran { ok, out: out.into(), err: err.into() });
            if self.fail_on.is_some_and(|f| line.starts_with(f)) {
                return ran(false, "", "boom\nsecond line");
            }
            match line.as_str() {
                "plugin marketplace list --json" => ran(true, self.markets, ""),
                "plugin list --json" => ran(true, &self.plugins.borrow(), ""),
                _ => ran(true, "ok", ""),
            }
        }

        fn mutations(&self) -> Vec<String> {
            self.calls.borrow().iter().filter(|c| !c.ends_with("list --json")).cloned().collect()
        }
    }

    fn pane_dirs(name: &str) -> Dirs {
        let dir = tmp(name);
        let dirs = Dirs { root: dir.clone(), mcp_file: dir.join(".claude.json") };
        install_pane(&dirs, &mut Manifest::default(), false).unwrap();
        dirs
    }

    #[test]
    fn a_fresh_config_gets_the_marketplace_added_and_the_pane_installed() {
        let dirs = pane_dirs("setup-fresh");
        let fake = Fake::new("[]", "[]");
        let lines = setup_plugin(&dirs, Path::new("/x/my bin/codetypera2a"), &|a| fake.run(a));
        let market = marketplace_dir(&dirs).display().to_string();
        assert_eq!(
            fake.mutations(),
            [format!("plugin marketplace add {market}"), "plugin install a2a-pane@codetypera2a --scope user --config exe=/x/my bin/codetypera2a".to_string()]
        );
        assert!(lines.len() == 2 && lines[0].contains("added") && lines[1].contains("installed"), "{lines:?}");
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn a_second_run_only_reads_state() {
        let dirs = pane_dirs("setup-again");
        let version = installed_version(&dirs).unwrap();
        let plugins = format!(r#"[{{"id":"a2a-pane@codetypera2a","version":"{version}","enabled":true}}]"#);
        let fake = Fake::new(r#"[{"name":"codetypera2a"}]"#, &plugins);
        let lines = setup_plugin(&dirs, Path::new("/x/a"), &|a| fake.run(a));
        assert!(fake.mutations().is_empty(), "{:?}", fake.calls);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("already installed"), "{lines:?}");
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn a_changed_plugin_version_updates_instead_of_reinstalling() {
        let dirs = pane_dirs("setup-update");
        let fake = Fake::new(r#"[{"name":"codetypera2a"}]"#, r#"[{"id":"a2a-pane@codetypera2a","version":"0.0.1"}]"#);
        let lines = setup_plugin(&dirs, Path::new("/x/a"), &|a| fake.run(a));
        assert_eq!(fake.mutations(), ["plugin marketplace update codetypera2a", "plugin update a2a-pane@codetypera2a --scope user"]);
        assert!(lines[0].contains("updated from 0.0.1"), "{lines:?}");
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn a_failing_step_is_a_line_with_the_manual_commands_not_an_error() {
        let dirs = pane_dirs("setup-fail");
        let mut fake = Fake::new("[]", "[]");
        fake.fail_on = Some("plugin install");
        let lines = setup_plugin(&dirs, Path::new("/x/a"), &|a| fake.run(a));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("NOT enabled") && lines[0].contains("failed: boom") && !lines[0].contains("second line"), "{lines:?}");
        assert!(lines[0].contains("claude plugin marketplace add") && lines[0].contains("claude plugin install a2a-pane@codetypera2a"), "{lines:?}");
        let missing = setup_plugin(&dirs, Path::new("/x/a"), &no_claude);
        assert!(missing[0].contains("cannot run claude"), "{missing:?}");
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn an_older_release_of_the_pane_is_replaced_and_then_updated_in_claude() {
        let dir = tmp("setup-stale");
        let dirs = Dirs { root: dir.clone(), mcp_file: dir.join(".claude.json") };
        let mut m = Manifest::default();
        install_pane(&dirs, &mut m, false).unwrap();
        let manifest = marketplace_dir(&dirs).join("a2a-pane").join(PLUGIN_JSON);
        let old = std::fs::read_to_string(&manifest).unwrap().replace(&embedded_version().unwrap(), "0.0.1");
        std::fs::write(&manifest, &old).unwrap();
        m.files.insert(format!("a2a-marketplace/a2a-pane/{PLUGIN_JSON}"), hash(old.as_bytes()));
        m.save(&dir).unwrap();
        let fake = Fake::new(r#"[{"name":"codetypera2a"}]"#, r#"[{"id":"a2a-pane@codetypera2a","version":"0.0.1"}]"#);
        let lines = run(&dirs, Path::new("/x/a"), &Opts { force: false, md_only: false }, &|a| fake.run(a)).unwrap();
        assert_eq!(installed_version(&dirs), embedded_version(), "the new files replaced the old ones");
        assert!(fake.mutations().iter().any(|c| c.starts_with("plugin update a2a-pane@codetypera2a")), "{:?}", fake.calls);
        assert!(lines.iter().any(|l| l.contains("updated from 0.0.1")), "{lines:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dirs_follow_flag_then_env_then_home() {
        let home = Path::new("/h");
        let d = |flag: Option<&str>, env: Option<&str>| resolve_dirs(flag.map(PathBuf::from), env.map(PathBuf::from), home);
        assert_eq!(d(None, None), Dirs { root: "/h/.claude".into(), mcp_file: "/h/.claude.json".into() });
        assert_eq!(d(None, Some("/e")), Dirs { root: "/e".into(), mcp_file: "/e/.claude.json".into() });
        assert_eq!(d(Some("/f"), Some("/e")), Dirs { root: "/f".into(), mcp_file: "/f/.claude.json".into() });
        assert_eq!(d(Some("/h/.claude"), None).mcp_file, Path::new("/h/.claude.json"), "the default dir keeps the default file");
        assert_eq!(d(None, Some("/h/.claude")).mcp_file, Path::new("/h/.claude/.claude.json"), "an exported dir uses its own file");
    }

    #[test]
    fn opencode_entries_become_claude_entries() {
        assert_eq!(claude_server(&json!({ "type": "remote", "url": "https://x/mcp" })), json!({ "type": "http", "url": "https://x/mcp" }));
        let local = json!({ "type": "local", "command": ["npx", "-y", "@playwright/mcp@latest"], "codemode": false });
        assert_eq!(claude_server(&local), json!({ "type": "stdio", "command": "npx", "args": ["-y", "@playwright/mcp@latest"], "env": {} }));
    }

    #[test]
    fn register_adds_servers_keeps_other_keys_and_backs_up() {
        let dir = tmp("reg");
        let file = dir.join(".claude.json");
        std::fs::write(&file, r#"{"userID":"u","mcpServers":{"mine":{"type":"stdio","command":"m","args":[],"env":{}},"engram":{"type":"stdio","command":"/my/engram","args":["mcp"],"env":{}}}}"#).unwrap();
        let note = register_mcp(&file, Path::new("/x/codetypera2a"), false).unwrap();
        assert!(note.contains("a2a registered") && note.contains("mcp servers added: codegraph, context7, playwright"), "{note}");
        let cfg = read(&file);
        assert_eq!(cfg["userID"], "u");
        assert_eq!(cfg["mcpServers"]["mine"]["command"], "m");
        assert_eq!(cfg["mcpServers"]["engram"]["command"], "/my/engram", "existing entries are never replaced");
        assert_eq!(cfg["mcpServers"]["a2a"], json!({ "type": "stdio", "command": "/x/codetypera2a", "args": ["hub"], "env": {} }));
        assert_eq!(cfg["mcpServers"]["context7"]["type"], "http");
        assert!(dir.join(".claude.json.bak-a2a").exists());
        let before = std::fs::read_to_string(&file).unwrap();
        let again = register_mcp(&file, Path::new("/x/codetypera2a"), false).unwrap();
        assert!(again.contains("already registered") && !again.contains("added"), "{again}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "second run changes nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn register_creates_the_file_and_repairs_a_dead_path() {
        let dir = tmp("repair");
        let file = dir.join(".claude.json");
        register_mcp(&file, Path::new("/gone/codetypera2a"), false).unwrap();
        assert!(!dir.join(".claude.json.bak-a2a").exists(), "nothing to back up on a new file");
        let new = std::env::current_exe().unwrap();
        let note = register_mcp(&file, &new, false).unwrap();
        assert!(note.contains(&format!("path repaired (/gone/codetypera2a -> {})", new.display())), "{note}");
        assert_eq!(read(&file)["mcpServers"]["a2a"]["command"], new.display().to_string());
        let forced = register_mcp(&file, Path::new("/other/codetypera2a"), true).unwrap();
        assert!(forced.contains("registered in"), "{forced}");
        assert_eq!(read(&file)["mcpServers"]["a2a"]["command"], "/other/codetypera2a");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn register_leaves_a_broken_file_alone() {
        let dir = tmp("broken");
        let file = dir.join(".claude.json");
        std::fs::write(&file, "{ not json").unwrap();
        assert!(register_mcp(&file, Path::new("/x/a"), false).unwrap_err().contains("left untouched"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{ not json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_installs_converted_files_once_and_force_replaces_them() {
        let dir = tmp("run");
        let dirs = Dirs { root: dir.join("cfg"), mcp_file: dir.join("cfg/.claude.json") };
        let opts = Opts { force: false, md_only: false };
        let lines = run(&dirs, Path::new("/x/codetypera2a"), &opts, &no_claude).unwrap();
        assert!(lines[0].contains("written") && lines[0].contains("0 up to date, 0 kept"), "{lines:?}");
        let orch = std::fs::read_to_string(dirs.root.join("agents/orchestrator.md")).unwrap();
        assert!(orch.starts_with("---\nname: orchestrator\n") && orch.contains("tools: Agent(code-reviewer, doc-lookup, ") && orch.contains("mcp__a2a__*"), "{orch}");
        assert!(dirs.root.join("commands/plan-new.md").exists());
        assert_eq!(read(&dirs.mcp_file)["mcpServers"]["a2a"]["args"], json!(["hub"]));
        std::fs::write(dirs.root.join("agents/orchestrator.md"), "mine").unwrap();
        let again = run(&dirs, Path::new("/x/codetypera2a"), &opts, &no_claude).unwrap();
        assert!(again[0].starts_with("agents and commands: 0 written") && again[0].contains("1 kept"), "{again:?}");
        assert!(again.iter().any(|l| l.contains("agents/orchestrator.md")), "{again:?}");
        assert_eq!(std::fs::read_to_string(dirs.root.join("agents/orchestrator.md")).unwrap(), "mine");
        std::fs::write(dirs.root.join("agents/orchestrator.md"), "x").unwrap();
        let third = run(&dirs, Path::new("/x/codetypera2a"), &opts, &no_claude).unwrap();
        assert!(third[0].contains("1 kept"));
        std::fs::remove_file(dirs.root.join("agents/orchestrator.md")).unwrap();
        let fourth = run(&dirs, Path::new("/x/codetypera2a"), &opts, &no_claude).unwrap();
        assert!(fourth[0].contains("1 written") && fourth[0].contains("0 kept"), "{fourth:?}");
        let fifth = run(&dirs, Path::new("/x/codetypera2a"), &opts, &no_claude).unwrap();
        assert!(fifth[0].contains("0 written") && fifth[1].contains("0 written"), "an up-to-date install writes 0 files: {fifth:?}");
        run(&dirs, Path::new("/x/codetypera2a"), &Opts { force: true, md_only: false }, &no_claude).unwrap();
        assert_ne!(std::fs::read_to_string(dirs.root.join("agents/orchestrator.md")).unwrap(), "mine");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn md_only_refreshes_markdown_and_skips_mcp() {
        let dir = tmp("mdonly");
        let dirs = Dirs { root: dir.join("cfg"), mcp_file: dir.join("cfg/.claude.json") };
        let lines = run(&dirs, Path::new("/x/a"), &Opts { force: false, md_only: true }, &no_claude).unwrap();
        assert!(lines[1].contains("--md-only"), "{lines:?}");
        assert!(!dirs.mcp_file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
