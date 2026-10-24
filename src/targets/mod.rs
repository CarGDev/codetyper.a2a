//! Install targets of `init`: the tools it can set up, how each is detected and which are ready.
//! Only Claude Code and opencode install anything today; the others are listed so the picker shows
//! what is coming and adding one later is one more row in `TARGETS`.
use std::path::{Path, PathBuf};

pub mod claude;
pub mod convert;
pub mod picker;
pub mod settings;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Id {
    Claude,
    Opencode,
    Codex,
    Cursor,
    Gemini,
    Kiro,
    Vscode,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    Global,
    Project,
}

pub struct Target {
    pub id: Id,
    pub name: &'static str,
    pub scope: Scope,
    /// Installable today; the rest are shown as "coming soon".
    pub ready: bool,
    /// Executables looked up on PATH.
    bins: &'static [&'static str],
    /// Config dirs under the home dir; either one existing counts as detected.
    dirs: &'static [&'static str],
}

pub const TARGETS: [Target; 7] = [
    Target { id: Id::Claude, name: "Claude Code", scope: Scope::Global, ready: true, bins: &["claude"], dirs: &[".claude"] },
    Target { id: Id::Opencode, name: "opencode", scope: Scope::Global, ready: true, bins: &["opencode"], dirs: &[".config/opencode"] },
    Target { id: Id::Codex, name: "Codex CLI", scope: Scope::Global, ready: false, bins: &["codex"], dirs: &[".codex"] },
    Target { id: Id::Cursor, name: "Cursor", scope: Scope::Project, ready: false, bins: &["cursor"], dirs: &[".cursor"] },
    Target { id: Id::Gemini, name: "Gemini CLI", scope: Scope::Global, ready: false, bins: &["gemini"], dirs: &[".gemini"] },
    Target { id: Id::Kiro, name: "Kiro", scope: Scope::Project, ready: false, bins: &["kiro"], dirs: &[".kiro"] },
    Target { id: Id::Vscode, name: "VS Code (Copilot Chat)", scope: Scope::Project, ready: false, bins: &["code"], dirs: &[".vscode"] },
];

/// What detection looks at; injectable so tests need neither a PATH nor a home dir.
pub struct Probe {
    pub on_path: Box<dyn Fn(&str) -> bool>,
    pub exists: Box<dyn Fn(&Path) -> bool>,
    pub home: PathBuf,
}

impl Probe {
    pub fn system() -> Probe {
        Probe {
            on_path: Box::new(|bin| crate::cli::which(bin).is_some()),
            exists: Box::new(|p| p.exists()),
            home: std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")),
        }
    }
}

impl Target {
    pub fn installable(&self) -> bool {
        self.ready
    }

    pub fn detect(&self, probe: &Probe) -> bool {
        self.bins.iter().any(|b| (probe.on_path)(b)) || self.dirs.iter().any(|d| (probe.exists)(&probe.home.join(d)))
    }

    /// The picker row text: `Claude Code (detected) — global only`.
    pub fn label(&self, detected: bool) -> String {
        let state = match (self.ready, detected) {
            (false, _) => "coming soon",
            (true, true) => "detected",
            (true, false) => "not found",
        };
        let scope = if self.ready && self.scope == Scope::Global { " — global only" } else { "" };
        format!("{} ({state}){scope}", self.name)
    }
}

/// Ids of the installable targets found on this machine.
pub fn detected(probe: &Probe) -> Vec<Id> {
    TARGETS.iter().filter(|t| t.installable() && t.detect(probe)).map(|t| t.id).collect()
}

/// The `init` flags that choose targets.
#[derive(Default, Debug, PartialEq)]
pub struct Flags {
    pub all: bool,
    pub claude: bool,
    pub opencode: bool,
    pub yes: bool,
    pub md_only: bool,
    pub tty: bool,
}

impl Flags {
    pub fn from_args(args: &[String], tty: bool) -> Flags {
        let has = |f: &str| args.iter().any(|a| a == f);
        Flags { all: has("--all"), claude: has("--claude"), opencode: has("--opencode"), yes: has("--yes"), md_only: has("--md-only"), tty }
    }
}

#[derive(PartialEq, Debug)]
pub enum Plan {
    Run(Vec<Id>),
    /// Nothing decides it: show the picker.
    Ask,
}

