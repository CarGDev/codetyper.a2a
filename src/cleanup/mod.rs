//! `codetypera2a uninstall` and `reinstall`: delete everything the install put on the machine, for good,
//! in Rust only. No backups: this is a clean slate. `reinstall` keeps the running binary, runs `init`
//! again and checks that nothing old survived.
mod apply;
mod jsonc;
mod plan;
mod procs;
mod templates;

use plan::{Action, Env};
use std::path::{Path, PathBuf};
use std::process::Command;

const UNINSTALL_USAGE: &str = "usage: codetypera2a uninstall [--yes] [--keep-binary]";
const REINSTALL_USAGE: &str = "usage: codetypera2a reinstall [--yes]";
const STATE_TABLES: [&str; 5] = ["tasks", "events", "sessions", "contexts", "nudges"];

fn unknown(args: &[String], allowed: &[&str]) -> Option<String> {
    args.iter()
        .find(|a| !allowed.contains(&a.as_str()))
        .cloned()
}

/// A dispatcher of ours: `<...>/codetypera2a dispatcher` or `<...>/oc dispatch`.
pub(super) fn is_ours(command: &str) -> bool {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    tokens.windows(2).any(|w| {
        let name = w[0].rsplit('/').next().unwrap_or(w[0]);
        (name == "codetypera2a" && w[1] == "dispatcher") || (name == "oc" && w[1] == "dispatch")
    })
}

fn list(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

/// The dispatchers claimed by `homes` (and the active pidfile), split from the other ones on the machine.
fn dispatchers_of(homes: &[PathBuf]) -> procs::Scoped {
    let active = std::env::var_os("A2A_DISPATCHER_PID")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    let files = procs::claim_files(homes, active.as_deref(), &list);
    let (claims, rejected) = procs::read_claims(&files);
    let mut scoped = procs::scope(&claims, &procs::process_table(), &crate::sys::pid_alive);
    scoped.stale.extend(procs::unreadable(rejected));
    scoped
}

fn detect(keep_binary: bool) -> Result<Env, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let claude = crate::targets::claude::dirs_from(&[])?;
    let exe = std::env::current_exe()
        .ok()
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p));
    let runtime_homes = vec![crate::paths::runtime_home(), crate::paths::legacy_home()];
    let dispatchers = dispatchers_of(&runtime_homes);
    let manifest = crate::targets::claude::Manifest::load(&claude.root);
    let opencode_dir = crate::paths::config_dir();
    let claude_ours = crate::targets::guarded::guarded_files()
        .into_iter()
        .filter(|(rel, _)| crate::targets::guarded::ours_unchanged(&claude.root, rel, &manifest))
        .map(|(rel, _)| claude.root.join(rel));
    let opencode_ours = crate::targets::guarded::skill_files()
        .into_iter()
        .filter(|(rel, bytes)| crate::targets::guarded::ours_by_bytes(&opencode_dir, rel, bytes))
        .map(|(rel, _)| opencode_dir.join(rel));
    let guarded_ours = claude_ours.chain(opencode_ours).collect();
    Ok(Env {
        home,
        claude_root: claude.root,
        claude_json: claude.mcp_file,
        opencode_dir,
        runtime_homes,
        temp_dir: std::env::temp_dir(),
        dispatchers,
        guarded_ours,
        exe,
        keep_binary,
        agent_files: templates::agent_files(),
        command_files: templates::command_files(),
    })
}

/// Run (or, without `--yes`, only list) the plan. Returns the number of failed actions.
fn execute(actions: &[Action], apply: bool) -> usize {
    let mut failed = 0;
    for action in actions {
        if let Action::Note(text) = action {
            println!("  note {text}");
            continue;
        }
        if !apply {
            println!("  - {}", plan::describe(action));
            continue;
        }
        match apply::run(action) {
            Ok(message) => println!("  ok   {message}"),
            Err(message) => {
                failed += 1;
                println!("  FAIL {message}");
            }
        }
    }
    failed
}

fn run_plan(keep_binary: bool, apply: bool) -> Result<usize, String> {
    let env = detect(keep_binary)?;
    let exe = env.exe.clone();
    let links_to_exe = |link: &Path| {
        let target = std::fs::read_link(link)
            .ok()
            .and_then(|t| std::fs::canonicalize(link.parent()?.join(t)).ok());
        target.is_some() && target == exe
    };
    let probe = plan::Probe {
        list: &list,
        exists: &|p: &Path| p.exists(),
        links_to_exe: &links_to_exe,
    };
    let actions = plan::build(&env, &probe);
    println!(
        "codetypera2a: {} ({} actions, nothing is backed up)",
        if apply {
            "deleting"
        } else {
            "DRY RUN, pass --yes to delete"
        },
        actions.len()
    );
    Ok(execute(&actions, apply))
}

