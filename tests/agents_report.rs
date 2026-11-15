//! One compact report-back contract: the canonical block lives in `docs/report-back.md` and every template
//! that reports back carries it byte for byte.
use std::path::{Path, PathBuf};

const HEADING: &str = "### Report back (mandatory)";

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn templates() -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = std::fs::read_dir(root().join("agents"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .map(|e: std::fs::DirEntry| {
            let path: PathBuf = e.path();
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(path).unwrap())
        })
        .collect();
    found.sort();
    found
}

fn canonical() -> String {
    std::fs::read_to_string(root().join("docs/report-back.md"))
        .unwrap()
        .trim()
        .to_string()
}

/// The report-back section of a template: from its heading to the next heading or the end.
fn report_section(text: &str) -> Option<String> {
    let start = text.find(HEADING)?;
    let rest = &text[start..];
    let end = rest[HEADING.len()..]
        .find("\n#")
        .map_or(rest.len(), |at| at + HEADING.len());
    Some(rest[..end].trim().to_string())
}

#[test]
fn report_block_is_byte_equal_to_docs_in_every_template() {
    let expected = canonical();
    assert!(
        expected.starts_with(HEADING) && expected.len() < 700,
        "{expected}"
    );
    let mut carrying = 0;
    for (name, text) in templates() {
        let Some(section) = report_section(&text) else {
            assert_eq!(name, "orchestrator", "{name} has no report-back section");
            continue;
        };
        assert_eq!(section, expected, "{name} drifted from docs/report-back.md");
        carrying += 1;
    }
    assert_eq!(carrying, 21);
}

#[test]
fn orchestrator_item7_names_the_five_fields() {
    let text = std::fs::read_to_string(root().join("agents/orchestrator.md")).unwrap();
    let item = text
        .lines()
        .find(|line| line.starts_with("7. `REPORT BACK`"))
        .expect("item 7");
    let at = text.find(item).unwrap();
    let block: String = text[at..].lines().take(2).collect::<Vec<_>>().join(" ");
    for field in [
        "STATUS",
        "SUMMARY",
        "FILES",
        "EVIDENCE",
        "BLOCKERS",
        "a2a_update_task",
    ] {
        assert!(block.contains(field), "item 7 lacks {field}: {block}");
    }
}

#[test]
fn no_template_reports_open_issues_or_suggested_next() {
    for (name, text) in templates() {
        assert!(
            !text.contains("SUGGESTED NEXT"),
            "{name} still asks for SUGGESTED NEXT"
        );
        assert!(
            !text.contains("OPEN ISSUES"),
            "{name} still asks for OPEN ISSUES"
        );
    }
}
