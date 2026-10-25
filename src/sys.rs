//! Thin wrappers over the external tools the hub shells out to: `opencode api`, `tmux`, `ps`, `kill`.
use serde_json::{Value, json};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Single-quote a string for `sh -c`.
pub fn sh(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

/// Run a command, return trimmed stdout; Err carries the first line of stderr.
pub fn run_timeout(cmd: &str, args: &[&str], timeout: Option<Duration>) -> Result<String, String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{cmd}: {e}"))?;
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if timeout.is_some_and(|t| start.elapsed() > t) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{cmd}: timed out"));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(format!("{cmd}: {e}")),
        }
    };
    let (stdout, stderr) = (
        t_out.join().unwrap_or_default(),
        t_err.join().unwrap_or_default(),
    );
    if status.success() {
        Ok(stdout.trim().to_string())
    } else {
        let msg = first_line(&stderr);
        Err(if msg.is_empty() {
            format!("{cmd} exited with {status}")
        } else {
            msg
        })
    }
}

pub fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    run_timeout(cmd, args, None)
}

pub fn tmux(args: &[&str]) -> Result<String, String> {
    run("tmux", args)
}

pub fn tmux_ok(args: &[&str]) -> bool {
    Command::new("tmux")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Like `run_timeout`, but stdout goes through a temp file. `opencode api` cuts large responses
/// short when stdout is a pipe (the agent list is ~330 KB), so a pipe is not safe for it.
fn run_via_file(cmd: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "a2a-api-{}-{}.out",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let file = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let result = (|| {
        let mut child = Command::new(cmd)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(file))
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{cmd}: {e}"))?;
        let mut err = child.stderr.take().unwrap();
        let t_err = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = err.read_to_string(&mut s);
            s
        });
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st,
                Ok(None) => {
                    if start.elapsed() > timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(format!("{cmd}: timed out"));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(format!("{cmd}: {e}")),
            }
        };
        let stderr = t_err.join().unwrap_or_default();
        let stdout = std::fs::read_to_string(&path).unwrap_or_default();
        if status.success() {
            Ok(stdout.trim().to_string())
        } else {
            let msg = first_line(&stderr);
            Err(if msg.is_empty() {
                format!("{cmd} exited with {status}")
            } else {
                msg
            })
        }
    })();
    let _ = std::fs::remove_file(&path);
    result
}

/// `opencode api <method> <route> [-d body]`; non-JSON output comes back as `{"raw": ...}`.
pub fn api(oc: &str, method: &str, route: &str, body: Option<&Value>) -> Result<Value, String> {
    let body_s = body.map(|b| b.to_string());
    let mut args = vec!["api", method, route];
    if let Some(b) = &body_s {
        args.push("-d");
        args.push(b);
    }
    let out = run_via_file(oc, &args, Duration::from_secs(30))?;
    Ok(serde_json::from_str(&out).unwrap_or_else(|_| json!({ "raw": out })))
}

pub fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub fn pid_command(pid: u32) -> Option<String> {
    run("ps", &["-p", &pid.to_string(), "-o", "command="]).ok()
}

pub fn kill_term(pid: u32) {
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
}

/// TERM a whole process group (`pid` leads it): a headless turn and the tools it started.
pub fn kill_group_term(pid: u32) {
    let _ = Command::new("kill")
        .args(["-TERM", "--", &format!("-{pid}")])
        .status();
}

/// The dispatcher process is this binary run as `codetypera2a dispatcher`, or through the `oc`
/// symlink as `oc dispatch` (the `oc` name hands every other word to opencode).
pub fn is_dispatcher_command(cmd: &str) -> bool {
    if cmd.contains("codetypera2a") && cmd.contains("dispatcher") {
        return true;
    }
    let mut words = cmd.split_whitespace();
    let program = words.next().unwrap_or("");
    program.rsplit('/').next() == Some("oc") && words.next() == Some("dispatch")
}

/// Short window-name suffix for a folder: basename, unsafe chars -> `_`, max 24 chars.
pub fn dir_base(dir: &str) -> String {
    let base: String = std::path::Path::new(dir)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(24)
        .collect();
    if base.is_empty() { "root".into() } else { base }
}

/// pid of the running dispatcher recorded in `pid_file`, if that process is really one.
pub fn dispatcher_pid(pid_file: &std::path::Path) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(pid_file)
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|p| *p != 0)?;
    if !pid_alive(pid) {
        return None;
    }
    pid_command(pid)
        .filter(|c| is_dispatcher_command(c))
        .map(|_| pid)
}

/// Percent-encode a query-string value.
pub fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Start `exe args` detached in its own process group, output appended to `log`.
pub fn spawn_detached(
    exe: &std::path::Path,
    args: &[&str],
    log: &std::path::Path,
) -> std::io::Result<u32> {
    use std::os::unix::process::CommandExt;
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?;
    let err = out.try_clone()?;
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0)
        .spawn()?;
    let pid = child.id();
    // a long-lived parent (the hub) must reap it, or a stopped dispatcher stays a zombie that
    // still answers `kill -0`
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Tab {
    Created,
    AlreadyOpen,
    NotInTmux,
}

/// Make sure the tmux session of the current pane has a window (tab) called `name` running `cmd`;
/// a closed one is opened again. The window is created detached, so focus stays where it is.
pub fn tmux_ensure_window(name: &str, cmd: &str) -> Result<Tab, String> {
    let Some(pane) = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty()) else {
        return Ok(Tab::NotInTmux);
    };
    let sess = tmux(&["display-message", "-p", "-t", &pane, "#{session_name}"])?;
    let windows = tmux(&["list-windows", "-t", &sess, "-F", "#{window_name}"])?;
    if windows.lines().any(|w| w == name) {
        return Ok(Tab::AlreadyOpen);
    }
    // Concurrent callers (hub, tui plugin, oc) all see "no window" at once: only the one that
    // takes the lock opens it. A lock older than 20s is stale.
    let lock = std::env::temp_dir().join(format!("a2a-window-{}.lock", name.replace('/', "_")));
    let stale = std::fs::metadata(&lock)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|d| d.as_secs() > 20);
    if stale {
        let _ = std::fs::remove_file(&lock);
    }
    if std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .is_err()
    {
        return Ok(Tab::AlreadyOpen);
    }
    let opened = tmux(&[
        "new-window",
        "-d",
        "-t",
        &format!("{sess}:"),
        "-n",
        name,
        cmd,
    ]);
    // released once the window exists (or failed), so a window closed later can be opened again at once
    let _ = std::fs::remove_file(&lock);
    opened.map(|_| Tab::Created)
}

#[cfg(test)]
mod tests {
    use super::is_dispatcher_command;

    #[test]
    fn dispatcher_is_recognised_under_both_names() {
        assert!(is_dispatcher_command(
            "/opt/homebrew/bin/codetypera2a dispatcher"
        ));
        assert!(is_dispatcher_command("/Users/me/.cargo/bin/oc dispatch"));
        assert!(!is_dispatcher_command("/Users/me/.cargo/bin/oc status"));
        assert!(!is_dispatcher_command("opencode dispatch"));
        assert!(!is_dispatcher_command(
            "/Users/me/.cargo/bin/codetypera2a hub"
        ));
    }
}
