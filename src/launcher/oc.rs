//! oc: run OpenCode with the A2A dispatcher alongside it.
//!
//!   oc [opencode args]  start the dispatcher in the background (if not running), then run opencode
//!                       right here, in this terminal / tmux pane. Nothing new is opened.
//!   oc dispatch         run the dispatcher in THIS pane, in the foreground
//!   oc agent <name>     switch this folder's newest opencode session to another agent
//!   oc watch            follow the dispatcher log live (what the tmux `a2a-status` window runs)
//!   oc status           dispatcher state, running sessions and open tasks
//!   oc stop             stop the dispatcher
//!   oc trace <task>     per-task event timeline;  oc stats [--model]  per-agent / per-model summary
//!   oc open-project <dir> [--prompt <text>]
//!                       open <dir> in its own tmux tab running `oc`; with --prompt, hand the text to
//!                       that tab's orchestrator
//!   oc cancel <--all | task... | --context <ctx> | --dir <folder>> [--no-interrupt] [--yes]
//!                       cancel open tasks and stop the turns that are working on them
//!   oc shutdown [--force]
//!                       end this setup: cancel open tasks, interrupt running turns, retire the
//!                       sessions, stop the dispatcher and close the status tab. Runs by itself when
//!                       the last opencode started through `oc` exits (A2A_KEEP_RUNNING=1 disables)
//!   oc status --json [--dir <folder>]
//!                       the same as JSON (tasks, questions, progress, today's cost) for the panel plugin
//!   oc messages --json [--since <iso>] [--limit N]
//!                       the latest task messages, newest first, read-only (for the Claude Code pane)
//!   oc answer <task-id> <text>
//!                       reply to a task that waits for you (input-required)
//!   oc continue <ctx>   allow another DISPATCH_MAX_HOPS hops for a context that hit the hop cap
//!
//! ONE dispatcher serves all projects (it reads the folder from each task). It is single-instance
//! via a pidfile (<A2A_DB>.dispatcher.pid), so `oc` in any folder reuses it. All agents run inside
//! the opencode session you already have open: the dispatcher switches that session to the
//! assignee and back to the orchestrator; no session is ever created for an agent.
//! Env: A2A_DB, OPENCODE_BIN, DISPATCH_MAX_HOPS
use crate::dispatcher::backend;
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
  oc cancel --all       cancel every open task and interrupt the turns working on them
  oc cancel <task>...   cancel the given tasks (id or 8-char prefix); also --context <ctx>,
                        --dir <folder>; --no-interrupt keeps running turns going; --yes skips asking
  oc shutdown           cancel open tasks, stop running turns and the dispatcher (automatic when
                        the last opencode started with `oc` exits; --force ignores other opencodes)
  oc status --json      the same as JSON (tasks, questions, progress, today's cost); --dir <folder>
  oc messages --json    the latest task messages, newest first; --since <iso>, --limit N (default 50)
  oc answer <task> <text>
                        reply to a task that waits for you (input-required)
  oc continue <ctx>     allow more hops for a context that hit the hop cap
  oc trace <task-id>    timeline of one task (id or 8-char prefix): events, offsets, duration
  oc stats [--model]    per-agent summary: tasks, nudges, average duration, tokens/cost;
                        --model groups the same usage per model, one table per tool (opencode,
                        claude code; reads new Claude Code transcripts first);
                        --ingest only reads the Claude Code transcripts and prints the new rows
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
    std::env::current_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|_| ".".into())
}

fn ensure_dispatcher() {
    match crate::dispatcher::dispatcher::ensure_running() {
        Ok(Some(pid)) => println!("dispatcher started in the background (pid {pid})"),
        Ok(None) => println!("dispatcher already running; serving all projects"),
        Err(e) => die(&format!("cannot start the dispatcher: {e}")),
    }
    match crate::dispatcher::dispatcher::ensure_status_window() {
        Ok(sys::Tab::Created) => {
            println!("live status opened in a new tmux window (tab): a2a-status")
        }
        Ok(sys::Tab::AlreadyOpen) => {
            println!("live status is already open in tmux window a2a-status")
        }
        Ok(sys::Tab::NotInTmux) => {
            println!("not inside tmux: run `oc watch` in another terminal for the live status")
        }
        Err(e) => eprintln!("oc: could not open the tmux status window: {e}"),
    }
}

