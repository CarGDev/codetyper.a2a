use super::*;
use crate::models::tiers::{Tier, effective_tier};

fn temp_file(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("a2a-deep-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(DEEP_FILE)
}

fn list_of(names: &[&str]) -> DeepList {
    let mut list = DeepList::default();
    names.iter().for_each(|name| {
        list.insert(name.to_string());
    });
    list
}

#[test]
fn parse_name_accepts_shipped_agents_only() {
    assert_eq!(parse_name(" architect ").as_deref(), Ok("architect"));
    assert_eq!(parse_name("code-debugger").as_deref(), Ok("code-debugger"));
    let long = "a".repeat(64);
    for bad in [
        "Architect",
        "../x",
        "",
        "   ",
        "a b",
        "-x",
        "__proto__",
        long.as_str(),
    ] {
        assert!(
            matches!(parse_name(bad), Err(DeepError::InvalidName(_))),
            "{bad:?}"
        );
    }
    assert_eq!(
        parse_name("someones-own-agent"),
        Err(DeepError::UnknownAgent("someones-own-agent".into()))
    );
}

#[test]
fn a_missing_file_is_an_empty_list_and_a_saved_one_round_trips_sorted() {
    let path = temp_file("roundtrip");
    assert_eq!(load(&path), Ok(DeepList::default()));
    save(&path, &list_of(&["plan", "architect"])).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path)
            .unwrap()
            .replace(char::is_whitespace, ""),
        r#"["architect","plan"]"#
    );
    assert_eq!(load(&path), Ok(list_of(&["architect", "plan"])));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_corrupt_file_is_reported_and_left_untouched() {
    let path = temp_file("corrupt");
    for body in [
        "not json",
        "{\"a\":1}",
        "[1, 2]",
        "[\"Bad Name\"]",
        "[\"not-a-shipped-agent\"]",
    ] {
        std::fs::write(&path, body).unwrap();
        assert_eq!(load(&path), Err(DeepError::Corrupt(path.clone())), "{body}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
    }
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_list_over_the_limit_is_not_saved_or_loaded() {
    let path = temp_file("limit");
    let too_many: Vec<String> = (0..=MAX_DEEP).map(|n| format!("agent-{n}")).collect();
    std::fs::write(&path, serde_json::to_string(&too_many).unwrap()).unwrap();
    assert!(load(&path).is_err());
    let mut full = DeepList::default();
    full.0.extend(too_many);
    assert_eq!(save(&path, &full), Err(DeepError::TooMany));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn the_environment_adds_valid_names_and_warns_about_the_rest() {
    let (list, warnings) = from_env(Some("architect, plan,, ../x ,nobody"));
    assert_eq!(list, list_of(&["architect", "plan"]));
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert_eq!(from_env(None), (DeepList::default(), Vec::new()));
    assert_eq!(from_env(Some("")).0, DeepList::default());
}

#[test]
fn a_listed_agent_is_deep_and_converts_to_opus_while_the_default_install_has_no_opus() {
    let deep = list_of(&["architect"]);
    assert_eq!(effective_tier("architect", &deep), Some(Tier::Deep));
    assert_eq!(effective_tier("build", &deep), Some(Tier::Balanced));
    let text = "---\ndescription: d\nmodel: github-copilot/claude-sonnet-5.5\n---\nb\n";
    let alias = effective_tier("architect", &deep).map(crate::models::claude_alias);
    let out = crate::targets::convert::agent("architect", text, alias).unwrap();
    assert!(out.contains("model: opus"), "{out}");
    let none = DeepList::default();
    for file in crate::cli::AGENTS.files() {
        let name = file.path().file_stem().unwrap().to_str().unwrap();
        assert_ne!(effective_tier(name, &none), Some(Tier::Deep), "{name}");
    }
}

#[test]
fn a_deep_agent_gets_the_strongest_installed_model_on_opencode_and_nobody_else_does() {
    use crate::models::tui::build_rows;
    let installed: Vec<String> = [
        "github-copilot/claude-haiku-5.5",
        "github-copilot/claude-sonnet-5.5",
        "github-copilot/claude-opus-5.5",
    ]
    .map(String::from)
    .to_vec();
    let agents = || vec![("architect".to_string(), String::new(), None)];
    let recommended = |deep: &DeepList| {
        build_rows(agents(), &Default::default(), &installed, deep)[0]
            .rec
            .clone()
    };
    assert_eq!(
        recommended(&DeepList::default()).as_deref(),
        Some("github-copilot/claude-sonnet-5.5")
    );
    assert_eq!(
        recommended(&list_of(&["architect"])).as_deref(),
        Some("github-copilot/claude-opus-5.5")
    );
}

#[test]
fn a_symlinked_deep_file_is_refused_and_never_followed() {
    let path = temp_file("symlink");
    let target = path.with_file_name("elsewhere.json");
    std::fs::write(&target, r#"["architect"]"#).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert_eq!(load(&path), Err(DeepError::Corrupt(path.clone())));
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        r#"["architect"]"#
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
