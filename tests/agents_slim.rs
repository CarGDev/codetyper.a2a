//! Coordination and read-only agents carry no writer standards: the synced "Engineering Standards" block is
//! gone from every template, each keeps its own `## Role boundary`, and the orchestrator's helpers note is short.
use std::path::{Path, PathBuf};

fn agents_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("agents")
}

fn templates() -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = std::fs::read_dir(agents_dir())
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .map(|e| {
            let name = e.path().file_stem().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(e.path()).unwrap())
        })
        .collect();
    found.sort();
    found
}

fn template(name: &str) -> String {
    std::fs::read_to_string(agents_dir().join(format!("{name}.md"))).unwrap()
}

#[test]
fn no_template_has_the_synced_engineering_standards_block() {
    let all = templates();
    assert_eq!(all.len(), 22);
    for (name, text) in all {
        assert!(
            !text.contains("Engineering Standards (Synchronized)"),
            "{name} still carries the synced block"
        );
        assert!(
            !text.contains("Keep modified source files under 500 lines"),
            "{name} still carries the writer sentence"
        );
    }
}

#[test]
fn every_template_keeps_a_role_boundary_section() {
    for (name, text) in templates() {
        assert!(
            text.lines().any(|line| line == "## Role boundary"),
            "{name} lost its role boundary"
        );
        assert!(!text.contains("### Role boundary"), "{name}: not renamed");
    }
}

const ORCHESTRATOR_BUDGET: usize = 18_000;

#[test]
fn orchestrator_is_at_most_18kb() {
    let size = template("orchestrator").len();
    assert!(size <= ORCHESTRATOR_BUDGET, "orchestrator is {size} bytes");
}

#[test]
fn orchestrator_helpers_section_is_three_lines_a2a_first() {
    let text = template("orchestrator");
    assert!(!text.contains("## Subagents (cheap, in-session helpers)"));
    let start = text.find("## Helpers").expect("a Helpers section");
    let body: Vec<&str> = text[start..]
        .lines()
        .skip(1)
        .take_while(|line| !line.starts_with("## "))
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(body.len(), 3, "{body:?}");
    let joined = body.join(" ");
    assert!(joined.contains("A2A"), "{joined}");
    assert!(joined.contains("permission.task"), "{joined}");
}

/// Template sizes after the synced block is gone (before it: +1480 bytes), each with 5% headroom.
const SIZE_TABLE: [(&str, usize); 5] = [
    ("orchestrator", 17_810),
    ("summarizer", 3_160),
    ("doc-lookup", 3_490),
    ("git-historian", 4_020),
    ("test-runner", 5_550),
];

#[test]
fn small_templates_stay_within_their_size_table() {
    for (name, expected) in SIZE_TABLE {
        let size = template(name).len();
        let limit = expected + expected / 20;
        assert!(size <= limit, "{name} is {size} bytes, limit {limit}");
    }
}

/// The body of `## <heading>` in `text`, up to the next `## ` heading.
fn section<'a>(text: &'a str, heading: &str) -> Option<&'a str> {
    let marker = format!("\n## {heading}\n");
    let start = text.find(&marker)? + 1;
    let rest = &text[start..];
    let end = rest[3..].find("\n## ").map_or(rest.len(), |at| at + 3);
    Some(&rest[..end])
}

#[test]
fn orchestrator_has_a_token_discipline_section() {
    let text = template("orchestrator");
    assert_eq!(text.matches("\n## Token discipline\n").count(), 1);
}

#[test]
fn token_discipline_section_is_at_most_600_bytes() {
    let text = template("orchestrator");
    let body = section(&text, "Token discipline").expect("the section");
    assert!(body.len() <= 600, "{} bytes", body.len());
}

#[test]
fn token_discipline_names_the_five_rules() {
    let text = template("orchestrator");
    let body = section(&text, "Token discipline").expect("the section");
    for phrase in [
        "by reference",
        "12 lines",
        "never read a full plan",
        "artifact summary",
        "batch",
        "one status table",
    ] {
        assert!(
            body.to_lowercase().contains(phrase),
            "the section lacks '{phrase}': {body}"
        );
    }
}

#[test]
fn orchestrator_stays_within_the_w7_size_budget() {
    let size = template("orchestrator").len();
    assert!(
        size <= ORCHESTRATOR_BUDGET,
        "{size} > {ORCHESTRATOR_BUDGET}"
    );
}

#[test]
fn brain_dispatch_rule_matches_if_present() {
    let brain = template("brain");
    let dispatches = brain.contains("a2a_send_message {from:\"brain\"");
    assert!(dispatches, "brain is expected to dispatch work");
    assert!(
        brain.contains("by reference"),
        "brain lacks the dispatch rule"
    );
    assert!(brain.contains("12 lines"), "brain lacks the size limit");
}
