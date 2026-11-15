//! What `uninstall` removes, decided from a snapshot of the machine and with no side effects.
use super::procs::Scoped;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const RC_START: &str = "# >>> a2a (codetypera2a) >>>";
pub const RC_END: &str = "# <<< a2a (codetypera2a) <<<";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JsonKind {
    Opencode,
    ClaudeJson,
    ClaudeSettings,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Stop {
        pid: u32,
        command: String,
        claimed_at: SystemTime,
    },
    /// A report line (other homes' dispatchers, stale pid files); nothing to carry out.
    Note(String),
    ClaudePlugin,
    TmuxWindow,
    Remove(PathBuf),
    /// Removes a directory only when it is empty (a skill folder after its file went).
    RemoveDirIfEmpty(PathBuf),
    EditJson(PathBuf, JsonKind),
    EditRc(PathBuf),
    CargoUninstall,
    BrewUninstall,
}

/// The facts the plan is built from.
pub struct Env {
    pub home: PathBuf,
    pub claude_root: PathBuf,
    pub claude_json: PathBuf,
    pub opencode_dir: PathBuf,
    pub runtime_homes: Vec<PathBuf>,
    pub temp_dir: PathBuf,
    pub dispatchers: Scoped,
    /// Guarded files (CLAUDE.md) that are still exactly what `init` wrote: the only ones uninstall removes.
    pub guarded_ours: Vec<PathBuf>,
    pub exe: Option<PathBuf>,
    pub keep_binary: bool,
    /// Template files `init` installs (relative paths): the only agents and commands that are removed.
    pub agent_files: Vec<String>,
    pub command_files: Vec<String>,
}

const SHELL_RCS: [&str; 3] = [".zshrc", ".bashrc", ".profile"];
const BACKUP_SUFFIX: &str = ".bak-a2a";

fn entries(list: &dyn Fn(&Path) -> Vec<PathBuf>, dir: &Path, prefix: &str) -> Vec<PathBuf> {
    list(dir)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(prefix))
        })
        .collect()
}

/// Who owns the binary: a Homebrew cellar, cargo's bin dir, or neither.
pub fn owner(exe: &Path) -> Option<Action> {
    let text = exe.to_string_lossy();
    let in_cellar = text.contains("/Cellar/codetypera2a/");
    if in_cellar || text.starts_with("/opt/homebrew/") || text.starts_with("/home/linuxbrew/") {
        return Some(Action::BrewUninstall);
    }
    text.contains("/.cargo/bin/")
        .then_some(Action::CargoUninstall)
}

/// The binary is removed only from a location a package manager owns (never a build folder or a copy someone
/// placed by hand), and `oc` only when it is a link to that very binary (another tool may own an `oc`).
fn binary_actions(env: &Env, links_to_exe: &dyn Fn(&Path) -> bool) -> Vec<Action> {
    let Some(exe) = env.exe.as_ref().filter(|_| !env.keep_binary) else {
        return Vec::new();
    };
    let Some(owner) = owner(exe) else {
        return Vec::new();
    };
    let mut out = vec![owner, Action::Remove(exe.clone())];
    let link = exe
        .parent()
        .map(|dir| dir.join("oc"))
        .filter(|link| links_to_exe(link));
    out.extend(link.map(Action::Remove));
    out
}

const RUNTIME_HOME_NAME: &str = ".codetypera2a";
const LEGACY_HOME_NAME: &str = "a2a";
const LEGACY_HOME_PARENT: &str = "opencode";
const RUNTIME_HOME_MARKERS: [&str; 3] = ["hub.db", "codetypera2a.path", ".migration-release"];

fn name_of(path: &Path) -> Option<&str> {
    path.file_name().and_then(|n| n.to_str())
}

/// A runtime home is deleted only if it is `.codetypera2a`, the legacy `<opencode dir>/a2a`, or holds our files, so a
/// stray `CODETYPERA2A_HOME=$HOME` (or a project folder that happens to be called `a2a`) is never wiped.
pub fn looks_like_our_home(path: &Path, has: &dyn Fn(&Path) -> bool) -> bool {
    let canonical = name_of(path) == Some(RUNTIME_HOME_NAME);
    let legacy = name_of(path) == Some(LEGACY_HOME_NAME)
        && path.parent().and_then(name_of) == Some(LEGACY_HOME_PARENT);
    canonical
        || legacy
        || RUNTIME_HOME_MARKERS
            .iter()
            .any(|marker| has(&path.join(marker)))
}

/// Facts about the file system the plan needs, injected so planning stays pure.
pub struct Probe<'a> {
    pub list: &'a dyn Fn(&Path) -> Vec<PathBuf>,
    pub exists: &'a dyn Fn(&Path) -> bool,
    pub links_to_exe: &'a dyn Fn(&Path) -> bool,
}

/// The template agents and commands that are really on disk, and the `.bak-a2a` copy `init` may have left next to
/// each. Files the user wrote under other names are never in this list.
fn installed_templates(env: &Env, exists: &dyn Fn(&Path) -> bool) -> Vec<Action> {
    let groups = [
        ("agents", &env.agent_files),
        ("commands", &env.command_files),
    ];
    let mut out = Vec::new();
    for root in [&env.claude_root, &env.opencode_dir] {
        for (folder, names) in groups {
            for name in names.iter() {
                let path = root.join(folder).join(name);
                let backup = PathBuf::from(format!("{}{BACKUP_SUFFIX}", path.display()));
                out.extend(
                    [path, backup]
                        .into_iter()
                        .filter(|p| exists(p))
                        .map(Action::Remove),
                );
            }
        }
    }
    out
}

