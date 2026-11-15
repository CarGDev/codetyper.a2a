//! The deep list: agents the user chose to run on the strongest model (opus on Claude, the most expensive
//! installed model on opencode). It is empty by default and only ever grows by an explicit command or the
//! `A2A_DEEP_AGENTS` environment variable.
use super::config::is_agent_name;
use super::tiers::tier_for;
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

pub const MAX_DEEP: usize = 32;
const MAX_NAME_LEN: usize = 63;
const MAX_FILE_BYTES: u64 = 4096;
const DEEP_FILE: &str = "deep-agents.json";
const ENV_VAR: &str = "A2A_DEEP_AGENTS";
const NEXT_STEP: &str = "applies at the next `codetypera2a init` (Claude files) / `codetypera2a models --recommended` (opencode)";
const USAGE: &str = "usage: codetypera2a models deep list | add <agent>... | remove <agent>...";

/// Agent names that get the strongest model.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeepList(BTreeSet<String>);

impl DeepList {
    pub fn contains(&self, agent: &str) -> bool {
        self.0.contains(agent)
    }

    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.0.iter()
    }

    pub fn insert(&mut self, agent: String) -> bool {
        self.0.insert(agent)
    }

    pub fn remove(&mut self, agent: &str) -> bool {
        self.0.remove(agent)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum DeepError {
    InvalidName(String),
    UnknownAgent(String),
    TooMany,
    Corrupt(PathBuf),
    Io(String),
}

impl fmt::Display for DeepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeepError::InvalidName(name) => write!(f, "invalid agent name: {name:?}"),
            DeepError::UnknownAgent(name) => write!(f, "{name} is not a shipped agent"),
            DeepError::TooMany => write!(f, "the deep list holds at most {MAX_DEEP} agents"),
            DeepError::Corrupt(path) => write!(
                f,
                "{} is not a valid deep list; fix or delete it",
                path.display()
            ),
            DeepError::Io(message) => write!(f, "{message}"),
        }
    }
}

/// A valid agent name that is one of the shipped agents.
pub fn parse_name(raw: &str) -> Result<String, DeepError> {
    let name = raw.trim();
    if !is_agent_name(name) {
        return Err(DeepError::InvalidName(
            name.chars().take(MAX_NAME_LEN + 1).collect(),
        ));
    }
    if tier_for(name).is_none() {
        return Err(DeepError::UnknownAgent(name.to_string()));
    }
    Ok(name.to_string())
}

pub fn deep_path() -> PathBuf {
    crate::paths::runtime_home().join(DEEP_FILE)
}

/// The stored list. A missing file is an empty list; anything that is not a small regular file holding a
/// JSON array of valid shipped agent names is `Corrupt`, and is never rewritten by this module.
pub fn load(path: &Path) -> Result<DeepList, DeepError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(DeepList::default()),
        Err(err) => {
            return Err(DeepError::Io(format!(
                "cannot read {}: {}",
                path.display(),
                err.kind()
            )));
        }
    };
    let corrupt = || DeepError::Corrupt(path.to_path_buf());
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return Err(corrupt());
    }
    let text = std::fs::read_to_string(path).map_err(|_| corrupt())?;
    let names: Vec<String> = serde_json::from_str(&text).map_err(|_| corrupt())?;
    let mut list = DeepList::default();
    for name in &names {
        list.insert(parse_name(name).map_err(|_| corrupt())?);
    }
    if names.len() > MAX_DEEP {
        return Err(corrupt());
    }
    Ok(list)
}

