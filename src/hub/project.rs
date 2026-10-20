//! Project identity shared by the hub and the dispatcher. Same project = same git root.
//! A folder outside git is its own project and only matches the exact same path.
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Long-lived hub process: a folder may become a repo later.
const TTL: Duration = Duration::from_secs(30);

static GIT_CACHE: LazyLock<Mutex<HashMap<String, (Option<String>, Instant)>>> =
    LazyLock::new(Default::default);

pub fn git_root(dir: &str) -> Option<String> {
    if dir.is_empty() {
        return None;
    }
    let mut cache = GIT_CACHE.lock().unwrap();
    if let Some((root, at)) = cache.get(dir) {
        if at.elapsed() < TTL {
            return root.clone();
        }
    }
    // not a git repo or folder is gone -> None
    let root = Command::new("git")
        .args(["-C", dir, "rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    cache.insert(dir.to_string(), (root.clone(), Instant::now()));
    root
}

pub fn clear_project_cache() {
    GIT_CACHE.lock().unwrap().clear();
}

pub fn same_project(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    match (git_root(a), git_root(b)) {
        (Some(ra), Some(rb)) => ra == rb, // outside git a folder is its own project: exact path only
        _ => false,
    }
}

/// Grouping root for display/logging: git toplevel, else the folder itself.
pub fn project_root(dir: &str) -> Option<String> {
    if dir.is_empty() {
        None
    } else {
        Some(git_root(dir).unwrap_or_else(|| dir.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_rules() {
        assert!(!same_project("", "/x"));
        assert!(same_project("/no/such/dir", "/no/such/dir"));
        assert!(!same_project("/no/such/a", "/no/such/b"));
        assert_eq!(project_root(""), None);
        assert_eq!(
            project_root("/no/such/dir").as_deref(),
            Some("/no/such/dir")
        );
        let here = env!("CARGO_MANIFEST_DIR");
        assert!(same_project(here, &format!("{here}/src")));
    }
}