/// Live view: the current status, then follow the dispatcher log (what the tmux window runs).
fn watch() -> i32 {
    use std::io::{Read, Seek, SeekFrom};
    let log = crate::dispatcher::dispatcher::log_path();
    println!("a2a live status  (log: {})", log.display());
    if let Ok(d) = db::open(&paths::db_path()) {
        if let Ok(open) = db::tasks(
            &d,
            "WHERE state IN ('submitted','working','input-required','auth-required') ORDER BY updated_at DESC LIMIT 10",
            &[],
        ) {
            println!("open tasks: {}", open.len());
            for t in open {
                println!(
                    "  {} {:<14} {} -> {}",
                    &t.id[..8],
                    t.state,
                    t.from_agent,
                    t.to_agent
                );
            }
        }
    }
    println!("{}", "-".repeat(60));
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(&log)
    else {
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
pub(crate) fn hop_capped(d: &rusqlite::Connection, ctx: &str, max_hops: i64) -> bool {
    let n: i64 = d
        .query_row("SELECT COUNT(*) FROM nudges WHERE context_id = ? AND kind IN ('assignee','requester') AND status IN ('claimed','delivered')", [ctx], |r| r.get(0))
        .unwrap_or(0);
    let grants: i64 = d
        .query_row(
            "SELECT grants FROM hop_allow WHERE context_id = ?",
            [ctx],
            |r| r.get(0),
        )
        .unwrap_or(0);
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
    let Some(dir) = dir else {
        die("usage: oc open-project <dir> [--prompt <text>]")
    };
    let dir = std::fs::canonicalize(&dir)
        .unwrap_or_else(|_| die(&format!("no such folder: {dir}")))
        .display()
        .to_string();
    let cmd = format!("cd '{}' && oc", dir.replace('\'', "'\\''"));
    match ok(sys::tmux_ensure_window(&sys::dir_base(&dir), &cmd)) {
        sys::Tab::Created => println!("opened tmux window {} for {dir}", sys::dir_base(&dir)),
        sys::Tab::AlreadyOpen => {
            println!("tmux window {} already open for {dir}", sys::dir_base(&dir))
        }
        sys::Tab::NotInTmux => die("not inside tmux: cannot open a tab"),
    }
    let Some(text) = prompt else { return 0 };
    // the new opencode needs a moment to start and register its session
    let wait = paths::env_u64("OC_OPEN_PROJECT_WAIT_SECS", 60);
    for _ in 0..wait {
        if let Some(s) = opencode::list(&dir).into_iter().next() {
            ok(opencode::switch_agent(&s.id, "orchestrator"));
            ok(opencode::prompt(&s.id, &text));
            println!(
                "prompt sent to the orchestrator of {dir} (session {})",
                s.id
            );
            return 0;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    die(&format!(
        "no opencode session appeared in {dir} within {wait}s; send the prompt from its tab"
    ))
}

const OPEN_STATES: &str =
    "WHERE state IN ('submitted','working','input-required','auth-required') ORDER BY created_at";

/// `oc cancel`: cancel open tasks from the terminal and stop the turns that work on them.
fn cancel(args: &[String]) -> i32 {
    cancel_inner(args).0
}

/// Returns the exit code and the sessions that were interrupted.
fn cancel_inner(args: &[String]) -> (i32, std::collections::BTreeSet<String>) {
    const USAGE: &str = "usage: oc cancel <--all | task-id... | --context <ctx> | --dir <folder>> [--no-interrupt] [--yes]";
    let (mut all, mut interrupt, mut yes) = (false, true, false);
    let (mut ids, mut ctx, mut dir) = (Vec::<String>::new(), None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--all" => all = true,
            "--no-interrupt" => interrupt = false,
            "--yes" | "-y" => yes = true,
            "--context" => ctx = it.next().cloned(),
            "--dir" => dir = it.next().cloned(),
            id if !id.starts_with('-') => ids.push(id.to_string()),
            _ => die(USAGE),
        }
    }
    if !all && ids.is_empty() && ctx.is_none() && dir.is_none() {
        die(USAGE);
    }
    let d = ok(db::open(&paths::db_path()));
    let dir = dir.map(|x| {
        std::fs::canonicalize(&x)
            .map(|p| p.display().to_string())
            .unwrap_or(x)
    });
    let tasks: Vec<_> = ok(db::tasks(&d, OPEN_STATES, &[]))
        .into_iter()
        .filter(|t| {
            all || ids.iter().any(|i| t.id.starts_with(i.as_str()))
                || ctx.as_deref() == Some(t.context_id.as_str())
                || dir.as_deref().is_some_and(|x| {
                    [t.directory.as_deref(), t.from_directory.as_deref()]
                        .into_iter()
                        .flatten()
                        .any(|td| crate::hub::project::same_project(x, td))
                })
        })
        .collect();
    if tasks.is_empty() {
        println!("no open tasks to cancel");
        return (0, Default::default());
    }
    for t in &tasks {
        println!(
            "{} {:<14} {} -> {} ctx={}",
            &t.id[..8.min(t.id.len())],
            t.state,
            t.from_agent,
            t.to_agent,
            t.context_id
        );
    }
    if !yes && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        use std::io::Write;
        print!("Cancel {} task(s)? [y/N] ", tasks.len());
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        if !line.trim().eq_ignore_ascii_case("y") {
            println!("nothing canceled");
            return (0, Default::default());
        }
    }
    ok(d.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| e.to_string()));
    let mut canceled = vec![];
    for t in &tasks {
        let ts = db::now();
        let msg = serde_json::json!({
            "messageId": uuid::Uuid::new_v4().to_string(), "role": "agent", "from": "user",
            "parts": [{ "text": "canceled by user (oc cancel)" }], "taskId": t.id,
        });
        let mut history: serde_json::Value =
            serde_json::from_str(&t.history).unwrap_or(serde_json::json!([]));
        if let Some(h) = history.as_array_mut() {
            h.push(msg.clone());
        }
        let n = d
            .execute(
                "UPDATE tasks SET state='canceled', status_message=?, history=?, updated_at=?, completed_at=?, version=version+1
                 WHERE id=? AND state IN ('submitted','working','input-required','auth-required')",
                rusqlite::params![msg.to_string(), history.to_string(), ts, ts, t.id],
            )
            .unwrap_or(0);
        if n == 1 {
            db::record_event(
                &d,
                Some(&t.id),
                Some(&t.context_id),
                "state_change",
                Some("user"),
                Some("canceled"),
            );
            // nothing may still be delivered for a canceled task
            let _ = d.execute(
                "UPDATE nudges SET status='dead', last_error='task canceled', updated_at=?, claimed_by=NULL, claim_expires_at=NULL
                 WHERE task_id=? AND status IN ('pending','claimed')",
                rusqlite::params![ts, t.id],
            );
            canceled.push(t);
        }
    }
    ok(d.execute_batch("COMMIT").map_err(|e| e.to_string()));
    println!("canceled {} task(s)", canceled.len());
    if !interrupt {
        return (0, Default::default());
    }
    // stop the turns that are running right now for those tasks (they would keep spending tokens)
    let running = backend::running_everywhere();
    let mut stopped = std::collections::BTreeSet::new(); // sessions interrupted (once each)
    for t in &canceled {
        let Some(dir) = t.directory.as_deref().or(t.from_directory.as_deref()) else {
            continue;
        };
        let rows = db::sessions(&d, "WHERE agent = ? AND status = 'live'", &[&t.to_agent])
            .unwrap_or_default();
        for r in rows
            .iter()
            .filter(|r| r.directory == dir || crate::hub::project::same_project(dir, &r.directory))
        {
            if !running.contains(&r.session_id) || !stopped.insert(r.session_id.clone()) {
                continue;
            }
            let Some(runtime) = backend::for_stored(r.backend.as_deref()) else {
                continue;
            };
            match runtime.interrupt(&r.session_id) {
                Ok(()) => println!("interrupted {} (session {})", t.to_agent, r.session_id),
                Err(e) => {
                    eprintln!("could not interrupt session {}: {e}", r.session_id);
                    if e.contains("404") {
                        let _ = d.execute(
                            "UPDATE sessions SET status='stale' WHERE session_id = ?",
                            [&r.session_id],
                        );
                    }
                }
            }
        }
    }
    (0, stopped)
}

fn clients_dir() -> std::path::PathBuf {
    let mut p = paths::dispatcher_pid_path().into_os_string();
    p.push(".clients");
    std::path::PathBuf::from(p)
}

/// Remember that an opencode (pid) is running against this setup.
fn register_client(pid: u32) {
    let dir = clients_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(pid.to_string()), "");
}