/// The folders of the skill files being removed: each goes away too once it is empty.
fn skill_dirs(files: &[PathBuf]) -> impl Iterator<Item = PathBuf> + '_ {
    files
        .iter()
        .filter(|file| file.file_name().is_some_and(|name| name == "SKILL.md"))
        .filter_map(|file| file.parent().map(Path::to_path_buf))
}

/// Report lines for dispatchers that are not ours to stop. Counts only: another home's command line may
/// carry paths that belong to someone else.
fn dispatcher_notes(dispatchers: &Scoped) -> Vec<Action> {
    let foreign = (dispatchers.foreign > 0).then(|| {
        format!(
            "{} dispatcher(s) of other homes left running (not ours to stop)",
            dispatchers.foreign
        )
    });
    let stale = dispatchers.stale.iter().map(|claim| {
        let who = claim.pid.map_or_else(
            || {
                let name = claim.file.file_name().unwrap_or_default();
                format!("file {}", name.to_string_lossy())
            },
            |pid| format!("pid {pid}"),
        );
        format!("stale dispatcher claim, {who}: {}", claim.reason.text())
    });
    foreign.into_iter().chain(stale).map(Action::Note).collect()
}

pub fn build(env: &Env, probe: &Probe) -> Vec<Action> {
    let list = probe.list;
    let mut plan: Vec<Action> = env
        .dispatchers
        .ours
        .iter()
        .map(|(proc, claimed_at)| Action::Stop {
            pid: proc.pid,
            command: proc.command.clone(),
            claimed_at: *claimed_at,
        })
        .collect();
    plan.extend(dispatcher_notes(&env.dispatchers));
    plan.push(Action::ClaudePlugin);
    plan.push(Action::TmuxWindow);
    plan.extend(installed_templates(env, probe.exists));
    plan.push(Action::Remove(env.claude_root.join("a2a-marketplace")));
    plan.extend(env.guarded_ours.iter().cloned().map(Action::Remove));
    plan.extend(skill_dirs(&env.guarded_ours).map(Action::RemoveDirIfEmpty));
    plan.push(Action::Remove(env.claude_root.join(".a2a-manifest.json")));
    plan.push(Action::Remove(env.opencode_dir.join("plugins").join("a2a")));
    plan.extend(
        entries(list, &env.opencode_dir, "opencode.jsonc.bak-a2a")
            .into_iter()
            .map(Action::Remove),
    );
    plan.extend(
        entries(list, &env.temp_dir, "a2a-window-")
            .into_iter()
            .map(Action::Remove),
    );
    plan.extend(
        env.runtime_homes
            .iter()
            .filter(|home| looks_like_our_home(home, probe.exists))
            .cloned()
            .map(Action::Remove),
    );
    for name in ["opencode.jsonc", "opencode.json"] {
        plan.push(Action::EditJson(
            env.opencode_dir.join(name),
            JsonKind::Opencode,
        ));
    }
    plan.push(Action::EditJson(
        env.claude_json.clone(),
        JsonKind::ClaudeJson,
    ));
    plan.push(Action::EditJson(
        env.claude_root.join("settings.json"),
        JsonKind::ClaudeSettings,
    ));
    plan.extend(SHELL_RCS.iter().map(|rc| Action::EditRc(env.home.join(rc))));
    plan.extend(binary_actions(env, probe.links_to_exe));
    plan
}

pub fn describe(action: &Action) -> String {
    match action {
        Action::Stop { pid, command, .. } => format!("stop process {pid} ({command})"),
        Action::Note(text) => text.clone(),
        Action::ClaudePlugin => "claude plugin uninstall a2a-pane + marketplace remove".into(),
        Action::TmuxWindow => "close the a2a-status tmux window".into(),
        Action::Remove(p) => format!("delete {}", p.display()),
        Action::RemoveDirIfEmpty(p) => format!("delete {} if empty", p.display()),
        Action::EditJson(p, _) => format!("remove our entries from {}", p.display()),
        Action::EditRc(p) => format!("remove the a2a block from {}", p.display()),
        Action::CargoUninstall => "cargo uninstall codetypera2a".into(),
        Action::BrewUninstall => "brew uninstall codetypera2a".into(),
    }
}

/// `text` without the a2a block (and the blank line init put before it). `None` = nothing to remove.
pub fn without_rc_block(text: &str) -> Option<String> {
    let start = text.find(RC_START)?;
    let end = text[start..].find(RC_END)? + start + RC_END.len();
    let head = text[..start].trim_end_matches('\n');
    let tail = text[end..].trim_start_matches('\n');
    let joined = match (head.is_empty(), tail.is_empty()) {
        (true, true) => String::new(),
        (true, false) => tail.to_string(),
        (false, true) => format!("{head}\n"),
        (false, false) => format!("{head}\n{tail}"),
    };
    Some(joined)
}

#[cfg(test)]
mod tests;
