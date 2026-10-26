//! `codetypera2a uninstall` and `reinstall`: delete everything the install put on the machine, for good,
//! in Rust only. No backups: this is a clean slate. `reinstall` keeps the running binary, runs `init`
//! again and checks that nothing old survived.
mod apply;
mod jsonc;
mod plan;
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

/// `ps -axo pid=,command=` output reduced to our dispatchers, never this process or its parent.
fn parse_ps(text: &str, skip: &[u32]) -> Vec<(u32, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, command) = line.split_once(char::is_whitespace)?;
            Some((pid.parse::<u32>().ok()?, command.trim().to_string()))
        })
        .filter(|(pid, command)| !skip.contains(pid) && is_ours(command))
        .collect()
}

fn dispatchers() -> Vec<(u32, String)> {
    let out = Command::new("ps").args(["-axo", "pid=,command="]).output();
    let text = out
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    parse_ps(
        &text,
        &[std::process::id(), std::os::unix::process::parent_id()],
    )
}

fn list(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

fn detect(keep_binary: bool) -> Result<Env, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let claude = crate::targets::claude::dirs_from(&[])?;
    let exe = std::env::current_exe()
        .ok()
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p));
    Ok(Env {
        home,
        claude_root: claude.root,
        claude_json: claude.mcp_file,
        opencode_dir: crate::paths::config_dir(),
        runtime_homes: vec![crate::paths::runtime_home(), crate::paths::legacy_home()],
        temp_dir: std::env::temp_dir(),
        processes: dispatchers(),
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

/// Problems that mean the clean-up did not hold: a live dispatcher, a legacy database, rows in the new one.
fn verify() -> Vec<String> {
    let mut problems = Vec::new();
    let live = dispatchers();
    if !live.is_empty() {
        problems.push(format!("{} dispatcher(s) still running", live.len()));
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
    fn only_our_dispatchers_are_picked_from_the_process_list() {
        let ps = " 8112 /Users/x/.cargo/bin/oc dispatch\n 67814 /Users/x/.cargo/bin/codetypera2a dispatcher\n 5 vim oc dispatch.md\n 9 /usr/bin/other dispatcher\n 77 /x/codetypera2a dispatcher\n";
        let found = parse_ps(ps, &[77]);
        assert_eq!(
            found.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
            vec![8112, 67814]
        );
    }

    #[test]
    fn unknown_flags_are_refused_before_anything_happens() {
        assert_eq!(uninstall(&["--purge".into()]), 2);
        assert_eq!(reinstall(&["--keep-binary".into()]), 2);
        assert_eq!(unknown(&["--yes".into()], &["--yes"]), None);
    }
}