/// pids of registered opencodes that are still alive (dead ones are forgotten).
fn live_clients() -> Vec<u32> {
    let Ok(rd) = std::fs::read_dir(clients_dir()) else {
        return vec![];
    };
    let mut out = vec![];
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        if sys::pid_alive(pid) {
            out.push(pid);
        } else {
            let _ = std::fs::remove_file(e.path());
        }
    }
    out
}

/// An opencode that was registered (pid `me`) is done: when no other one is left, end everything
/// this setup started. `force` ends it regardless of other opencodes.
fn shutdown_if_last(me: u32, force: bool) -> i32 {
    if me != 0 {
        let _ = std::fs::remove_file(clients_dir().join(me.to_string()));
    }
    if std::env::var("A2A_KEEP_RUNNING").as_deref() == Ok("1") {
        return 0;
    }
    let others: Vec<u32> = live_clients().into_iter().filter(|p| *p != me).collect();
    // silent: this runs on every opencode exit and its output can land in the dispatcher log
    if !force && !others.is_empty() {
        return 0;
    }
    shutdown()
}

/// Cancel open tasks, interrupt every running turn, retire the sessions, stop the dispatcher and
/// close the live-status tab.
fn shutdown() -> i32 {
    // open tasks and the turns working on them
    let (_, interrupted) = cancel_inner(&["--all".to_string(), "--yes".to_string()]);
    if let Ok(d) = db::open(&paths::db_path()) {
        // any other turn of a session this setup opened is still spending tokens
        let running = backend::running_everywhere();
        for r in db::sessions(&d, "WHERE status = 'live'", &[]).unwrap_or_default() {
            let Some(runtime) = backend::for_stored(r.backend.as_deref()) else {
                continue;
            };
            if running.contains(&r.session_id) && !interrupted.contains(&r.session_id) {
                match runtime.interrupt(&r.session_id) {
                    Ok(()) => println!("interrupted {} (session {})", r.agent, r.session_id),
                    Err(e) => eprintln!("could not interrupt session {}: {e}", r.session_id),
                }
            }
        }
        let _ = d.execute(
            "UPDATE sessions SET status='ended', ended_at=? WHERE status='live'",
            [db::now()],
        );
    }
    crate::dispatcher::dispatcher::stop_all();
    crate::dispatcher::dispatcher::sweep_unregistered();
    let _ = sys::tmux(&["kill-window", "-t", "a2a-status"]);
    println!(
        "a2a stopped: tasks canceled, turns interrupted, sessions retired, dispatcher stopped"
    );
    0
}

