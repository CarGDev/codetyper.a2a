//! `uninstall` / `reinstall` in a throwaway HOME: everything is deleted for good and the reinstall verifies a clean database.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;

use common::{BIN, run, sandbox, text};
#[test]
fn the_dry_run_changes_nothing() {
    let home = sandbox("dry");
    assert!(
        run(
            &home,
            &[
                "init",
                "--yes",
                "--all",
                "--no-models",
                "--no-link",
                "--shell"
            ]
        )
        .status
        .success()
    );
    let before = std::fs::read_dir(home.join(".claude/agents"))
        .unwrap()
        .count();
    let out = run(&home, &["uninstall", "--keep-binary"]);
    assert!(text(&out).contains("DRY RUN"), "{}", text(&out));
    assert_eq!(
        std::fs::read_dir(home.join(".claude/agents"))
            .unwrap()
            .count(),
        before
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn uninstall_deletes_everything_and_reinstall_ends_clean() {
    let home = sandbox("full");
    assert!(
        run(
            &home,
            &[
                "init",
                "--yes",
                "--all",
                "--no-models",
                "--no-link",
                "--shell"
            ]
        )
        .status
        .success()
    );
    std::fs::write(home.join(".claude/agents/mine.md"), "mine").unwrap();
    assert!(
        std::fs::read_to_string(home.join(".zshrc"))
            .unwrap()
            .contains("codetypera2a")
    );

    let out = run(&home, &["uninstall", "--yes", "--keep-binary"]);
    assert!(out.status.success(), "{}", text(&out));
    let gone = |p: &str| !home.join(p).exists();
    assert!(
        gone(".codetypera2a") && gone(".config/opencode/a2a") && gone(".claude/.a2a-manifest.json")
    );
    assert!(gone(".claude/a2a-marketplace") && gone(".config/opencode/plugins/a2a"));
    let leftover = |dir: &str| {
        std::fs::read_dir(home.join(dir))
            .map(|d| d.count())
            .unwrap_or(0)
    };
    assert_eq!(
        std::fs::read_to_string(home.join(".claude/agents/mine.md")).unwrap(),
        "mine",
        "an agent the user wrote stays"
    );
    assert_eq!(
        (leftover(".claude/agents"), leftover(".claude/commands")),
        (1, 0),
        "only our templates are removed"
    );
    assert_eq!(
        (
            leftover(".config/opencode/agents"),
            leftover(".config/opencode/commands")
        ),
        (0, 0)
    );
    assert_eq!(
        std::fs::read_to_string(home.join(".zshrc")).unwrap(),
        "export KEEP=1\n"
    );
    assert!(
        !home.join("codetypera2a-uninstall").exists(),
        "no backup folder"
    );

    let out = run(&home, &["reinstall", "--yes"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("verified"), "{}", text(&out));
    assert!(
        home.join(".codetypera2a/hub.db").exists(),
        "fresh database at the runtime home"
    );
    assert!(
        !home.join(".config/opencode/a2a/hub.db").exists(),
        "no legacy database"
    );
    let _ = std::fs::remove_dir_all(home);
}

fn parent_tmp_file(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("a2a-window-{tag}-{}", std::process::id()));
    std::fs::write(&path, "x").unwrap();
    path
}

#[test]
fn a_window_file_outside_the_sandbox_survives_uninstall() {
    let home = sandbox("window");
    let outside = parent_tmp_file("w9");
    let out = run(&home, &["uninstall", "--yes", "--keep-binary"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(outside.exists(), "a file outside the sandbox was deleted");
    let _ = std::fs::remove_file(outside);
    let _ = std::fs::remove_dir_all(home);
}

const LAUNCH_ATTEMPTS: usize = 3;
const LAUNCH_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);

/// A fake dispatcher process that is terminated when this guard drops, even if the test panics. It only ever
/// signals the pid it launched, and only while that pid still runs its own script.
#[cfg(unix)]
struct FakeDispatcher {
    pid: u32,
    script: String,
}

#[cfg(unix)]
impl Drop for FakeDispatcher {
    fn drop(&mut self) {
        if !command_of(self.pid).contains(&self.script) {
            return;
        }
        let _ = Command::new("kill")
            .arg(self.pid.to_string())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// A fake dispatcher (`codetypera2a dispatcher` that only sleeps), detached from the test so a stopped one
/// is reaped by init like a real daemon. Another test thread forking while the script
/// is open for writing can make the first exec fail with "text file busy", so a launch that does not
/// show up is repeated.
#[cfg(unix)]
fn fake_dispatcher(home: &Path) -> FakeDispatcher {
    use std::os::unix::fs::PermissionsExt;
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("codetypera2a");
    if !script.exists() {
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let launch = format!(
        "{} dispatcher >/dev/null 2>&1 </dev/null & echo $!",
        script.display()
    );
    for _ in 0..LAUNCH_ATTEMPTS {
        let out = Command::new("/bin/sh")
            .args(["-c", &launch])
            .output()
            .unwrap();
        let pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        if wait_for(LAUNCH_TIMEOUT, || {
            command_of(pid).contains("codetypera2a dispatcher")
        }) {
            return FakeDispatcher {
                pid,
                script: script.display().to_string(),
            };
        }
    }
    panic!("the fake dispatcher did not start after {LAUNCH_ATTEMPTS} attempts");
}

/// How long a process that must survive is watched.
const SURVIVAL_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);
const POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Checks `condition` every 20 ms for up to 5 s; true as soon as it holds.
fn wait_until(condition: impl Fn() -> bool) -> bool {
    wait_for(POLL_TIMEOUT, condition)
}

fn wait_for(timeout: std::time::Duration, condition: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    condition()
}

#[cfg(unix)]
fn command_of(pid: u32) -> String {
    Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default()
}

#[cfg(unix)]
fn is_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
#[test]
fn a_dispatcher_of_another_home_survives_and_reinstall_is_verified() {
    let home = sandbox("foreign");
    let foreign = fake_dispatcher(&home);
    let out = run(&home, &["uninstall", "--yes", "--keep-binary"]);
    let survived = !wait_for(SURVIVAL_WINDOW, || !is_alive(foreign.pid));
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        survived,
        "the foreign dispatcher was stopped: {}",
        text(&out)
    );
    let out = run(&home, &["reinstall", "--yes"]);
    let still_alive = is_alive(foreign.pid);
    assert!(still_alive, "{}", text(&out));
    assert!(text(&out).contains("verified"), "{}", text(&out));
    assert!(!text(&out).contains("NOT CLEAN"), "{}", text(&out));
    let _ = std::fs::remove_dir_all(home);
}

#[cfg(unix)]
#[test]
fn the_dispatcher_claimed_by_this_home_is_stopped() {
    let home = sandbox("claimed");
    let ours = fake_dispatcher(&home);
    let runtime = home.join(".codetypera2a");
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::write(runtime.join("dispatcher.pid"), ours.pid.to_string()).unwrap();
    let out = run(&home, &["uninstall", "--yes", "--keep-binary"]);
    let stopped = wait_until(|| !is_alive(ours.pid));
    assert!(out.status.success(), "{}", text(&out));
    assert!(stopped, "{}", text(&out));
    assert!(text(&out).contains("stopped"), "{}", text(&out));
    let _ = std::fs::remove_dir_all(home);
}

/// `oc <args>` in the sandbox with a `pkill` that only records that it was called, so a regression can
/// never signal a real process, and a `tmux` that does nothing.
#[cfg(unix)]
fn oc_with_recording_pkill(home: &Path, args: &[&str]) -> Output {
    use std::os::unix::fs::PermissionsExt;
    let shim = home.join("shim");
    std::fs::create_dir_all(&shim).unwrap();
    let pkill = shim.join("pkill");
    let log = home.join("pkill.log");
    std::fs::write(
        &pkill,
        format!("#!/bin/sh\necho \"$@\" >> {}\n", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&pkill, std::fs::Permissions::from_mode(0o755)).unwrap();
    let tmux = shim.join("tmux");
    std::fs::write(&tmux, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    let tmp = home.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    Command::new(BIN)
        .arg("oc")
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("SHELL", "/bin/zsh")
        .env("OPENCODE_BIN", "/usr/bin/false")
        .env("PATH", format!("{}:/usr/bin:/bin", shim.display()))
        .env("TMUX_PANE", "%0")
        .env("TMPDIR", &tmp)
        .output()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn a_dispatcher_of_another_home_survives_ensure_running() {
    let home = sandbox("ensure");
    let foreign = fake_dispatcher(&home);
    let owner = fake_dispatcher(&home);
    let runtime = home.join(".codetypera2a");
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::write(runtime.join("dispatcher.pid"), owner.pid.to_string()).unwrap();
    let out = oc_with_recording_pkill(&home, &["ensure"]);
    let foreign_alive = is_alive(foreign.pid);
    let owner_replaced = wait_until(|| !is_alive(owner.pid));
    let name_kill = std::fs::read_to_string(home.join("pkill.log")).unwrap_or_default();
    oc_with_recording_pkill(&home, &["stop"]);
    assert!(foreign_alive, "{}", text(&out));
    assert!(
        owner_replaced,
        "the replaced pidfile owner is still running"
    );
    assert!(name_kill.is_empty(), "a name-pattern kill ran: {name_kill}");
    let _ = std::fs::remove_dir_all(home);
}

#[cfg(unix)]
#[test]
fn a_registered_stray_of_this_home_is_stopped_on_start() {
    let home = sandbox("stray");
    let stray = fake_dispatcher(&home);
    let registry = home.join(".codetypera2a/dispatcher.pid.d");
    std::fs::create_dir_all(&registry).unwrap();
    std::fs::write(registry.join(stray.pid.to_string()), "2.2.0").unwrap();
    let out = oc_with_recording_pkill(&home, &["ensure"]);
    let stopped = wait_until(|| !is_alive(stray.pid));
    oc_with_recording_pkill(&home, &["stop"]);
    assert!(stopped, "{}", text(&out));
    let _ = std::fs::remove_dir_all(home);
}

const SLOW_PS_DELAY_SECONDS: u32 = 3;

/// `uninstall --yes` with a `ps` that answers correctly but returns only after a delay, as one does on a
/// machine under heavy load.
#[cfg(unix)]
fn uninstall_with_slow_ps(home: &Path) -> Output {
    use std::os::unix::fs::PermissionsExt;
    let shim = home.join("slow-ps");
    std::fs::create_dir_all(&shim).unwrap();
    let ps = shim.join("ps");
    let body = format!("#!/bin/sh\n/bin/ps \"$@\"\nsleep {SLOW_PS_DELAY_SECONDS}\n");
    std::fs::write(&ps, body).unwrap();
    std::fs::set_permissions(&ps, std::fs::Permissions::from_mode(0o755)).unwrap();
    let tmp = home.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    Command::new(BIN)
        .args(["uninstall", "--yes", "--keep-binary"])
        .env_clear()
        .env("HOME", home)
        .env("SHELL", "/bin/zsh")
        .env("OPENCODE_BIN", "/usr/bin/false")
        .env("PATH", format!("{}:/usr/bin:/bin", shim.display()))
        .env("TMPDIR", &tmp)
        .output()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn a_slow_process_listing_does_not_make_a_claimed_dispatcher_stale() {
    let home = sandbox("slow-ps");
    let ours = fake_dispatcher(&home);
    let runtime = home.join(".codetypera2a");
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::write(runtime.join("dispatcher.pid"), ours.pid.to_string()).unwrap();
    let out = uninstall_with_slow_ps(&home);
    let stopped = wait_until(|| !is_alive(ours.pid));
    assert!(stopped, "{}", text(&out));
    assert!(text(&out).contains("stopped"), "{}", text(&out));
    assert!(!text(&out).contains("stale"), "{}", text(&out));
    let _ = std::fs::remove_dir_all(home);
}

#[cfg(unix)]
#[test]
fn a_fake_dispatcher_is_killed_when_its_guard_drops() {
    let home = sandbox("guard");
    let fake = fake_dispatcher(&home);
    let pid = fake.pid;
    assert!(is_alive(pid));
    drop(fake);
    assert!(
        wait_until(|| !is_alive(pid)),
        "the guard left {pid} running"
    );
    let _ = std::fs::remove_dir_all(home);
}
