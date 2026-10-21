//! After an upgrade `oc` replaces an older dispatcher (they used to survive and fail on the new
//! schema), never replaces a newer one, and `oc stop` ends every dispatcher of the database.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

struct Sandbox {
    dir: PathBuf,
    fakes: Vec<u32>,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // whatever a failing assertion left behind
        let _ = Command::new(env!("CARGO_BIN_EXE_codetypera2a")).args(["oc", "stop"]).envs(self.env()).output();
        for pid in &self.fakes {
            let _ = Command::new("kill").arg(pid.to_string()).stderr(Stdio::null()).status();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Sandbox {
    fn new(name: &str) -> Sandbox {
        let dir = std::env::temp_dir().join(format!("a2a-upg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // a stand-in for an old dispatcher: its command line is `<dir>/codetypera2a dispatcher`
        let fake = dir.join("codetypera2a");
        std::fs::write(&fake, "#!/bin/sh\nsleep 60\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        Sandbox { dir, fakes: vec![] }
    }

    fn env(&self) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("A2A_DB", self.dir.join("hub.db")),
            ("A2A_DISPATCHER_PID", self.dir.join("d.pid")),
            ("OPENCODE_BIN", PathBuf::from("/usr/bin/false")),
        ]
    }

    fn oc(&self, arg: &str) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_codetypera2a"))
            .args(["oc", arg])
            .envs(self.env())
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Start a fake dispatcher, write it into the pidfile, optionally register it with `version`.
    fn fake_dispatcher(&mut self, version: Option<&str>) -> u32 {
        // started from a throwaway shell so it is reparented to init, like a detached dispatcher
        // (a child of the test process would stay a zombie after being killed and look alive)
        let out = Command::new("sh")
            .args(["-c", "\"$0\" dispatcher >/dev/null 2>&1 & echo $!"])
            .arg(self.dir.join("codetypera2a"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        self.fakes.push(pid);
        std::fs::write(self.dir.join("d.pid"), pid.to_string()).unwrap();
        if let Some(v) = version {
            std::fs::create_dir_all(self.dir.join("d.pid.d")).unwrap();
            std::fs::write(self.dir.join("d.pid.d").join(pid.to_string()), v).unwrap();
        }
        std::thread::sleep(Duration::from_millis(150));
        pid
    }
}

fn alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().unwrap().success()
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..100 {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for: {what}");
}

fn pidfile(dir: &Path) -> Option<u32> {
    std::fs::read_to_string(dir.join("d.pid")).ok()?.trim().parse().ok()
}

#[test]
fn an_older_dispatcher_is_replaced_by_the_current_one_and_oc_stop_ends_it() {
    let mut sb = Sandbox::new("older");
    let old = sb.fake_dispatcher(None); // an old build: no registry entry
    assert!(alive(old));
    sb.oc("ensure");
    wait_until("the old dispatcher is gone", || !alive(old));
    wait_until("a new dispatcher owns the pidfile", || pidfile(&sb.dir).is_some_and(|p| p != old && alive(p)));
    let new = pidfile(&sb.dir).unwrap();
    wait_until("it registered its version", || {
        std::fs::read_to_string(sb.dir.join("d.pid.d").join(new.to_string())).is_ok_and(|v| v.trim() == env!("CARGO_PKG_VERSION"))
    });
    assert!(sb.oc("ensure").contains("already running"), "the current one is left alone");
    assert_eq!(pidfile(&sb.dir), Some(new));
    sb.oc("stop");
    wait_until("oc stop ends it", || !alive(new));
}

#[test]
fn a_newer_dispatcher_is_never_replaced() {
    let mut sb = Sandbox::new("newer");
    let newer = sb.fake_dispatcher(Some("99.0.0"));
    assert!(sb.oc("ensure").contains("already running"));
    assert!(alive(newer));
    assert_eq!(pidfile(&sb.dir), Some(newer));
    sb.oc("stop");
    wait_until("oc stop ends it", || !alive(newer));
}

#[test]
fn a_stopped_dispatcher_stays_stopped_until_opencode_is_started_again() {
    let sb = Sandbox::new("stay");
    std::fs::create_dir_all(sb.dir.join("agents")).unwrap();
    // a hub like the ones opencode keeps running: it supervises the dispatcher every second
    let mut hub = Command::new(env!("CARGO_BIN_EXE_codetypera2a"))
        .arg("hub")
        .envs(sb.env())
        .env("A2A_AGENT_DIR", sb.dir.join("agents"))
        .env("A2A_SUPERVISE_SECS", "1")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the hub starts a dispatcher", || pidfile(&sb.dir).is_some_and(alive));
    let first = pidfile(&sb.dir).unwrap();
    sb.oc("stop");
    wait_until("the dispatcher is gone", || !alive(first));
    std::thread::sleep(Duration::from_millis(3500)); // several supervision rounds
    assert!(pidfile(&sb.dir).is_none_or(|p| !alive(p)), "the supervising hub restarted a stopped dispatcher");
    sb.oc("ensure"); // starting again is explicit
    wait_until("it is started again", || pidfile(&sb.dir).is_some_and(alive));
    sb.oc("stop");
    let _ = hub.kill();
    let _ = hub.wait();
}
