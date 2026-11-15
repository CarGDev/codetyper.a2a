//! Files `init` installs into the Claude config dir that the user may also own, starting with `CLAUDE.md`.
//! Unlike agent files they are never replaced just because they differ: a file we did not write, or one the
//! user edited, is kept; `--force` replaces it only after a timestamped backup; a symlink is never replaced.
use super::claude::{Manifest, hash};
use std::path::Path;
use std::time::SystemTime;

pub const REL: &str = "CLAUDE.md";
pub static CLAUDE_MD: &str = include_str!("../../templates/claude/CLAUDE.user.md");
/// Every guarded file: path relative to the config dir, and the bytes this release installs.
pub fn guarded_files() -> Vec<(String, Vec<u8>)> {
    let mut files = vec![(REL.to_string(), CLAUDE_MD.as_bytes().to_vec())];
    files.extend(skill_files());
    files
}

/// `skills/<name>/SKILL.md` for every embedded skill, with the bytes this release installs.
pub fn skill_files() -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = crate::cli::SKILLS
        .dirs()
        .filter_map(|dir| {
            let name = dir.path().file_name()?.to_str()?;
            let skill = dir.get_file(dir.path().join(SKILL_FILE))?;
            Some((
                format!("skills/{name}/{SKILL_FILE}"),
                skill.contents().to_vec(),
            ))
        })
        .collect();
    files.sort();
    files
}

#[derive(Debug, Eq, PartialEq)]
pub enum Existing {
    Missing,
    NotRegularFile,
    File(Vec<u8>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeepReason {
    Edited,
    Foreign,
    GentleAi,
    NotRegularFile,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    Write,
    Current,
    Refresh,
    Keep(KeepReason),
    Replace(KeepReason),
}

const SKILL_FILE: &str = "SKILL.md";
const CLAUDE_FORCE: &str = "`init --claude --force`";
const OPENCODE_FORCE: &str = "`init --opencode --force`";
const GENTLE_AI_MARKER: &str = "<!-- gentle-ai:";
const MAX_BACKUP_TRIES: usize = 100;
const SECONDS_PER_DAY: u64 = 86_400;

/// What to do with `existing`, given the hash `init` recorded for it (if any) and the bytes to install.
pub fn decide(existing: &Existing, recorded: Option<&str>, new: &[u8], force: bool) -> Decision {
    let bytes = match existing {
        Existing::Missing => return Decision::Write,
        Existing::NotRegularFile => return Decision::Keep(KeepReason::NotRegularFile),
        Existing::File(bytes) => bytes,
    };
    if bytes == new {
        return Decision::Current;
    }
    if recorded == Some(hash(bytes).as_str()) {
        return Decision::Refresh;
    }
    let reason = keep_reason(bytes, recorded);
    match force {
        true => Decision::Replace(reason),
        false => Decision::Keep(reason),
    }
}

fn keep_reason(bytes: &[u8], recorded: Option<&str>) -> KeepReason {
    let text = String::from_utf8_lossy(bytes);
    if text.contains(GENTLE_AI_MARKER) {
        return KeepReason::GentleAi;
    }
    match recorded {
        Some(_) => KeepReason::Edited,
        None => KeepReason::Foreign,
    }
}

/// `yyyymmddThhmmssZ` in UTC, from days since the epoch (civil-from-days).
fn utc_stamp(now: SystemTime) -> String {
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rest) = (secs / SECONDS_PER_DAY, secs % SECONDS_PER_DAY);
    let shifted = days as i64 + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// `CLAUDE.md.bak-<UTC stamp>`, with `-1`, `-2`, ... appended while the name is taken; an error after 100 tries.
pub fn backup_name(now: SystemTime, taken: &dyn Fn(&str) -> bool) -> Result<String, String> {
    backup_name_for(REL, now, taken)
}

fn backup_name_for(
    file_name: &str,
    now: SystemTime,
    taken: &dyn Fn(&str) -> bool,
) -> Result<String, String> {
    let base = format!("{file_name}.bak-{}", utc_stamp(now));
    (0..MAX_BACKUP_TRIES)
        .map(|n| match n {
            0 => base.clone(),
            n => format!("{base}-{n}"),
        })
        .find(|name| !taken(name))
        .ok_or_else(|| {
            format!("no free backup name for {file_name} after {MAX_BACKUP_TRIES} tries")
        })
}

fn existing_of(path: &Path) -> Result<Existing, String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Existing::Missing),
        Err(err) => return Err(format!("cannot inspect {}: {}", path.display(), err.kind())),
    };
    if !meta.is_file() {
        return Ok(Existing::NotRegularFile);
    }
    std::fs::read(path)
        .map(Existing::File)
        .map_err(|err| format!("cannot read {}: {}", path.display(), err.kind()))
}

/// Writes through a temporary file in the same directory and a rename, so a half-written file never exists.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let dir = path.parent().ok_or("no parent directory")?;
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("cannot create {}: {}", dir.display(), e.kind()))?;
    let temp = dir.join(format!(".a2a-tmp-{}", std::process::id()));
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|_| std::fs::rename(&temp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(|e| format!("cannot write {}: {}", path.display(), e.kind()))
}

