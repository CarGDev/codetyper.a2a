//! What `uninstall` removes, decided from a snapshot of the machine and with no side effects.
use std::path::{Path, PathBuf};

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
    Stop { pid: u32, command: String },
    ClaudePlugin,
    TmuxWindow,
    Remove(PathBuf),
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
    pub processes: Vec<(u32, String)>,
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

pub fn build(env: &Env, probe: &Probe) -> Vec<Action> {
    let list = probe.list;
    let mut plan: Vec<Action> = env
        .processes
        .iter()
        .map(|(pid, command)| Action::Stop {
            pid: *pid,
            command: command.clone(),
        })
        .collect();
    plan.push(Action::ClaudePlugin);
    plan.push(Action::TmuxWindow);
    plan.extend(installed_templates(env, probe.exists));
    plan.push(Action::Remove(env.claude_root.join("a2a-marketplace")));
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
        Action::Stop { pid, command } => format!("stop process {pid} ({command})"),
        Action::ClaudePlugin => "claude plugin uninstall a2a-pane + marketplace remove".into(),
        Action::TmuxWindow => "close the a2a-status tmux window".into(),
        Action::Remove(p) => format!("delete {}", p.display()),
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
mod tests {
    use super::*;

    fn env(keep_binary: bool) -> Env {
        Env {
            home: "/h".into(),
            claude_root: "/h/.claude".into(),
            claude_json: "/h/.claude.json".into(),
            opencode_dir: "/h/.config/opencode".into(),
            runtime_homes: vec!["/h/.codetypera2a".into()],
            temp_dir: "/t".into(),
            processes: vec![(7, "/h/.cargo/bin/oc dispatch".into())],
            exe: Some("/h/.cargo/bin/codetypera2a".into()),
            keep_binary,
            agent_files: vec!["brain.md".into(), "mentor.md".into()],
            command_files: vec!["plan-new.md".into()],
        }
    }

    fn fake(dir: &Path) -> Vec<PathBuf> {
        let names: &[&str] = match dir.to_str().unwrap() {
            "/h/.claude/agents" => &["mine.md", "brain.md"],
            "/h/.config/opencode/commands" => &["plan-new.md"],
            "/h/.config/opencode" => &[
                "opencode.jsonc",
                "opencode.jsonc.bak-a2a",
                "opencode.jsonc.bak-a2a-models",
            ],
            "/t" => &["a2a-window-a2a-status.lock", "other"],
            _ => &[],
        };
        names.iter().map(|n| dir.join(n)).collect()
    }

    fn probe() -> Probe<'static> {
        Probe {
            list: &fake,
            exists: &|p| {
                [
                    "/h/.claude/agents/mine.md",
                    "/h/.claude/agents/brain.md",
                    "/h/.claude/agents/brain.md.bak-a2a",
                    "/h/.config/opencode/commands/plan-new.md",
                ]
                .contains(&p.to_str().unwrap())
            },
            links_to_exe: &|p| p == Path::new("/h/.cargo/bin/oc"),
        }
    }

    #[test]
    fn a_folder_that_is_not_ours_is_never_deleted_and_a_foreign_oc_is_kept() {
        let mut stray = env(false);
        stray.runtime_homes = vec!["/h".into(), "/h/projects".into(), "/h/.codetypera2a".into()];
        let none = |_: &Path| false;
        let foreign = Probe {
            list: &fake,
            exists: &none,
            links_to_exe: &none,
        };
        let plan = build(&stray, &foreign);
        assert!(
            !plan.contains(&Action::Remove("/h".into()))
                && !plan.contains(&Action::Remove("/h/projects".into()))
        );
        assert!(plan.contains(&Action::Remove("/h/.codetypera2a".into())));
        assert!(
            !plan.contains(&Action::Remove("/h/.cargo/bin/oc".into())),
            "oc is not a link to us"
        );
        let marked = |p: &Path| p == Path::new("/h/work/hub.db");
        assert!(
            !looks_like_our_home(Path::new("/h/projects/a2a"), &none),
            "a project folder named a2a"
        );
        assert!(looks_like_our_home(
            Path::new("/h/.codetypera2a"),
            &none
        ));
        assert!(looks_like_our_home(Path::new("/h/work"), &marked));
        assert!(!looks_like_our_home(Path::new("/h/work"), &none));
        let mut by_hand = env(false);
        by_hand.exe = Some("/usr/local/bin/codetypera2a".into());
        assert!(
            !build(&by_hand, &probe())
                .iter()
                .any(|a| matches!(a, Action::Remove(p) if p.starts_with("/usr/local")))
        );
    }

    #[test]
    fn the_plan_removes_everything_ours_and_the_binary() {
        let plan = build(&env(false), &probe());
        let has = |a: Action| plan.contains(&a);
        assert!(has(Action::Stop {
            pid: 7,
            command: "/h/.cargo/bin/oc dispatch".into()
        }));
        assert!(
            has(Action::Remove("/h/.claude/agents/brain.md".into()))
                && has(Action::Remove("/h/.claude/agents/brain.md.bak-a2a".into())),
            "an installed template and its backup copy"
        );
        assert!(
            !has(Action::Remove("/h/.claude/agents/mine.md".into())),
            "an agent the user wrote is never removed"
        );
        assert!(has(Action::Remove(
            "/h/.config/opencode/commands/plan-new.md".into()
        )));
        assert!(has(Action::Remove(
            "/h/.config/opencode/opencode.jsonc.bak-a2a-models".into()
        )));
        assert!(
            !has(Action::Remove("/h/.config/opencode/opencode.jsonc".into())),
            "the config itself stays"
        );
        assert!(
            has(Action::Remove("/t/a2a-window-a2a-status.lock".into()))
                && !has(Action::Remove("/t/other".into()))
        );
        assert!(
            has(Action::Remove("/h/.codetypera2a".into()))
                && has(Action::Remove("/h/.codetypera2a".into()))
        );
        assert!(has(Action::EditRc("/h/.zshrc".into())) && has(Action::CargoUninstall));
        assert!(
            has(Action::Remove("/h/.cargo/bin/codetypera2a".into()))
                && has(Action::Remove("/h/.cargo/bin/oc".into()))
        );
    }

    #[test]
    fn keeping_the_binary_leaves_it_alone() {
        let plan = build(&env(true), &probe());
        assert!(!plan.contains(&Action::CargoUninstall));
        assert!(!plan.contains(&Action::Remove("/h/.cargo/bin/codetypera2a".into())));
    }

    #[test]
    fn the_shell_block_is_cut_out_and_nothing_else() {
        let text = "export A=1\n\n# >>> a2a (codetypera2a) >>>\nalias opencode='oc'\n# <<< a2a (codetypera2a) <<<\nexport B=2\n";
        assert_eq!(without_rc_block(text).unwrap(), "export A=1\nexport B=2\n");
        assert_eq!(without_rc_block("export A=1\n"), None);
        let only = "# >>> a2a (codetypera2a) >>>\nx\n# <<< a2a (codetypera2a) <<<\n";
        assert_eq!(without_rc_block(only).unwrap(), "");
    }

    #[test]
    fn the_owner_is_read_from_the_install_path() {
        assert_eq!(
            owner(Path::new(
                "/opt/homebrew/Cellar/codetypera2a/2/bin/codetypera2a"
            )),
            Some(Action::BrewUninstall)
        );
        assert_eq!(
            owner(Path::new("/h/.cargo/bin/codetypera2a")),
            Some(Action::CargoUninstall)
        );
        assert_eq!(owner(Path::new("/usr/local/bin/codetypera2a")), None);
    }
}
