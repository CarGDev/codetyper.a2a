//! Is Claude Code work still going when an opencode exits? The shutdown of the last opencode must not
//! take the dispatcher or the `a2a-status` window down while a Claude turn is running or a task is open
//! for an agent that runs on the Claude backend.
use crate::{hub::db, sys};
use rusqlite::Connection;
use std::path::Path;

const OPEN_TASK_ON_CLAUDE: &str = "SELECT 1 FROM tasks t JOIN sessions s ON s.agent = t.to_agent \
     WHERE t.state IN ('submitted','working','input-required','auth-required') \
     AND s.backend = 'claude' AND s.status = 'live' LIMIT 1";

/// A `<session>.pid` file in `runs` whose process is alive. Stale files are ignored, never deleted here.
fn live_turn_in(runs: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(runs) else {
        return false;
    };
    rd.flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "pid"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| t.trim().parse::<u32>().ok())
        .any(sys::pid_alive)
}

fn open_task_on_claude(conn: &Connection) -> bool {
    conn.query_row(OPEN_TASK_ON_CLAUDE, [], |_| Ok(())).is_ok()
}

/// True when a Claude turn is alive in `runs` or `conn` has an open task for a live Claude-backend session.
pub fn claude_work_live_in(conn: Option<&Connection>, runs: &Path) -> bool {
    live_turn_in(runs) || conn.is_some_and(open_task_on_claude)
}

/// [`claude_work_live_in`] over this setup's hub database and runs folder.
pub fn claude_work_live() -> bool {
    let conn = db::open(&crate::paths::db_path()).ok();
    claude_work_live_in(conn.as_ref(), &crate::paths::claude_runs_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("a2a-live-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn hub(sessions: &[(&str, &str)], tasks: &[(&str, &str)]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE tasks (to_agent TEXT, state TEXT);
             CREATE TABLE sessions (agent TEXT, backend TEXT, status TEXT);",
        )
        .unwrap();
        for (agent, backend) in sessions {
            let b = (*backend != "").then_some(*backend);
            c.execute(
                "INSERT INTO sessions VALUES (?, ?, 'live')",
                rusqlite::params![agent, b],
            )
            .unwrap();
        }
        for (agent, state) in tasks {
            c.execute(
                "INSERT INTO tasks VALUES (?, ?)",
                rusqlite::params![agent, state],
            )
            .unwrap();
        }
        c
    }

    #[test]
    fn a_live_pid_in_the_runs_folder_counts_and_a_stale_one_does_not() {
        let runs = tmp("pid");
        assert!(!claude_work_live_in(None, &runs));
        std::fs::write(runs.join("s1.pid"), "999999999").unwrap();
        assert!(!claude_work_live_in(None, &runs), "dead pid is ignored");
        std::fs::write(runs.join("s2.pid"), std::process::id().to_string()).unwrap();
        assert!(claude_work_live_in(None, &runs));
        let _ = std::fs::remove_dir_all(&runs);
    }

    #[test]
    fn an_open_task_counts_only_for_a_claude_backend_agent() {
        let runs = tmp("task");
        let claude = hub(&[("coder", "claude")], &[("coder", "working")]);
        assert!(claude_work_live_in(Some(&claude), &runs));
        let done = hub(
            &[("coder", "claude")],
            &[("coder", "completed"), ("coder", "canceled")],
        );
        assert!(!claude_work_live_in(Some(&done), &runs));
        let oc = hub(&[("coder", "")], &[("coder", "working")]);
        assert!(
            !claude_work_live_in(Some(&oc), &runs),
            "opencode-backend work does not keep Claude's setup alive"
        );
        let _ = std::fs::remove_dir_all(&runs);
    }
}
