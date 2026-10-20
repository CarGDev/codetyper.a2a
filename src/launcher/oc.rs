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
//!   oc open-project <dir> [--prompt <text>]
//!                       open <dir> in its own tmux tab running `oc`; with --prompt, hand the text to
//!                       that tab's orchestrator
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
  oc open-project <dir> [--prompt <text>]
                        open <dir> in its own tmux tab (running `oc`); with --prompt, send the text
                        to that project's orchestrator
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

/// Same rule as the dispatcher: agent-to-agent nudges against DISPATCH_MAX_HOPS * (1 + grants).
fn hop_capped(d: &rusqlite::Connection, ctx: &str, max_hops: i64) -> bool {
    let n: i64 = d
        .query_row("SELECT COUNT(*) FROM nudges WHERE context_id = ? AND kind IN ('assignee','requester')", [ctx], |r| r.get(0))
        .unwrap_or(0);
    let grants: i64 = d.query_row("SELECT grants FROM hop_allow WHERE context_id = ?", [ctx], |r| r.get(0)).unwrap_or(0);
    n >= max_hops * (1 + grants)
}

/// `oc open-project <dir> [--prompt <text>]`: one tmux tab per project, running `oc` there.
fn open_project(args: &[String]) -> i32 {
    let mut dir = None;
    let mut prompt = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--prompt" => prompt = it.next().cloned(),
            _ if dir.is_none() => dir = Some(a.clone()),
            _ => die("usage: oc open-project <dir> [--prompt <text>]"),
        }
    }
    let Some(dir) = dir else { die("usage: oc open-project <dir> [--prompt <text>]") };
    let dir = std::fs::canonicalize(&dir).unwrap_or_else(|_| die(&format!("no such folder: {dir}"))).display().to_string();
    let cmd = format!("cd '{}' && oc", dir.replace('\'', "'\\''"));
    match ok(sys::tmux_ensure_window(&sys::dir_base(&dir), &cmd)) {
        sys::Tab::Created => println!("opened tmux window {} for {dir}", sys::dir_base(&dir)),
        sys::Tab::AlreadyOpen => println!("tmux window {} already open for {dir}", sys::dir_base(&dir)),
        sys::Tab::NotInTmux => die("not inside tmux: cannot open a tab"),
    }
    let Some(text) = prompt else { return 0 };
    // the new opencode needs a moment to start and register its session
    let wait = paths::env_u64("OC_OPEN_PROJECT_WAIT_SECS", 60);
    for _ in 0..wait {
        if let Some(s) = opencode::list(&dir).into_iter().next() {
            ok(opencode::switch_agent(&s.id, "orchestrator"));
            ok(opencode::prompt(&s.id, &text));
            println!("prompt sent to the orchestrator of {dir} (session {})", s.id);
            return 0;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    die(&format!("no opencode session appeared in {dir} within {wait}s; send the prompt from its tab"))
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
        Some("open-project") => return open_project(&args[1..]),
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
            let max_hops = paths::env_u64("DISPATCH_MAX_HOPS", 40) as i64;
            for t in open {
                let capped = hop_capped(&d, &t.context_id, max_hops);
                println!(
                    "{} {:<14} {} -> {} {} ctx={}{}",
                    &t.id[..8], t.state, t.from_agent, t.to_agent, t.directory.as_deref().unwrap_or("-"), t.context_id,
                    if capped { format!(" HOP-CAPPED (run: oc continue {})", t.context_id) } else { String::new() }
                );
            }
        }
        // anything else (including nothing) is a normal opencode invocation
        _ => return run_opencode(args),
    }
    0
}