/// `codetypera2a uninstall [--yes] [--keep-binary]`.
pub fn uninstall(args: &[String]) -> i32 {
    if unknown(args, &["--yes", "--keep-binary"]).is_some() {
        eprintln!("{UNINSTALL_USAGE}");
        return 2;
    }
    let apply = args.iter().any(|a| a == "--yes");
    match run_plan(args.iter().any(|a| a == "--keep-binary"), apply) {
        Ok(0) => 0,
        Ok(failed) => {
            eprintln!("{failed} action(s) failed");
            1
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// Rows in the tables that hold state; a clean install has none.
fn leftover_rows() -> Result<i64, String> {
    let conn = crate::hub::db::open(&crate::paths::db_path())?;
    let mut total = 0;
    for table in STATE_TABLES {
        total += conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(total)
}

/// The homes `verify` looks at.
fn verify_homes() -> Vec<PathBuf> {
    vec![crate::paths::runtime_home(), crate::paths::legacy_home()]
}

/// Problems that mean the clean-up did not hold: a live dispatcher of this home, a legacy database, rows in
/// the new one. Dispatchers of other homes are reported and never fail the check.
fn verify() -> Vec<String> {
    let mut problems = Vec::new();
    let live = dispatchers_of(&verify_homes());
    if !live.ours.is_empty() {
        problems.push(format!(
            "{} dispatcher(s) of this home still running",
            live.ours.len()
        ));
    }
    if live.foreign > 0 {
        println!(
            "note: {} dispatcher(s) of other homes left running (not ours to stop)",
            live.foreign
        );
    }
    if crate::paths::legacy_home().join("hub.db").exists() {
        problems.push("a legacy database exists".into());
    }
    match leftover_rows() {
        Ok(0) => {}
        Ok(n) => problems.push(format!("the database holds {n} rows")),
        Err(e) => problems.push(format!("cannot check the database: {e}")),
    }
    problems
}

/// `codetypera2a reinstall [--yes]`: delete everything except this binary, `init --yes`, verify.
pub fn reinstall(args: &[String]) -> i32 {
    if unknown(args, &["--yes"]).is_some() {
        eprintln!("{REINSTALL_USAGE}");
        return 2;
    }
    let apply = !args.is_empty();
    match run_plan(true, apply) {
        Ok(0) if apply => {}
        Ok(0) => return 0,
        Ok(failed) => {
            eprintln!("{failed} action(s) failed; not reinstalling");
            return 1;
        }
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    }
    let init =
        std::env::current_exe().and_then(|exe| Command::new(exe).args(["init", "--yes"]).status());
    if !init.is_ok_and(|s| s.success()) {
        eprintln!("init failed");
        return 1;
    }
    let problems = verify();
    if problems.is_empty() {
        println!("verified: no dispatcher, no legacy database, the database is empty");
        return 0;
    }
    problems.iter().for_each(|p| eprintln!("NOT CLEAN: {p}"));
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_covers_every_home_that_stop_covers() {
        let homes = verify_homes();
        assert!(homes.contains(&crate::paths::runtime_home()));
        assert!(homes.contains(&crate::paths::legacy_home()));
    }

    fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn no_process_name_pattern_kill_remains_in_src() {
        let needles = [
            ["pk", "ill"].concat(),
            ["kill", "all"].concat(),
            ["[\"-", "f\""].concat(),
        ];
        let mut files = Vec::new();
        rust_sources(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        assert!(
            files.len() > 20,
            "the source scan found {} files",
            files.len()
        );
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            for needle in &needles {
                assert!(
                    !text.contains(needle.as_str()),
                    "{} kills processes by name pattern ({needle})",
                    file.display()
                );
            }
        }
    }

    #[test]
    fn unknown_flags_are_refused_before_anything_happens() {
        assert_eq!(uninstall(&["--purge".into()]), 2);
        assert_eq!(reinstall(&["--keep-binary".into()]), 2);
        assert_eq!(unknown(&["--yes".into()], &["--yes"]), None);
    }
}