/// Subcommands that never open the TUI: they run as plain opencode, with or without tmux.
const PLAIN_SUBCOMMANDS: &[&str] = &[
    "upgrade",
    "update",
    "uninstall",
    "acp",
    "api",
    "debug",
    "auth",
    "mcp",
    "plugin",
    "models",
    "stats",
    "run",
    "session",
    "service",
    "reload",
    "pair",
    "serve",
    "mini",
];

/// Why a2a cannot run here, `None` when tmux is installed and this pane is inside it.
/// `A2A_NO_TMUX_CHECK=1` skips the check (the integration tests run without tmux).
fn tmux_problem(in_pane: bool, installed: bool) -> Option<&'static str> {
    if std::env::var("A2A_NO_TMUX_CHECK").as_deref() == Ok("1") {
        return None;
    }
    if !installed {
        return Some(
            "tmux is not installed (a2a opens one tab per agent in tmux); install it, e.g. `brew install tmux`",
        );
    }
    if !in_pane {
        return Some(
            "not inside tmux (a2a opens one tab per agent in tmux); start it with `tmux`, then run oc again",
        );
    }
    None
}

/// opencode's own flags and plain subcommands do not need a2a, so they do not need tmux.
fn needs_tmux(args: &[String]) -> bool {
    let first = args.first().map(String::as_str).unwrap_or("");
    let plain = first.starts_with('-')
        && !args
            .iter()
            .any(|a| a == "-c" || a == "-s" || a == "--continue" || a == "--session");
    !(plain || PLAIN_SUBCOMMANDS.contains(&first))
}

