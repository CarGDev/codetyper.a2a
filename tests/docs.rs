//! The README and CONTRIBUTING describe the model tiers and the deep opt-in as they work now.
use std::path::Path;

fn read(file: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(file)).unwrap()
}

#[test]
fn the_readme_documents_the_deep_opt_in_and_the_current_tiers() {
    let readme = read("README.md");
    for needed in ["models deep", "A2A_DEEP_AGENTS", "no `model:` line"] {
        assert!(readme.contains(needed), "README lacks: {needed}");
    }
    for stale in [
        "a different family than the Claude",
        "`gpt-6.1-sol`, a different",
    ] {
        assert!(!readme.contains(stale), "README still says: {stale}");
    }
}

#[test]
fn contributing_names_the_tier_table_not_the_old_reviewer_tier() {
    let contributing = read("CONTRIBUTING.md");
    assert!(
        !contributing.contains("other-family"),
        "CONTRIBUTING mentions the removed tier"
    );
    assert!(contributing.contains("tier_for"));
}

#[test]
fn the_models_module_doc_does_not_promise_another_family_for_reviewers() {
    let source = read("src/models.rs");
    let doc: String = source
        .lines()
        .take_while(|l| l.starts_with("//!"))
        .collect();
    assert!(!doc.contains("another model family"), "{doc}");
}

fn tracked_docs() -> Vec<String> {
    let mut docs: Vec<String> = [
        "README.md",
        "CHANGELOG.md",
        "CONTRIBUTING.md",
        "SECURITY.md",
    ]
    .map(String::from)
    .to_vec();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".md") {
            docs.push(format!("docs/{name}"));
        }
    }
    docs
}

#[test]
fn no_doc_advises_killing_dispatchers_by_name_pattern() {
    for file in tracked_docs() {
        let text = read(&file);
        let offender = text
            .lines()
            .find(|line| line.contains("pkill") || line.contains("killall"));
        assert!(
            offender.is_none(),
            "{file} advises a name-pattern kill: {offender:?}"
        );
    }
}

#[test]
fn the_changelog_corrects_the_old_inherit_claim() {
    let changelog = read("CHANGELOG.md");
    assert!(
        changelog.contains("Claude subagents never get `inherit`"),
        "the 2.0 entry that maps gpt-* to inherit needs the correction note"
    );
}
