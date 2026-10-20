//! oc: run OpenCode with the A2A dispatcher alongside it.
//!
//!   oc [opencode args]  start the dispatcher in the background (if not running), then run opencode
//!                       right here, in this terminal / tmux pane. Nothing new is opened.
//!   oc dispatch         run the dispatcher in THIS pane, in the foreground
//!   oc agent <name>     switch this folder's newest opencode session to another agent
//!   oc watch            follow the dispatcher log live (what the tmux `a2a-status` window runs)
//!   oc status           dispatcher state, running sessions and open tasks
//!   oc stop             stop the dispatcher
//!   oc trace <task>     per-task event timeline;  oc stats  per-agent summary
//!   oc continue <ctx>   allow another DISPATCH_MAX_HOPS hops for a context that hit the hop cap
//!
//! ONE dispatcher serves all projects (it reads the folder from each task). It is single-instance
//! via a pidfile (<A2A_DB>.dispatcher.pid), so `oc` in any folder reuses it. All agents run inside
//! the opencode session you already have open: the dispatcher switches that session to the
//! assignee and back to the orchestrator; no session is ever created for an agent.
//! Env: A2A_DB, OPENCODE_BIN, DISPATCH_MAX_HOPS
use crate::hub::db;
use crate::{opencode, paths, sys};
use std::process::{Command, Stdio};

pub const OC_HELP: &str = "\
oc - run opencode with the A2A dispatcher alongside it

USAGE:
  oc [opencode args]    start the dispatcher if needed, open the live status in a tmux tab
                        (window `a2a-status`, when inside tmux), then run opencode right here.
                        Arguments are passed to opencode: `oc -c`, `oc /path/to/project`, ...

COMMANDS:
  oc status             dispatcher state, running sessions, open tasks
  oc watch              live view of the dispatcher (what the tmux tab shows)
  oc ensure             start the dispatcher and open the status tab, without starting opencode
                        (the opencode tabs plugin runs this whenever opencode starts)
  oc dispatch           run the dispatcher in this pane (foreground)
  oc stop               stop the dispatcher
  oc agent <name>       switch this folder's newest session to another agent
  oc continue <ctx>     allow more hops for a context that hit the hop cap
  oc trace <task-id>    timeline of one task (id or 8-char prefix): events, offsets, duration
  oc stats              per-agent summary: tasks, nudges, average duration, tokens/cost
  oc help               this help
  oc version            print the version (also --version, -V)

For opencode's own help run: opencode --help
";

fn die(m: &str) -> ! {
    eprintln!("oc: {m}");
    std::process::exit(1);
}

fn ok<T>(r: Result<T, String>) -> T {
    r.unwrap_or_else(|m| die(&m))
}

fn cwd() -> String {
    std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_else(|_| ".".into())
}

fn ensure_dispatcher() {
    match crate::dispatcher::dispatcher::ensure_running() {
        Ok(Some(pid)) => println!("dispatcher started in the background (pid {pid})"),
        Ok(None) => println!("dispatcher already running; serving all projects"),
        Err(e) => die(&format!("cannot start the dispatcher: {e}")),
    }
    match crate::dispatcher::dispatcher::ensure_status_window() {
        Ok(sys::Tab::Created) => println!("live status opened in a new tmux window (tab): a2a-status"),
        Ok(sys::Tab::AlreadyOpen) => println!("live status is already open in tmux window a2a-status"),
        Ok(sys::Tab::NotInTmux) => println!("not inside tmux: run `oc watch` in another terminal for the live status"),
        Err(e) => eprintln!("oc: could not open the tmux status window: {e}"),
    }
}

