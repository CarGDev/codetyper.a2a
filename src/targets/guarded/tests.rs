use super::*;
use std::time::{Duration, UNIX_EPOCH};

const NEW: &[u8] = b"new text\n";

fn file(bytes: &[u8]) -> Existing {
    Existing::File(bytes.to_vec())
}

fn hash_of(bytes: &[u8]) -> String {
    super::super::claude::hash(bytes)
}

#[test]
fn decide_policy_table_p1_to_p7_with_and_without_force() {
    use Decision::*;
    use KeepReason::*;
    let theirs = file(b"their own notes\n");
    let ours_old = file(b"older release text\n");
    let old_hash = hash_of(b"older release text\n");
    let edited_hash = hash_of(b"what we wrote\n");
    let marked = file(b"<!-- gentle-ai:persona -->\nhi\n");
    let cases: Vec<(&str, Existing, Option<&str>, Decision, Decision)> = vec![
        ("P1 missing", Existing::Missing, None, Write, Write),
        ("P2 identical", file(NEW), None, Current, Current),
        (
            "P3 ours unedited",
            ours_old,
            Some(old_hash.as_str()),
            Refresh,
            Refresh,
        ),
        (
            "P4 edited ours",
            theirs_clone(&theirs),
            Some(edited_hash.as_str()),
            Keep(Edited),
            Replace(Edited),
        ),
        ("P5 foreign", theirs, None, Keep(Foreign), Replace(Foreign)),
        (
            "P6 gentle-ai",
            marked,
            None,
            Keep(GentleAi),
            Replace(GentleAi),
        ),
        (
            "P7 symlink or directory",
            Existing::NotRegularFile,
            None,
            Keep(NotRegularFile),
            Keep(NotRegularFile),
        ),
    ];
    for (name, existing, recorded, plain, forced) in cases {
        assert_eq!(decide(&existing, recorded, NEW, false), plain, "{name}");
        assert_eq!(
            decide(&existing, recorded, NEW, true),
            forced,
            "{name} --force"
        );
    }
}

fn theirs_clone(existing: &Existing) -> Existing {
    match existing {
        Existing::File(bytes) => Existing::File(bytes.clone()),
        Existing::Missing => Existing::Missing,
        Existing::NotRegularFile => Existing::NotRegularFile,
    }
}

#[test]
fn backup_name_format_counter_and_limit() {
    let at = UNIX_EPOCH + Duration::from_secs(1_794_205_060);
    let base = "CLAUDE.md.bak-20261109T061740Z";
    assert_eq!(backup_name(at, &|_| false).as_deref(), Ok(base));
    let taken = [base.to_string(), format!("{base}-1")];
    let second = backup_name(at, &|name| taken.iter().any(|t| t == name));
    assert_eq!(second, Ok(format!("{base}-2")));
    assert!(
        backup_name(at, &|_| true).is_err(),
        "gives up after 100 tries"
    );
    let epoch = backup_name(UNIX_EPOCH, &|_| false).unwrap();
    assert_eq!(epoch, "CLAUDE.md.bak-19700101T000000Z");
}

#[test]
fn claude_user_md_is_at_most_4096_bytes() {
    assert!(CLAUDE_MD.len() <= 4096, "{} bytes", CLAUDE_MD.len());
}

#[test]
fn claude_user_md_has_required_sections() {
    for section in [
        "Authorization",
        "Blocking choices",
        "Coordinating",
        "Reporting",
        "Review (RDD)",
        "Engram memory",
        "Safety",
        "Language",
    ] {
        assert!(
            CLAUDE_MD
                .lines()
                .any(|l| l.starts_with("## ") && l.contains(section)),
            "missing section {section}"
        );
    }
}

#[test]
fn claude_user_md_engram_section_is_the_four_pinned_lines() {
    let start = CLAUDE_MD.find("## Engram memory").unwrap();
    let section: Vec<&str> = CLAUDE_MD[start..]
        .lines()
        .skip(1)
        .take_while(|l| !l.starts_with("## "))
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(section.len(), 4, "{section:?}");
    assert!(section[0].starts_with("- Follow Engram MCP/SessionStart protocol as authoritative"));
    assert!(section[1].starts_with("- Proactively mem_save decisions, bugfixes"));
    assert!(section[2].starts_with("- For past work: mem_context"));
    assert!(section[3].starts_with("- Memory failures never block the user-facing answer"));
}

#[test]
fn claude_user_md_has_no_lifecycle_terms() {
    for term in ["START", "capture-result", "acknowledge", "odd/tasks"] {
        assert!(!CLAUDE_MD.contains(term), "contains {term}");
    }
}

#[test]
fn claude_user_md_has_no_paths_email_or_markers() {
    for term in ["/Users/", "/home/", "gentle-ai:", "@gmail", "@example"] {
        assert!(!CLAUDE_MD.contains(term), "contains {term}");
    }
    assert!(
        !CLAUDE_MD
            .split_whitespace()
            .any(|w| w.contains('@') && w.contains('.'))
    );
}

fn sandbox(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("a2a-guarded-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn sync_writes_records_and_reports_up_to_date_on_rerun() {
    let root = sandbox("sync");
    let mut manifest = Manifest::default();
    assert_eq!(
        sync(&root, &mut manifest, false).unwrap(),
        "CLAUDE.md: written"
    );
    assert_eq!(std::fs::read_to_string(root.join(REL)).unwrap(), CLAUDE_MD);
    assert_eq!(
        manifest.recorded(REL),
        Some(hash_of(CLAUDE_MD.as_bytes()).as_str())
    );
    assert_eq!(
        sync(&root, &mut manifest, false).unwrap(),
        "CLAUDE.md: up to date"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn ours_unchanged_needs_a_regular_file_with_the_recorded_hash() {
    let root = sandbox("ours");
    let mut manifest = Manifest::default();
    sync(&root, &mut manifest, false).unwrap();
    assert!(ours_unchanged(&root, REL, &manifest));
    std::fs::write(root.join(REL), "edited").unwrap();
    assert!(!ours_unchanged(&root, REL, &manifest));
    assert!(!ours_unchanged(&root, REL, &Manifest::default()));
    let _ = std::fs::remove_dir_all(root);
}

const SKILL_REL: &str = "skills/rdd-review/SKILL.md";

fn skill_text() -> String {
    let bytes = guarded_files()
        .into_iter()
        .find(|(rel, _)| rel == SKILL_REL)
        .map(|(_, bytes)| bytes)
        .expect("the rdd-review skill is a guarded file");
    String::from_utf8(bytes).unwrap()
}

#[test]
fn skill_is_at_most_4096_bytes_with_name_and_description() {
    let text = skill_text();
    assert!(text.len() <= 4096, "{} bytes", text.len());
    assert!(text.starts_with("---\nname: rdd-review\n"));
    assert!(text.lines().take(4).any(|l| l.starts_with("description: ")));
}

#[test]
fn skill_description_mentions_review_due_trigger() {
    let text = skill_text();
    let description = text
        .lines()
        .find(|l| l.starts_with("description: "))
        .unwrap();
    assert!(description.contains("review_due=true"), "{description}");
    assert!(
        description.contains("gentle-ai review assess"),
        "{description}"
    );
}
