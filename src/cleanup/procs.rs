//! Which running dispatchers belong to the runtime homes being cleaned. A dispatcher is "ours" only when a
//! claim file inside one of those homes names its pid and the live process is a dispatcher that started no
//! later than the claim was written, so a reused pid or another home's dispatcher is never a kill target.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Clock resolution of `ps` start times against a file's modification time.
pub const START_SLACK: Duration = Duration::from_secs(2);
const MAX_CLAIM_BYTES: u64 = 64;
const PIDFILE_NAMES: [&str; 2] = ["dispatcher.pid", "hub.db.dispatcher.pid"];
const REGISTRY_SUFFIX: &str = ".d";
const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 3600;
const SECONDS_PER_DAY: u64 = 86_400;

/// A pid recorded by a pidfile or a registry entry, and when that file was written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Claim {
    pub pid: u32,
    pub file: PathBuf,
    pub at: SystemTime,
}

/// One dispatcher row of the process table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub command: String,
    pub started: SystemTime,
}

/// Why a claim file no longer points at one of our dispatchers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StaleReason {
    Gone,
    NotDispatcher,
    StartedAfterClaim,
    UnreadableClaim,
}

impl StaleReason {
    pub fn text(self) -> &'static str {
        match self {
            StaleReason::Gone => "gone",
            StaleReason::NotDispatcher => "not a dispatcher command",
            StaleReason::StartedAfterClaim => "started after its claim",
            StaleReason::UnreadableClaim => "unreadable claim",
        }
    }
}

/// A claim file that is not usable, with the pid it named when it could be read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Stale {
    pub file: PathBuf,
    pub pid: Option<u32>,
    pub reason: StaleReason,
}

/// Claim files that were present but could not be read as a claim.
pub fn unreadable(files: Vec<PathBuf>) -> Vec<Stale> {
    files
        .into_iter()
        .map(|file| Stale {
            file,
            pid: None,
            reason: StaleReason::UnreadableClaim,
        })
        .collect()
}

/// A live dispatcher with the claimed pid means it started after the claim; otherwise the pid is
/// either some other program or no process at all.
fn stale_reason(claim: &Claim, procs: &[Proc], alive: &dyn Fn(u32) -> bool) -> StaleReason {
    if procs.iter().any(|proc| proc.pid == claim.pid) {
        return StaleReason::StartedAfterClaim;
    }
    if alive(claim.pid) {
        return StaleReason::NotDispatcher;
    }
    StaleReason::Gone
}

/// Dispatchers split by who owns them. `ours` carries the claim time for the pre-signal re-check.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Scoped {
    pub ours: Vec<(Proc, SystemTime)>,
    pub stale: Vec<Stale>,
    pub foreign: usize,
}

/// The claim names this process, which is a dispatcher that started no later than the claim file was written.
pub fn claims_owned(claim: &Claim, proc: &Proc) -> bool {
    claim.pid == proc.pid && super::is_ours(&proc.command) && proc.started <= claim.at + START_SLACK
}

/// Splits the dispatchers in `procs` by the claims: owned ones (one per pid), dispatchers nobody here
/// claimed (`foreign`, counted only), and claim files that no longer point at one of ours (`stale`).
pub fn scope(claims: &[Claim], procs: &[Proc], alive: &dyn Fn(u32) -> bool) -> Scoped {
    let mut scoped = Scoped::default();
    for claim in claims {
        let owner = procs.iter().find(|proc| claims_owned(claim, proc));
        let already_owned = scoped.ours.iter().any(|(proc, _)| proc.pid == claim.pid);
        match owner {
            Some(_) if already_owned => {}
            Some(proc) => scoped.ours.push((proc.clone(), claim.at)),
            None => scoped.stale.push(Stale {
                file: claim.file.clone(),
                pid: Some(claim.pid),
                reason: stale_reason(claim, procs, alive),
            }),
        }
    }
    scoped.foreign = procs
        .iter()
        .filter(|proc| !scoped.ours.iter().any(|(owned, _)| owned.pid == proc.pid))
        .count();
    scoped
}

/// `ps` elapsed time: `[[dd-]hh:]mm:ss` (a bare number of seconds is accepted too).
pub fn parse_etime(text: &str) -> Option<Duration> {
    let text = text.trim();
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, text),
    };
    let fields: Vec<u64> = clock
        .split(':')
        .map(|field| field.parse().ok())
        .collect::<Option<_>>()?;
    let seconds = match fields.as_slice() {
        [seconds] => *seconds,
        [minutes, seconds] => minutes * SECONDS_PER_MINUTE + seconds,
        [hours, minutes, seconds] => {
            hours * SECONDS_PER_HOUR + minutes * SECONDS_PER_MINUTE + seconds
        }
        _ => return None,
    };
    Some(Duration::from_secs(days * SECONDS_PER_DAY + seconds))
}

