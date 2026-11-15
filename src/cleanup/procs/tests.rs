use super::*;
use std::time::UNIX_EPOCH;

const DISPATCHER: &str = "/h/.cargo/bin/codetypera2a dispatcher";

fn at(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

fn proc_at(pid: u32, started: u64, command: &str) -> Proc {
    Proc {
        pid,
        command: command.into(),
        started: at(started),
    }
}

fn claim_at(pid: u32, file: &str, written: u64) -> Claim {
    Claim {
        pid,
        file: file.into(),
        at: at(written),
    }
}

#[test]
fn a_dispatcher_without_a_claim_is_foreign() {
    let scoped = scope(&[], &[proc_at(7, 100, DISPATCHER)], &|_| false);
    assert!(scoped.ours.is_empty() && scoped.stale.is_empty());
    assert_eq!(scoped.foreign, 1);
}

#[test]
fn a_claimed_dispatcher_started_before_its_claim_is_ours() {
    let scoped = scope(
        &[claim_at(7, "/h/dispatcher.pid", 105)],
        &[proc_at(7, 100, DISPATCHER)],
        &|_| true,
    );
    assert_eq!(scoped.ours.len(), 1);
    assert_eq!(scoped.ours[0].0.pid, 7);
    assert_eq!(scoped.ours[0].1, at(105));
    assert_eq!(scoped.foreign, 0);
}

#[test]
fn a_reused_pid_that_started_after_the_claim_is_stale() {
    let scoped = scope(
        &[claim_at(7, "/h/dispatcher.pid", 100)],
        &[proc_at(7, 110, DISPATCHER)],
        &|_| true,
    );
    assert!(scoped.ours.is_empty());
    assert_eq!(
        scoped.stale,
        vec![Stale {
            file: PathBuf::from("/h/dispatcher.pid"),
            pid: Some(7),
            reason: StaleReason::StartedAfterClaim
        }]
    );
    assert_eq!(scoped.foreign, 1, "the live dispatcher is not ours");
}

#[test]
fn a_start_within_the_slack_still_counts_as_ours() {
    assert!(claims_owned(
        &claim_at(7, "f", 100),
        &proc_at(7, 100 + START_SLACK.as_secs(), DISPATCHER)
    ));
    assert!(!claims_owned(
        &claim_at(7, "f", 100),
        &proc_at(7, 101 + START_SLACK.as_secs(), DISPATCHER)
    ));
}

#[test]
fn a_dead_or_non_dispatcher_claim_is_stale() {
    let claims = [
        claim_at(7, "/h/dead.pid", 100),
        claim_at(8, "/h/vim.pid", 100),
    ];
    let scoped = scope(&claims, &[proc_at(8, 50, "vim notes.md")], &|pid| pid == 8);
    assert!(scoped.ours.is_empty());
    assert_eq!(scoped.stale.len(), 2);
}

#[test]
fn a_stale_claim_names_why_it_is_stale() {
    let claim = [claim_at(7, "/h/dispatcher.pid", 100)];
    let why = |procs: &[Proc], alive: bool| scope(&claim, procs, &|_| alive).stale[0].reason;
    assert_eq!(why(&[], false), StaleReason::Gone);
    assert_eq!(why(&[], true), StaleReason::NotDispatcher);
    assert_eq!(
        why(&[proc_at(7, 500, DISPATCHER)], true),
        StaleReason::StartedAfterClaim
    );
    let unreadable = unreadable(vec![PathBuf::from("/h/bad.pid")]);
    assert_eq!(unreadable[0].reason, StaleReason::UnreadableClaim);
    assert_eq!(unreadable[0].pid, None);
    let wording: Vec<&str> = [
        StaleReason::Gone,
        StaleReason::NotDispatcher,
        StaleReason::StartedAfterClaim,
        StaleReason::UnreadableClaim,
    ]
    .map(StaleReason::text)
    .to_vec();
    assert_eq!(
        wording,
        [
            "gone",
            "not a dispatcher command",
            "started after its claim",
            "unreadable claim"
        ]
    );
}

#[test]
fn the_clock_is_read_before_ps_runs() {
    let ps_calls = std::cell::Cell::new(0u64);
    let clock = || at(1_000 + 10 * ps_calls.get());
    let ps = || {
        ps_calls.set(ps_calls.get() + 1);
        " 7 01:05 /h/codetypera2a dispatcher\n".to_string()
    };
    let found = table_from(&clock, &ps, &[]);
    assert_eq!(
        found[0].started,
        at(1_000) - Duration::from_secs(65),
        "a clock read after ps shifts every start time later by the time ps took"
    );
    assert!(claims_owned(&claim_at(7, "f", 1_000 - 64), &found[0]));
}

#[test]
fn the_pidfile_and_the_registry_entry_of_one_pid_give_one_owner() {
    let claims = [
        claim_at(7, "/h/dispatcher.pid", 105),
        claim_at(7, "/h/dispatcher.pid.d/7", 105),
    ];
    let scoped = scope(&claims, &[proc_at(7, 100, DISPATCHER)], &|_| false);
    assert_eq!(scoped.ours.len(), 1);
    assert!(scoped.stale.is_empty());
}

#[test]
fn canonical_legacy_and_active_pid_paths_are_scanned() {
    let homes = [PathBuf::from("/h/.codetypera2a"), PathBuf::from("/h/old")];
    let listed = |dir: &Path| {
        if dir == Path::new("/x/active.pid.d") {
            vec![PathBuf::from("/x/active.pid.d/9")]
        } else {
            Vec::new()
        }
    };
    let files = claim_files(&homes, Some(Path::new("/x/active.pid")), &listed);
    for expected in [
        "/h/.codetypera2a/dispatcher.pid",
        "/h/.codetypera2a/hub.db.dispatcher.pid",
        "/h/old/dispatcher.pid",
        "/h/old/hub.db.dispatcher.pid",
        "/x/active.pid",
        "/x/active.pid.d/9",
    ] {
        assert!(files.contains(&PathBuf::from(expected)), "{expected}");
    }
    let mut unique = files.clone();
    unique.dedup();
    assert_eq!(unique, files, "no path twice");
}

#[test]
fn malformed_claims_are_rejected_and_reported() {
    let dir = std::env::temp_dir().join(format!("a2a-procs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("dispatcher.pid.d")).unwrap();
    let write = |name: &str, body: &str| {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    };
    let good = write("good.pid", "4242");
    let registry_good = write("dispatcher.pid.d/4343", "2.2.3");
    let pidfile = dir.join("dispatcher.pid");
    let bad = [
        write("zero.pid", "0"),
        write("one.pid", "1"),
        write("abc.pid", "abc"),
        write("big.pid", &"1".repeat(65)),
        write("dispatcher.pid.d/notanumber", "2.2.3"),
    ];
    let target = write("target.pid", "5555");
    let link = dir.join("link.pid");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut files = vec![pidfile, good.clone(), registry_good.clone(), link.clone()];
    files.extend(bad.iter().cloned());
    files.push(dir.join("absent.pid"));
    let (claims, stale) = read_claims(&files);
    let pids: Vec<u32> = claims.iter().map(|c| c.pid).collect();
    assert_eq!(pids, vec![4242, 4343]);
    assert!(stale.contains(&link), "a symlink is never followed");
    for path in &bad {
        assert!(stale.contains(path), "{path:?}");
    }
    assert!(
        !stale.contains(&dir.join("absent.pid")),
        "missing is not stale"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_pid_above_i32_max_is_rejected() {
    let dir = std::env::temp_dir().join(format!("a2a-procs-pid-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut files = Vec::new();
    for (name, pid) in [
        ("max.pid", "2147483647"),
        ("over.pid", "2147483648"),
        ("u32.pid", "4294967295"),
    ] {
        std::fs::write(dir.join(name), pid).unwrap();
        files.push(dir.join(name));
    }
    let (claims, stale) = read_claims(&files);
    assert_eq!(
        claims.iter().map(|c| c.pid).collect::<Vec<_>>(),
        vec![2_147_483_647]
    );
    assert_eq!(stale.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_registry_entry_must_sit_in_exactly_pidfile_dot_d() {
    let dir = std::env::temp_dir().join(format!("a2a-procs-reg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("dispatcher.pid.d")).unwrap();
    std::fs::create_dir_all(dir.join("unrelated.d")).unwrap();
    let entry = |name: &str| {
        let path = dir.join(name);
        std::fs::write(&path, "2.2.3").unwrap();
        path
    };
    let ours = entry("dispatcher.pid.d/4343");
    let other = entry("unrelated.d/4444");
    let files = vec![dir.join("dispatcher.pid"), ours.clone(), other.clone()];
    let (claims, stale) = read_claims(&files);
    assert_eq!(claims.iter().map(|c| c.pid).collect::<Vec<_>>(), vec![4343]);
    assert_eq!(stale, vec![other]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn etime_formats_are_parsed() {
    let cases = [
        ("05", 5),
        ("01:05", 65),
        ("02:01:05", 7265),
        ("3-02:01:05", 3 * 86_400 + 7265),
        ("  12:00 ", 720),
    ];
    for (text, seconds) in cases {
        assert_eq!(
            parse_etime(text),
            Some(Duration::from_secs(seconds)),
            "{text}"
        );
    }
    for garbage in ["", "abc", "1-", "a:b", "1:2:3:4", "-5", "1--2:00"] {
        assert_eq!(parse_etime(garbage), None, "{garbage}");
    }
}

#[test]
fn only_our_dispatchers_are_picked_from_the_process_list() {
    let ps = " 8112   01:05 /Users/x/.cargo/bin/oc dispatch\n 67814 2-01:00:00 /Users/x/.cargo/bin/codetypera2a dispatcher\n 5 00:01 vim oc dispatch.md\n 9 00:01 /usr/bin/other dispatcher\n 77 00:01 /x/codetypera2a dispatcher\n 12 junk /x/codetypera2a dispatcher\n";
    let now = at(1_000_000);
    let found = parse_ps(ps, now, &[77]);
    let pids: Vec<u32> = found.iter().map(|p| p.pid).collect();
    assert_eq!(pids, vec![8112, 67814]);
    assert_eq!(found[0].started, now - Duration::from_secs(65));
    assert_eq!(
        found[1].command,
        "/Users/x/.cargo/bin/codetypera2a dispatcher"
    );
}

#[test]
fn a_younger_or_different_process_is_no_longer_ours() {
    let now = at(1_000);
    let claimed = at(500);
    let long_running = Duration::from_secs(600);
    assert!(still_ours(DISPATCHER, long_running, now, claimed));
    assert!(!still_ours(
        DISPATCHER,
        Duration::from_secs(10),
        now,
        claimed
    ));
    assert!(!still_ours("vim notes.md", long_running, now, claimed));
}