fn run_opencode(args: &[String]) -> i32 {
    let a2a = needs_tmux(args);
    if a2a {
        let in_pane = std::env::var("TMUX_PANE").is_ok_and(|p| !p.is_empty());
        if let Some(why) = tmux_problem(in_pane, sys::tmux_ok(&["-V"])) {
            eprintln!("oc: {why}");
            return 1;
        }
        ensure_dispatcher();
    }
    let me = std::process::id();
    a2a.then(|| register_client(me));
    let code = Command::new(opencode::bin())
        .args(args)
        .stdin(Stdio::inherit())
        .status()
        .ok()
        .and_then(|s| s.code())
        .unwrap_or(1);
    a2a.then(|| shutdown_if_last(me, false));
    code
}

pub fn run(args: &[String]) -> i32 {
    // Claude Code's footer and hooks call this on every refresh: no heal, no output but the line.
    if args.first().map(String::as_str) == Some("statusline") {
        return super::statusline::run(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("follow") {
        let Some(file) = args.get(1) else {
            die("usage: oc follow <log file>")
        };
        return crate::dispatcher::claude_backend::follow(std::path::Path::new(file));
    }
    let (cmd, arg) = (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    );
    if !matches!(
        cmd,
        Some("help" | "--help" | "-h" | "version" | "--version" | "-V")
    ) {
        if let Some(msg) = crate::cli::heal(&paths::config_dir(), &crate::cli::stable_exe()) {
            eprintln!("oc: {msg}");
        }
    }
    match cmd {
        // oc's own help, with no side effects (nothing is started or opened)
        Some("help" | "--help" | "-h") => print!("{OC_HELP}"),
        Some("version" | "--version" | "-V") => println!("oc {}", env!("CARGO_PKG_VERSION")),
        // dispatcher + live status tab, without starting opencode (what the tabs plugin runs)
        Some("ensure") => {
            let in_pane = std::env::var("TMUX_PANE").is_ok_and(|p| !p.is_empty());
            if let Some(why) = tmux_problem(in_pane, sys::tmux_ok(&["-V"])) {
                eprintln!("oc: {why}");
                return 1;
            }
            // `--client <pid>`: an opencode (the tabs plugin's TUI) that counts as running
            if let (Some("--client"), Some(pid)) =
                (arg, args.get(2).and_then(|p| p.parse::<u32>().ok()))
            {
                register_client(pid);
            }
            ensure_dispatcher()
        }
        Some("shutdown") => {
            let force = args[1..].iter().any(|a| a == "--force");
            let client = args
                .iter()
                .position(|a| a == "--client")
                .and_then(|i| args.get(i + 1))
                .and_then(|p| p.parse().ok());
            return shutdown_if_last(client.unwrap_or(0), force);
        }
        Some("dispatch") => return crate::dispatcher::dispatcher::run(),
        Some("watch") => return watch(),
        Some("agent" | "open") => {
            let Some(agent) = arg else {
                die("usage: oc agent <name>")
            };
            let dir = cwd();
            let Some(s) = opencode::list(&dir).into_iter().next() else {
                die(&format!(
                    "no opencode session open in {dir}; start one with: oc"
                ))
            };
            ok(opencode::switch_agent(&s.id, agent));
            println!("session {} -> agent {agent}", s.id);
        }
        Some("stop") => {
            // every dispatcher of this database, not just the pidfile owner
            crate::dispatcher::dispatcher::stop_all();
            crate::dispatcher::dispatcher::sweep_unregistered();
            println!("dispatcher stopped");
        }
        Some("continue") => {
            let Some(ctx) = arg else {
                die("usage: oc continue <contextId>")
            };
            let max_hops = paths::env_u64("DISPATCH_MAX_HOPS", 40) as i64;
            let d = ok(db::open(&paths::db_path()));
            ok(db::ensure_context(&d, ctx).map_err(|e| e.to_string()));
            ok(d.execute(
                "INSERT INTO hop_allow (context_id, grants, updated_at) VALUES (?, 1, strftime('%Y-%m-%dT%H:%M:%fZ','now')) ON CONFLICT(context_id) DO UPDATE SET grants = grants + 1, updated_at = excluded.updated_at",
                [ctx],
            )
            .map_err(|e| e.to_string()));
            let g: i64 = ok(d
                .query_row(
                    "SELECT grants FROM hop_allow WHERE context_id = ?",
                    [ctx],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string()));
            println!(
                "context {ctx}: allowance is now {} hops (+{max_hops})",
                max_hops * (1 + g)
            );
        }
        Some("open-project") => return open_project(&args[1..]),
        Some("cancel") => return cancel(&args[1..]),
        Some("trace") => {
            let Some(id) = arg else {
                die("usage: oc trace <task-id-or-prefix>")
            };
            let d = ok(db::open_read_compat(&paths::db_path()).map(|(d, _)| d));
            let t = ok(crate::trace::find_task(&d, id));
            let evs = ok(crate::trace::events_for(&d, &t.id));
            print!("{}", crate::trace::format_trace(&t, &evs));
        }
        Some("stats") => {
            let d = ok(if args.iter().any(|a| a == "--ingest") {
                db::open(&paths::db_path())
            } else {
                db::open_read_compat(&paths::db_path()).map(|(d, _)| d)
            });
            let colour = std::io::IsTerminal::is_terminal(&std::io::stdout())
                && std::env::var_os("NO_COLOR").is_none();
            let by_model = args.iter().any(|a| a == "--model" || a == "--models");
            if args.iter().any(|a| a == "--ingest") {
                match crate::launcher::claude_usage::ingest(&d, &paths::claude_dir()) {
                    Ok(n) => println!("claude code: {n} new usage rows"),
                    Err(e) => die(&format!("stats --ingest: {e}")),
                }
                return 0;
            }
            // a failed ingest never breaks the stats: the rows stored so far are still shown
            // Read-only stats must not ingest or mutate usage state implicitly.
            let table = match by_model {
                true => {
                    crate::stats_view::render_models(&ok(crate::trace::model_stats(&d)), colour)
                }
                false => crate::stats_view::render(&ok(crate::trace::agent_stats(&d)), colour),
            };
            print!("{table}");
        }
        Some("status") if args.iter().any(|a| a == "--json") => {
            println!("{}", ok(crate::launcher::panel::status_json(&args[1..])));
        }
        Some("messages") if args.iter().any(|a| a == "--json") => {
            println!("{}", ok(crate::launcher::panel::messages_json(&args[1..])));
        }
        Some("answer") => println!("{}", ok(crate::launcher::panel::answer(&args[1..]))),
        Some("status") => {
            let owner = paths::resolve_runtime_home(&paths::runtime_home(), &paths::legacy_home());
            let running = sys::dispatcher_pid(&paths::runtime_paths(&owner).pid).is_some();
            println!(
                "dispatcher: {}",
                if running { "running" } else { "not running" }
            );
            if let Ok(ids) = opencode::active_ids() {
                println!("sessions running a turn: {}", ids.len());
            }
            let claude_turns = backend::for_stored(Some("claude"))
                .and_then(|b| b.active_ids().ok())
                .map_or(0, |ids| ids.len());
            if claude_turns > 0 {
                println!("claude turns running: {claude_turns}");
            }
            let d = ok(db::open_read_compat(&paths::db_path()).map(|(d, _)| d));
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
                    &t.id[..8],
                    t.state,
                    t.from_agent,
                    t.to_agent,
                    t.directory.as_deref().unwrap_or("-"),
                    t.context_id,
                    if capped {
                        format!(" HOP-CAPPED (run: oc continue {})", t.context_id)
                    } else {
                        String::new()
                    }
                );
            }
        }
        // anything else (including nothing) is a normal opencode invocation
        _ => return run_opencode(args),
    }
    0
}

#[cfg(test)]
mod tmux_tests {
    use super::{needs_tmux, tmux_problem};

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a2a_needs_tmux_installed_and_a_pane() {
        assert!(tmux_problem(true, false).is_some_and(|m| m.contains("not installed")));
        assert!(tmux_problem(false, true).is_some_and(|m| m.contains("not inside tmux")));
        assert_eq!(tmux_problem(true, true), None);
    }

    #[test]
    fn plain_opencode_commands_do_not_need_tmux() {
        assert!(needs_tmux(&v(&[])));
        assert!(needs_tmux(&v(&["/some/folder"])));
        assert!(needs_tmux(&v(&["-c"])));
        assert!(needs_tmux(&v(&["--session", "ses_1"])));
        assert!(!needs_tmux(&v(&["models"])));
        assert!(!needs_tmux(&v(&["run", "hi"])));
        assert!(!needs_tmux(&v(&["--version"])));
    }
}