fn next_field(text: &str) -> Option<(&str, &str)> {
    let (field, rest) = text.trim_start().split_once(char::is_whitespace)?;
    Some((field, rest.trim()))
}

/// `ps -axo pid=,etime=,command=` output reduced to our dispatchers, never the pids in `skip`.
pub fn parse_ps(text: &str, now: SystemTime, skip: &[u32]) -> Vec<Proc> {
    text.lines()
        .filter_map(|line| {
            let (pid, rest) = next_field(line)?;
            let (etime, command) = next_field(rest)?;
            let pid = pid.parse::<u32>().ok()?;
            let started = now.checked_sub(parse_etime(etime)?)?;
            Some(Proc {
                pid,
                command: command.to_string(),
                started,
            })
        })
        .filter(|proc| !skip.contains(&proc.pid) && super::is_ours(&proc.command))
        .collect()
}

/// Right before a signal: the process is still a dispatcher and still the one the claim was written for.
pub fn still_ours(
    command: &str,
    elapsed: Duration,
    now: SystemTime,
    claimed_at: SystemTime,
) -> bool {
    let Some(started) = now.checked_sub(elapsed) else {
        return false;
    };
    super::is_ours(command) && started <= claimed_at + START_SLACK
}

/// Every file that may claim a dispatcher: the pidfiles of each home, the active pidfile, and the
/// registry entries (`<pidfile>.d/*`) next to each.
pub fn claim_files(
    homes: &[PathBuf],
    active_pid: Option<&Path>,
    list: &dyn Fn(&Path) -> Vec<PathBuf>,
) -> Vec<PathBuf> {
    let mut pidfiles: Vec<PathBuf> = homes
        .iter()
        .flat_map(|home| PIDFILE_NAMES.iter().map(move |name| home.join(name)))
        .collect();
    pidfiles.extend(active_pid.map(Path::to_path_buf));
    let mut files = Vec::new();
    for pidfile in pidfiles {
        let registry = PathBuf::from(format!("{}{REGISTRY_SUFFIX}", pidfile.display()));
        files.extend(list(&registry));
        files.push(pidfile);
    }
    files.sort();
    files.dedup();
    files
}

fn parse_claimed_pid(text: &str) -> Option<u32> {
    text.trim()
        .parse::<u32>()
        .ok()
        .filter(|pid| crate::sys::is_signalable_pid(*pid))
}

/// A registry entry sits directly in `<pidfile>.d` of one of the pidfiles being scanned, nowhere else.
fn is_registry_entry(file: &Path, candidates: &[PathBuf]) -> bool {
    let Some(dir) = file.parent() else {
        return false;
    };
    candidates
        .iter()
        .any(|pidfile| PathBuf::from(format!("{}{REGISTRY_SUFFIX}", pidfile.display())) == dir)
}

/// The claim a file makes, or `None` when it is not a small regular file holding a valid pid.
fn read_claim(file: &Path, meta: &std::fs::Metadata, candidates: &[PathBuf]) -> Option<Claim> {
    if !meta.is_file() || meta.len() > MAX_CLAIM_BYTES {
        return None;
    }
    let pid_text = if is_registry_entry(file, candidates) {
        file.file_name()?.to_str()?.to_string()
    } else {
        std::fs::read_to_string(file).ok()?
    };
    Some(Claim {
        pid: parse_claimed_pid(&pid_text)?,
        file: file.to_path_buf(),
        at: meta.modified().ok()?,
    })
}

/// Valid claims and the files that were present but unusable (symlinks, oversized, malformed).
/// A file that does not exist is simply not a claim.
pub fn read_claims(files: &[PathBuf]) -> (Vec<Claim>, Vec<PathBuf>) {
    let mut claims = Vec::new();
    let mut rejected = Vec::new();
    for file in files {
        let Ok(meta) = std::fs::symlink_metadata(file) else {
            continue;
        };
        match read_claim(file, &meta, files) {
            Some(claim) => claims.push(claim),
            None => rejected.push(file.clone()),
        }
    }
    (claims, rejected)
}

/// One `ps` call: our dispatchers on the machine, never this process or its parent. The clock is read before
/// `ps` runs: its elapsed times are measured at some later moment, so reading it afterwards would push every
/// computed start time later by however long a loaded machine took to run `ps`.
pub fn process_table() -> Vec<Proc> {
    let skip = [std::process::id(), std::os::unix::process::parent_id()];
    table_from(&SystemTime::now, &run_ps, &skip)
}

fn run_ps() -> String {
    let output = std::process::Command::new("ps")
        .args(["-axo", "pid=,etime=,command="])
        .output();
    output
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Reads `clock` and then `ps`, in that order, and builds the table from both.
fn table_from(clock: &dyn Fn() -> SystemTime, ps: &dyn Fn() -> String, skip: &[u32]) -> Vec<Proc> {
    let sampled_at = clock();
    let text = ps();
    parse_ps(&text, sampled_at, skip)
}

#[cfg(test)]
mod tests;
