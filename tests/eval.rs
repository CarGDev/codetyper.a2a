//! `codetypera2a eval` replays fixed deterministic scenarios (no LLM, fake opencode).
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_codetypera2a");
const NAMES: [&str; 5] = [
    "routing",
    "hop-cap",
    "no-duplicate-nudges",
    "reviewer-seeded-bug",
    "reviewer-live",
];

fn eval(args: &[&str]) -> (i32, String) {
    let out = Command::new(BIN).arg("eval").args(args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn eval_passes_every_scenario() {
    let (code, out) = eval(&[]);
    assert_eq!(code, 0, "eval must exit 0:\n{out}");
    for name in &NAMES[..4] {
        assert!(
            out.lines()
                .any(|l| l.starts_with("PASS") && l.contains(name)),
            "{name} must PASS:\n{out}"
        );
    }
    assert!(
        out.lines()
            .any(|l| l.starts_with("SKIP") && l.contains("reviewer-live")),
        "reviewer-live must SKIP:\n{out}"
    );
    assert!(!out.contains("FAIL"), "{out}");
}

#[test]
fn eval_list_prints_the_scenario_names() {
    let (code, out) = eval(&["--list"]);
    assert_eq!(code, 0);
    for name in NAMES {
        assert!(
            out.lines().any(|l| l.trim() == name),
            "missing {name}:\n{out}"
        );
    }
}

#[test]
fn eval_unknown_filter_fails() {
    let (code, _) = eval(&["no-such-scenario"]);
    assert_ne!(code, 0);
}