/// Writes the sorted list through a private temporary file and a rename, so a crash never leaves half a file.
/// The temporary file and then the directory are flushed to disk around the rename. Two concurrent
/// `models deep` runs can lose one update (last rename wins): this is a single-user CLI, so there is no lock.
pub fn save(path: &Path, list: &DeepList) -> Result<(), DeepError> {
    if list.len() > MAX_DEEP {
        return Err(DeepError::TooMany);
    }
    let io = |err: std::io::Error| {
        DeepError::Io(format!("cannot write {}: {}", path.display(), err.kind()))
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let names: Vec<&String> = list.iter().collect();
    let body =
        serde_json::to_string_pretty(&names).map_err(|err| DeepError::Io(err.to_string()))?;
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let written = write_private(&temp, &body)
        .and_then(|_| std::fs::rename(&temp, path))
        .and_then(|_| sync_dir(path.parent()));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written.map_err(io)
}

/// Flushes a directory entry (the rename) to disk; a missing parent is nothing to flush.
fn sync_dir(dir: Option<&Path>) -> std::io::Result<()> {
    let Some(dir) = dir.filter(|d| !d.as_os_str().is_empty()) else {
        return Ok(());
    };
    std::fs::File::open(dir)?.sync_all()
}

fn write_private(path: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(body.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    crate::paths::ensure_private_file(path)
}

/// The names in `A2A_DEEP_AGENTS` (comma separated) and one warning per name that is not valid.
pub fn from_env(raw: Option<&str>) -> (DeepList, Vec<String>) {
    let mut list = DeepList::default();
    let mut warnings = Vec::new();
    for piece in raw
        .unwrap_or_default()
        .split(',')
        .filter(|p| !p.trim().is_empty())
    {
        match parse_name(piece) {
            Ok(name) if list.len() < MAX_DEEP => {
                list.insert(name);
            }
            Ok(_) => warnings.push(format!(
                "{ENV_VAR}: more than {MAX_DEEP} agents, the rest are ignored"
            )),
            Err(err) => warnings.push(format!("{ENV_VAR}: {err}")),
        }
    }
    (list, warnings)
}

fn stored_or_warn(path: &Path) -> DeepList {
    load(path).unwrap_or_else(|err| {
        eprintln!("models: deep list ignored: {err}");
        DeepList::default()
    })
}

fn from_environment() -> DeepList {
    let (list, warnings) = from_env(std::env::var(ENV_VAR).ok().as_deref());
    warnings
        .iter()
        .for_each(|warning| eprintln!("models: {warning}"));
    list
}

/// The stored list plus the environment's; never fails (a bad file or name only warns).
pub fn effective() -> DeepList {
    let mut list = stored_or_warn(&deep_path());
    from_environment().iter().for_each(|name| {
        list.insert(name.clone());
    });
    list
}

fn list_command() -> i32 {
    let stored = stored_or_warn(&deep_path());
    let env = from_environment();
    let names: BTreeSet<&String> = stored.iter().chain(env.iter()).collect();
    if names.is_empty() {
        println!("models: no agent is on the deep list (all agents use their default tier)");
    }
    for name in names {
        let source = match (stored.contains(name), env.contains(name)) {
            (true, true) => "file, env",
            (true, false) => "file",
            _ => "env",
        };
        println!("{name}  ({source})");
    }
    0
}

/// Applies `change` to the stored list; nothing is written unless every name is valid.
fn edit_command(names: &[String], change: fn(&mut DeepList, String) -> bool) -> i32 {
    let path = deep_path();
    let outcome = names
        .iter()
        .map(|raw| parse_name(raw))
        .collect::<Result<Vec<_>, _>>()
        .and_then(|valid| {
            let mut list = load(&path)?;
            let changed = valid
                .into_iter()
                .filter(|name| change(&mut list, name.clone()))
                .count();
            save(&path, &list).map(|_| changed)
        });
    match outcome {
        Ok(changed) => {
            println!("models: deep list updated ({changed} change(s)); {NEXT_STEP}");
            0
        }
        Err(err) => {
            eprintln!("models: {err}");
            1
        }
    }
}

/// `codetypera2a models deep list | add <agent>... | remove <agent>...`
pub fn run(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match (args.first().map(String::as_str), rest.is_empty()) {
        (Some("list"), _) => list_command(),
        (Some("add"), false) => edit_command(rest, |list, name| list.insert(name)),
        (Some("remove"), false) => edit_command(rest, |list, name| list.remove(&name)),
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

#[cfg(test)]
mod tests;