/// Copies `path` to a new, never-overwritten timestamped sibling and returns its name.
fn back_up(path: &Path, bytes: &[u8]) -> Result<String, String> {
    use std::io::Write;
    let dir = path.parent().ok_or("no parent directory")?;
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or(REL);
    let name = backup_name_for(file_name, SystemTime::now(), &|candidate| {
        dir.join(candidate).exists()
    })?;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(&name))
        .and_then(|mut file| file.write_all(bytes))
        .map_err(|e| format!("cannot back up {}: {}", path.display(), e.kind()))?;
    Ok(name)
}

fn keep_line(label: &str, force_hint: &str, reason: KeepReason) -> String {
    let replace = format!("{force_hint} replaces it after a backup");
    match reason {
        KeepReason::Edited => format!("{label}: kept (you edited it); {replace}"),
        KeepReason::Foreign => format!("{label}: kept (not written by codetypera2a); {replace}"),
        KeepReason::GentleAi => format!(
            "{label}: kept (managed by gentle-ai); {replace}, but `gentle-ai sync` may write it again"
        ),
        KeepReason::NotRegularFile => {
            format!("{label}: kept (a symlink or not a regular file); replace it yourself")
        }
    }
}

/// Applies the policy to `<root>/<rel>` and returns the one line to show the user. `recorded` is the hash
/// `init` last wrote (`None` for a target without a manifest); `force_hint` names the flag that replaces a kept file.
fn sync_one(
    root: &Path,
    rel: &str,
    new: &[u8],
    recorded: Option<&str>,
    force: bool,
    force_hint: &str,
) -> Result<String, String> {
    let path = root.join(rel);
    let existing = existing_of(&path)?;
    let decision = decide(&existing, recorded, new, force);
    let outcome = match (decision, &existing) {
        (Decision::Keep(reason), _) => return Ok(keep_line(rel, force_hint, reason)),
        (Decision::Current, _) => "up to date".to_string(),
        (Decision::Write, _) => "written".to_string(),
        (Decision::Refresh, _) => "updated".to_string(),
        (Decision::Replace(reason), Existing::File(old)) => {
            let backup = back_up(&path, old)?;
            match reason {
                KeepReason::GentleAi => {
                    format!("replaced (backup: {backup}); `gentle-ai sync` may write it again")
                }
                _ => format!("replaced (backup: {backup})"),
            }
        }
        (Decision::Replace(_), _) => {
            return Ok(keep_line(rel, force_hint, KeepReason::NotRegularFile));
        }
    };
    if decision != Decision::Current {
        write_atomic(&path, new)?;
    }
    Ok(format!("{rel}: {outcome}"))
}

/// The Claude dir: `CLAUDE.md` and every skill, each recorded in the manifest. One line per file.
pub fn sync_all(root: &Path, manifest: &mut Manifest, force: bool) -> Vec<String> {
    guarded_files()
        .into_iter()
        .map(|(rel, bytes)| {
            let recorded = manifest.recorded(&rel).map(str::to_string);
            let line = sync_one(root, &rel, &bytes, recorded.as_deref(), force, CLAUDE_FORCE);
            match line {
                Ok(text) if !text.contains(": kept") => {
                    manifest.record(&rel, hash(&bytes));
                    text
                }
                Ok(text) => text,
                Err(error) => format!("{rel}: {error}"),
            }
        })
        .collect()
}

/// `CLAUDE.md` alone (the Claude policy, with the manifest).
pub fn sync(root: &Path, manifest: &mut Manifest, force: bool) -> Result<String, String> {
    let new = CLAUDE_MD.as_bytes();
    let recorded = manifest.recorded(REL).map(str::to_string);
    let line = sync_one(root, REL, new, recorded.as_deref(), force, CLAUDE_FORCE)?;
    if !line.contains(": kept") {
        manifest.record(REL, hash(new));
    }
    Ok(line)
}

/// The opencode config dir has no manifest: a skill is written when missing, left alone when equal or
/// different, and replaced after a backup only with `--force`. One line per skill.
pub fn sync_skills_without_manifest(root: &Path, force: bool) -> Vec<String> {
    skill_files()
        .into_iter()
        .map(|(rel, bytes)| {
            sync_one(root, &rel, &bytes, None, force, OPENCODE_FORCE)
                .unwrap_or_else(|e| format!("{rel}: {e}"))
        })
        .collect()
}

/// A regular file whose bytes equal this release's embedded ones: ours and unedited, for a dir with no manifest.
pub fn ours_by_bytes(root: &Path, rel: &str, bytes: &[u8]) -> bool {
    matches!(existing_of(&root.join(rel)), Ok(Existing::File(found)) if found == bytes)
}

/// A regular file at `<root>/<rel>` whose bytes still hash to what `init` recorded: ours and unedited.
pub fn ours_unchanged(root: &Path, rel: &str, manifest: &Manifest) -> bool {
    let path = root.join(rel);
    let Ok(Existing::File(bytes)) = existing_of(&path) else {
        return false;
    };
    manifest.recorded(rel) == Some(hash(&bytes).as_str())
}

#[cfg(test)]
mod tests;