/// Which targets `init` sets up. Explicit flags win; `update` (`--md-only`) keeps refreshing opencode;
/// without a terminal or with `--yes` the detected ones are used (opencode when none are, as before).
pub fn plan(flags: &Flags, detected: &[Id]) -> Plan {
    if flags.all {
        return Plan::Run(TARGETS.iter().filter(|t| t.installable()).map(|t| t.id).collect());
    }
    let chosen: Vec<Id> = [(flags.opencode, Id::Opencode), (flags.claude, Id::Claude)].into_iter().filter(|(on, _)| *on).map(|(_, id)| id).collect();
    if !chosen.is_empty() {
        return Plan::Run(chosen);
    }
    if flags.md_only {
        return Plan::Run(vec![Id::Opencode]);
    }
    if !flags.yes && flags.tty {
        return Plan::Ask;
    }
    Plan::Run(if detected.is_empty() { vec![Id::Opencode] } else { detected.to_vec() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(bins: &'static [&'static str], dirs: &'static [&'static str]) -> Probe {
        Probe {
            on_path: Box::new(move |b| bins.contains(&b)),
            exists: Box::new(move |p| dirs.iter().any(|d| p == Path::new("/h").join(d))),
            home: PathBuf::from("/h"),
        }
    }

    fn flags(f: &[&str], tty: bool) -> Flags {
        Flags::from_args(&f.iter().map(|s| s.to_string()).collect::<Vec<_>>(), tty)
    }

    #[test]
    fn detection_uses_path_or_config_dir() {
        assert_eq!(detected(&probe(&["claude"], &[])), [Id::Claude]);
        assert_eq!(detected(&probe(&[], &[".config/opencode"])), [Id::Opencode]);
        assert_eq!(detected(&probe(&["claude", "opencode"], &[])), [Id::Claude, Id::Opencode]);
        assert!(detected(&probe(&[], &[])).is_empty());
    }

    #[test]
    fn coming_soon_targets_are_never_detected_as_installable() {
        let p = probe(&["codex", "cursor", "gemini", "kiro", "code"], &[]);
        assert!(detected(&p).is_empty());
        assert!(TARGETS.iter().filter(|t| !t.ready).all(|t| !t.installable()));
        assert_eq!(TARGETS.iter().filter(|t| t.installable()).count(), 2);
    }

    #[test]
    fn labels_follow_the_screenshot() {
        let [claude, opencode, codex, ..] = &TARGETS;
        assert_eq!(claude.label(true), "Claude Code (detected) — global only");
        assert_eq!(opencode.label(false), "opencode (not found) — global only");
        assert_eq!(codex.label(true), "Codex CLI (coming soon)");
    }

    #[test]
    fn flags_choose_targets_without_asking() {
        assert_eq!(plan(&flags(&["--claude"], true), &[]), Plan::Run(vec![Id::Claude]));
        assert_eq!(plan(&flags(&["--opencode"], true), &[Id::Claude]), Plan::Run(vec![Id::Opencode]));
        assert_eq!(plan(&flags(&["--claude", "--opencode"], true), &[]), Plan::Run(vec![Id::Opencode, Id::Claude]));
        assert_eq!(plan(&flags(&["--all"], true), &[]), Plan::Run(vec![Id::Claude, Id::Opencode]));
    }

    #[test]
    fn update_keeps_refreshing_opencode_only() {
        assert_eq!(plan(&flags(&["--md-only"], true), &[Id::Claude, Id::Opencode]), Plan::Run(vec![Id::Opencode]));
        assert_eq!(plan(&flags(&["--md-only", "--claude"], true), &[]), Plan::Run(vec![Id::Claude]));
    }

    #[test]
    fn picker_only_on_a_terminal_without_yes() {
        assert_eq!(plan(&flags(&[], true), &[Id::Claude]), Plan::Ask);
        assert_eq!(plan(&flags(&["--yes"], true), &[Id::Claude]), Plan::Run(vec![Id::Claude]));
        assert_eq!(plan(&flags(&[], false), &[Id::Claude, Id::Opencode]), Plan::Run(vec![Id::Claude, Id::Opencode]));
        assert_eq!(plan(&flags(&[], false), &[]), Plan::Run(vec![Id::Opencode]), "nothing detected keeps the old default");
    }
}
