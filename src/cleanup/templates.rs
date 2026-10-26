//! The agent and command files `init` installs, by relative path, read from the templates embedded in the binary.
//! `uninstall` removes exactly these, never a file the user wrote themselves.
use crate::cli::{AGENTS, COMMANDS};
use include_dir::Dir;

const MARKDOWN: &str = "md";

fn collect(dir: &Dir, names: &mut Vec<String>) {
    names.extend(
        dir.files()
            .filter(|file| file.path().extension().is_some_and(|ext| ext == MARKDOWN))
            .filter_map(|file| file.path().to_str().map(String::from)),
    );
    dir.dirs().for_each(|sub| collect(sub, names));
}

fn names_of(dir: &Dir) -> Vec<String> {
    let mut names = Vec::new();
    collect(dir, &mut names);
    names.sort();
    names
}

pub fn agent_files() -> Vec<String> {
    names_of(&AGENTS)
}

pub fn command_files() -> Vec<String> {
    names_of(&COMMANDS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installed_templates_are_listed_by_relative_path() {
        assert!(agent_files().iter().any(|name| name == "orchestrator.md"));
        assert!(agent_files().len() >= 21);
        assert!(command_files().iter().any(|name| name == "plan-new.md"));
    }
}