/// Live view: the current status, then follow the dispatcher log (what the tmux window runs).
fn watch() -> i32 {
    use std::io::{Read, Seek, SeekFrom};
    let log = crate::dispatcher::dispatcher::log_path();
    println!("a2a live status  (log: {})", log.display());
    if let Ok(d) = db::open(&paths::db_path()) {
        if let Ok(open) = db::tasks(&d, "WHERE state IN ('submitted','working','input-required','auth-required') ORDER BY updated_at DESC LIMIT 10", &[]) {
            println!("open tasks: {}", open.len());
            for t in open {
                println!("  {} {:<14} {} -> {}", &t.id[..8], t.state, t.from_agent, t.to_agent);
            }
        }
    }
    println!("{}", "-".repeat(60));
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).read(true).open(&log) else {
        die(&format!("cannot open {}", log.display()))
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(6000);
    let _ = f.seek(SeekFrom::Start(start));
    let mut buf = String::new();
    let _ = f.read_to_string(&mut buf);
    // drop a possibly cut first line, show the last 25
    let lines: Vec<&str> = buf.lines().skip(usize::from(start > 0)).collect();
    for l in &lines[lines.len().saturating_sub(25)..] {
        println!("{l}");
    }
    loop {
        buf.clear();
        if f.read_to_string(&mut buf).is_ok() && !buf.is_empty() {
            print!("{buf}");
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

fn run_opencode(args: &[String]) -> i32 {
    ensure_dispatcher();
    Command::new(opencode::bin())
        .args(args)
        .stdin(Stdio::inherit())
        .status()
        .ok()
        .and_then(|s| s.code())
        .unwrap_or(1)
}

pub fn run(args: &[String]) -> i32 {
    let (cmd, arg) = (args.first().map(String::as_str), args.get(1).map(String::as_str));
    if !matches!(cmd, Some("help" | "--help" | "-h" | "version" | "--version" | "-V")) {
        if let Some(msg) = crate::cli::heal(&paths::config_dir(), &crate::cli::stable_exe()) {
            eprintln!("oc: {msg}");
        }
    }
    match cmd {
        // oc's own help, with no side effects (nothing is started or opened)
        Some("help" | "--help" | "-h") => print!("{OC_HELP}"),
        Some("version" | "--version" | "-V") => println!("oc {}", env!("CARGO_PKG_VERSION")),
        // dispatcher + live status tab, without starting opencode (what the tabs plugin runs)
        Some("ensure") => ensure_dispatcher(),
        Some("dispatch") => return crate::dispatcher::dispatcher::run(),
        Some("watch") => return watch(),
        Some("agent" | "open") => {
            let Some(agent) = arg else { die("usage: oc agent <name>") };
            let dir = cwd();
            let Some(s) = opencode::list(&dir).into_iter().next() else {
                die(&format!("no opencode session open in {dir}; start one with: oc"))
            };
            ok(opencode::switch_agent(&s.id, agent));
            println!("session {} -> agent {agent}", s.id);
        }
        Some("stop") => {
            match sys::dispatcher_pid(&paths::dispatcher_pid_path()) {
                Some(pid) => sys::kill_term(pid),
                None => {
                    let _ = Command::new("pkill").args(["-f", "codetypera2a dispatcher"]).status();
                }
            }
            println!("dispatcher stopped");
        }
        Some("continue") => {
            let Some(ctx) = arg else { die("usage: oc continue <contextId>") };
            let max_hops = paths::env_u64("DISPATCH_MAX_HOPS", 40) as i64;
            let d = ok(db::open(&paths::db_path()));
            ok(d.execute(
                "INSERT INTO hop_allow (context_id, grants) VALUES (?, 1) ON CONFLICT(context_id) DO UPDATE SET grants = grants + 1",
                [ctx],
            )
            .map_err(|e| e.to_string()));
            let g: i64 = ok(d.query_row("SELECT grants FROM hop_allow WHERE context_id = ?", [ctx], |r| r.get(0)).map_err(|e| e.to_string()));
            println!("context {ctx}: allowance is now {} hops (+{max_hops})", max_hops * (1 + g));
        }
        Some("trace") => {
            let Some(id) = arg else { die("usage: oc trace <task-id-or-prefix>") };
            let d = ok(db::open(&paths::db_path()));
            let t = ok(crate::trace::find_task(&d, id));
            let evs = ok(crate::trace::events_for(&d, &t.id));
            print!("{}", crate::trace::format_trace(&t, &evs));
        }
        Some("stats") => {
            let d = ok(db::open(&paths::db_path()));
            print!("{}", crate::trace::format_stats(&ok(crate::trace::agent_stats(&d))));
        }
        Some("status") => {
            let running = sys::dispatcher_pid(&paths::dispatcher_pid_path()).is_some();
            println!("dispatcher: {}", if running { "running" } else { "not running" });
            if let Ok(ids) = opencode::active_ids() {
                println!("sessions running a turn: {}", ids.len());
            }
            let d = ok(db::open(&paths::db_path()));
            let open = ok(db::tasks(
                &d,
                "WHERE state IN ('submitted','working','input-required','auth-required') ORDER BY updated_at DESC LIMIT 20",
                &[],
            ));
            for t in open {
                println!(
                    "{} {:<14} {} -> {} {}",
                    &t.id[..8], t.state, t.from_agent, t.to_agent, t.directory.as_deref().unwrap_or("-")
                );
            }
        }
        // anything else (including nothing) is a normal opencode invocation
        _ => return run_opencode(args),
    }
    0
}
