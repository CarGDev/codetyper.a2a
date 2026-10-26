//! Where things live. Runtime state is client-neutral and hangs off one private home.
use std::path::{Path, PathBuf};

/// Marker written into a migrated runtime tree.
pub const COMPAT_RELEASE: &str = "codetypera2a-home-1";

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `$OPENCODE_CONFIG_DIR`, else `~/.config/opencode` (configuration only).
pub fn config_dir() -> PathBuf {
    env_path("OPENCODE_CONFIG_DIR").unwrap_or_else(|| {
        let home = env_path("HOME").unwrap_or_else(|| PathBuf::from("."));
        home.join(".config").join("opencode")
    })
}

pub fn a2a_dir() -> PathBuf {
    runtime_home()
}

/// `$CODETYPERA2A_HOME`, else `$HOME/.codetypera2a`.
///
/// A relative `HOME` fallback keeps command-line tools usable in deliberately minimal
/// environments while avoiding any dependence on the OpenCode configuration directory.
pub fn runtime_home() -> PathBuf {
    runtime_home_from(env_path("CODETYPERA2A_HOME"), env_path("HOME"))
}

fn runtime_home_from(override_home: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    override_home.unwrap_or_else(|| {
        home.unwrap_or_else(|| PathBuf::from("."))
            .join(".codetypera2a")
    })
}

/// `$A2A_DB`, else `<runtime-home>/hub.db`.
pub fn db_path() -> PathBuf {
    env_path("A2A_DB").unwrap_or_else(|| runtime_home().join("hub.db"))
}

/// `$A2A_AGENT_DIR`, else `<config>/agents` (the opencode v2 location), falling back to an
/// existing legacy `<config>/agent`.
pub fn agent_dir() -> PathBuf {
    env_path("A2A_AGENT_DIR").unwrap_or_else(|| {
        let (new, old) = (config_dir().join("agents"), config_dir().join("agent"));
        if !new.exists() && old.exists() {
            old
        } else {
            new
        }
    })
}

pub fn policy_path() -> PathBuf {
    env_path("A2A_POLICY").unwrap_or_else(|| runtime_home().join("model-policy.json"))
}

pub fn opencode_config_path() -> PathBuf {
    config_dir().join("opencode.jsonc")
}

pub fn legacy_home() -> PathBuf {
    config_dir().join("a2a")
}

pub fn migration_lock_path(home: &std::path::Path) -> PathBuf {
    home.parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(".codetypera2a.migration.lock")
}

/// The runtime tree which owns the database and all dispatcher state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeHome {
    pub root: PathBuf,
    pub legacy: bool,
}

/// Dispatcher paths derived from one owning home. Explicit environment overrides remain intact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePaths {
    pub pid: PathBuf,
    pub lock: PathBuf,
    pub log: PathBuf,
    pub tabs: PathBuf,
    pub clients: PathBuf,
}

/// Refusal for a live legacy owner. Safe context only: no paths, pids or secrets; raised before
/// the destination is inspected, so nothing was copied, merged, removed or renamed.
pub const LEGACY_LIVE_REFUSAL: &str = "legacy dispatcher is still running; migration refused (owner=legacy-live, destination_touched=false; stop the legacy dispatcher, then retry)";

pub fn legacy_dispatcher_live(legacy: &Path) -> bool {
    let pid = legacy.join("hub.db.dispatcher.pid").into_os_string();
    std::fs::read_to_string(pid)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .is_some_and(crate::sys::pid_alive)
}

pub fn resolve_runtime_home(canonical: &Path, legacy: &Path) -> RuntimeHome {
    // Legacy state is migration input only and can never become an active owner.
    let _ = legacy;
    RuntimeHome { root: canonical.to_path_buf(), legacy: false }
}

pub fn runtime_paths(home: &RuntimeHome) -> RuntimePaths {
    RuntimePaths {
        pid: env_path("A2A_DISPATCHER_PID").unwrap_or_else(|| {
            home.root.join("dispatcher.pid")
        }),
        lock: env_path("A2A_DISPATCHER_LOCK").unwrap_or_else(|| home.root.join("dispatcher.lock")),
        log: env_path("A2A_DISPATCHER_LOG").unwrap_or_else(|| home.root.join("dispatcher.log")),
        tabs: env_path("A2A_TABS").unwrap_or_else(|| home.root.join("tabs.jsonl")),
        clients: env_path("A2A_CLIENTS_DIR").unwrap_or_else(|| home.root.join("clients")),
    }
}

fn destination_is_scaffolding(path: &Path) -> std::io::Result<bool> {
    let root = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(err) => return Err(err),
    };
    if root.file_type().is_symlink() || !root.is_dir() {
        return Ok(false);
    }
    if !path.exists() {
        return Ok(true);
    }
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        if name != ".legacy-fallback-release" && name != ".migration-release" {
            return Ok(false);
        }
        if !entry.file_type()?.is_file() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn restrict_tree(path: &std::path::Path) -> std::io::Result<()> {
    let kind = std::fs::symlink_metadata(path)?.file_type();
    if kind.is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("symlink in migrated runtime state: {}", path.display()),
        ));
    }
    if kind.is_dir() {
        ensure_private_dir(path)?;
        for entry in std::fs::read_dir(path)? {
            restrict_tree(&entry?.path())?;
        }
    } else {
        ensure_private_file(path)?;
    }
    Ok(())
}

/// Atomically move a legacy runtime home into the canonical home. No copying or merging occurs.
pub fn migrate_legacy(home: &std::path::Path, legacy: &std::path::Path) -> Result<bool, String> {
    if !legacy.exists() || home.join("hub.db").exists() {
        return Ok(false);
    }
    let lock = migration_lock_path(home);
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let guard = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .map_err(|e| format!("migration lock unavailable: {e}"))?;
    ensure_private_file(&lock).map_err(|e| e.to_string())?;
    let result = migrate_legacy_locked(home, legacy);
    drop(guard);
    let _ = std::fs::remove_file(lock);
    result
}

/// Migrate while the caller holds the shared migration lock.
pub fn migrate_legacy_locked(
    home: &std::path::Path,
    legacy: &std::path::Path,
) -> Result<bool, String> {
    if !legacy.exists() || home.join("hub.db").exists() {
        return Ok(false);
    }
    if legacy_dispatcher_live(legacy) {
        return Err(LEGACY_LIVE_REFUSAL.into());
    }
    let result = (|| {
        if !home.is_absolute() || !legacy.is_absolute() {
            return Err("migration roots must be absolute".into());
        }
        if !destination_is_scaffolding(home).map_err(|e| e.to_string())? {
            return Err(format!(
                "migration destination is not empty: {}",
                home.display()
            ));
        }
        if !legacy.is_dir() { return Err("legacy runtime root is not a directory".into()); }
        let quarantine_root = legacy.parent().unwrap_or(Path::new(".")).join(".codetypera2a-legacy-quarantine");
        ensure_private_dir(&quarantine_root).map_err(|e| e.to_string())?;
        let mut quarantine = None;
        let entries = std::fs::read_dir(legacy).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        for entry in entries {
            let name = entry.file_name();
            if std::fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?.file_type().is_symlink() {
                return Err("legacy runtime state contains a symlink".into());
            }
            let recognized = matches!(name.to_str(), Some("hub.db" | "hub.db-wal" | "hub.db-shm" | "dispatcher.pid" | "hub.db.dispatcher.pid" | "dispatcher.lock" | "dispatcher.log" | "tabs.jsonl" | "dispatcher.stopped" | "model-policy.json" | "clients" | "statusline-subagents" | "claude-runs"));
            if !recognized { continue; }
            let src = entry.path();
            restrict_tree(&src).map_err(|e| format!("legacy runtime state is unsafe: {e}"))?;
            let dst = home.join(&name);
            if dst.exists() { return Err(format!("migration destination already contains {}", name.to_string_lossy())); }
            if let Some(dir) = dst.parent() { ensure_private_dir(dir).map_err(|e| e.to_string())?; }
            if quarantine.is_none() {
                let mut i = 0u32;
                loop { let candidate = quarantine_root.join(format!("state-{i}")); if std::fs::create_dir(&candidate).is_ok() { quarantine = Some(candidate); break; } i += 1; if i == 1000 { return Err("legacy quarantine unavailable".into()); } }
            }
            let quarantine_path = quarantine.as_ref().unwrap().join(&name);
            copy_tree(&src, &dst).map_err(|e| e.to_string())?;
            std::fs::rename(&src, &quarantine_path).map_err(|e| e.to_string())?;
        }
        if legacy.read_dir().map_err(|e| e.to_string())?.next().is_none() { let _ = std::fs::remove_dir(legacy); }
        let marker = home.join(".migration-release");
        std::fs::write(&marker, COMPAT_RELEASE).map_err(|e| e.to_string())?;
        ensure_private_file(&marker).map_err(|e| e.to_string())?;
        Ok(true)
    })();
    result
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() { return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "symlink in migration")); }
    if meta.is_dir() { ensure_private_dir(dst)?; for e in std::fs::read_dir(src)? { let e = e?; copy_tree(&e.path(), &dst.join(e.file_name()))?; } }
    else { std::fs::copy(src, dst)?; ensure_private_file(dst)?; }
    Ok(())
}

pub fn dispatcher_pid_path() -> PathBuf {
    runtime_paths(&RuntimeHome {
        root: runtime_home(),
        legacy: false,
    })
    .pid
}

pub fn dispatcher_log_path() -> PathBuf {
    runtime_paths(&RuntimeHome {
        root: runtime_home(),
        legacy: false,
    })
    .log
}

pub fn dispatcher_lock_path() -> PathBuf {
    runtime_paths(&RuntimeHome {
        root: runtime_home(),
        legacy: false,
    })
    .lock
}

pub fn dispatcher_stopped_path() -> PathBuf {
    env_path("A2A_DISPATCHER_STOPPED").unwrap_or_else(|| runtime_home().join("dispatcher.stopped"))
}

pub fn clients_dir() -> PathBuf {
    runtime_paths(&RuntimeHome {
        root: runtime_home(),
        legacy: false,
    })
    .clients
}

pub fn tabs_path() -> PathBuf {
    runtime_paths(&RuntimeHome {
        root: runtime_home(),
        legacy: false,
    })
    .tabs
}

pub fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// `$A2A_STATUSLINE_DIR`, else `<runtime-home>/statusline-subagents`.
pub fn statusline_dir() -> PathBuf {
    env_path("A2A_STATUSLINE_DIR").unwrap_or_else(|| runtime_home().join("statusline-subagents"))
}

/// `$CLAUDE_CONFIG_DIR`, else `~/.claude`: where Claude Code keeps its transcripts under `projects/`.
pub fn claude_dir() -> PathBuf {
    env_path("CLAUDE_CONFIG_DIR").unwrap_or_else(|| {
        env_path("HOME")
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".claude")
    })
}

/// Where the Claude backend keeps one `<session>.json` (folder, started?) and `<session>.pid` per headless run:
/// `<db>.claude-runs`. Files, not memory, because `oc cancel` is another process than the dispatcher.
pub fn claude_runs_dir() -> PathBuf {
    env_path("A2A_CLAUDE_RUNS_DIR").unwrap_or_else(|| runtime_home().join("claude-runs"))
}

/// Create a runtime directory without ever adding group/world permissions.
pub fn ensure_private_dir(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode() & 0o700;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// Restrict an existing state file to the owner without broadening a restrictive mode.
pub fn ensure_private_file(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode() & 0o600;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_override_beats_home_and_defaults_are_client_neutral() {
        let home = PathBuf::from("/tmp/example-home");
        assert_eq!(
            runtime_home_from(None, Some(home.clone())),
            home.join(".codetypera2a")
        );
        let override_home = PathBuf::from("/tmp/a2a-home");
        assert_eq!(
            runtime_home_from(Some(override_home.clone()), Some(home)),
            override_home
        );
    }

    #[cfg(unix)]
    #[test]
    fn runtime_directories_and_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("a2a-paths-{}", std::process::id()));
        let dir = root.join("nested");
        let file = dir.join("state");
        let _ = std::fs::remove_dir_all(&root);
        ensure_private_dir(&dir).unwrap();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::write(&file, "state").unwrap();
        ensure_private_file(&file).unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_home_moves_atomically_and_fallback_expires_by_release_marker() {
        let root = std::env::temp_dir().join(format!("a2a-migration-{}", std::process::id()));
        let legacy = root.join("legacy");
        let home = root.join("home");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(legacy.join("clients")).unwrap();
        std::fs::write(legacy.join("hub.db"), "db").unwrap();
        std::fs::write(legacy.join("hub.db-wal"), "wal").unwrap();
        std::fs::write(legacy.join("clients/one"), "client").unwrap();
        assert!(migrate_legacy(&home, &legacy).unwrap());
        assert!(!legacy.exists());
        assert!(home.join("hub.db-wal").exists());
        assert_eq!(
            std::fs::read_to_string(home.join(".migration-release")).unwrap(),
            COMPAT_RELEASE
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_state_is_not_read_without_migration() {
        let root = std::env::temp_dir().join(format!("a2a-fallback-marker-{}", std::process::id()));
        let legacy = root.join("legacy");
        let home = root.join("home");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("hub.db"), "legacy-db").unwrap();
        assert!(!home.exists());
        std::fs::create_dir_all(&home).unwrap();
        assert!(!home.join("hub.db").exists());
        assert_eq!(
            std::fs::read_to_string(legacy.join("hub.db")).unwrap(),
            "legacy-db"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn migration_refuses_nonempty_destinations_without_deleting_legacy() {
        let root =
            std::env::temp_dir().join(format!("a2a-migration-conflict-{}", std::process::id()));
        let legacy = root.join("legacy");
        let home = root.join("home");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(legacy.join("hub.db"), "db").unwrap();
        std::fs::write(home.join("keep"), "keep").unwrap();
        assert!(migrate_legacy(&home, &legacy).is_err());
        assert!(legacy.join("hub.db").exists());
        std::fs::remove_dir_all(&home).unwrap();
        std::fs::write(root.join(".codetypera2a.migration.lock"), "locked").unwrap();
        assert!(migrate_legacy(&home, &legacy).is_err());
        std::fs::remove_file(root.join(".codetypera2a.migration.lock")).unwrap();
        std::fs::write(
            legacy.join("hub.db.dispatcher.pid"),
            std::process::id().to_string(),
        )
        .unwrap();
        assert!(migrate_legacy(&home, &legacy).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn migration_accepts_only_documented_scaffolding() {
        for marker in [".legacy-fallback-release", ".migration-release"] {
            let root =
                std::env::temp_dir().join(format!("a2a-scaffold-{}-{marker}", std::process::id()));
            let legacy = root.join("legacy");
            let home = root.join("home");
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&legacy).unwrap();
            std::fs::write(legacy.join("hub.db"), "db").unwrap();
            std::fs::create_dir_all(&home).unwrap();
            std::fs::write(home.join(marker), COMPAT_RELEASE).unwrap();
            assert!(migrate_legacy(&home, &legacy).unwrap());
            assert!(home.join("hub.db").exists());
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn live_legacy_never_selects_an_active_runtime_owner() {
        let root = std::env::temp_dir().join(format!("a2a-owner-{}", std::process::id()));
        let legacy = root.join("legacy");
        let canonical = root.join("canonical");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&canonical).unwrap();
        std::fs::write(canonical.join("sentinel"), "must-survive").unwrap();
        std::fs::write(
            legacy.join("hub.db.dispatcher.pid"),
            std::process::id().to_string(),
        )
        .unwrap();
        assert!(migrate_legacy(&canonical, &legacy).is_err());
        assert!(canonical.exists());
        assert_eq!(
            std::fs::read_to_string(canonical.join("sentinel")).unwrap(),
            "must-survive"
        );
        assert_eq!(
            std::fs::read_to_string(legacy.join("hub.db.dispatcher.pid")).unwrap(),
            std::process::id().to_string()
        );
        let owner = resolve_runtime_home(&canonical, &legacy);
        assert_eq!(owner.root, canonical);
        assert!(!owner.legacy);
        let pid = runtime_paths(&owner).pid;
        assert!(
            std::env::var_os("A2A_DISPATCHER_PID")
                .map_or(pid == canonical.join("dispatcher.pid"), |value| pid
                    == PathBuf::from(value))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn migration_rejects_relative_and_non_directory_roots_without_touching_destination() {
        let root = std::env::temp_dir().join(format!("a2a-unsafe-root-{}", std::process::id()));
        let legacy = root.join("legacy");
        let home = root.join("home");
        let relative = PathBuf::from(format!(".a2a-relative-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&relative);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("hub.db"), "db").unwrap();
        std::fs::write(&relative, "relative file").unwrap();
        assert!(migrate_legacy(&relative, &legacy).is_err());
        assert_eq!(std::fs::read_to_string(legacy.join("hub.db")).unwrap(), "db");
        std::fs::remove_file(&relative).unwrap();
        std::fs::write(&home, "not a directory").unwrap();
        assert!(migrate_legacy(&home, &legacy).is_err());
        assert_eq!(std::fs::read_to_string(legacy.join("hub.db")).unwrap(), "db");
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn migration_rejects_symlinked_runtime_entries() {
        use std::os::unix::fs::symlink;
        let root =
            std::env::temp_dir().join(format!("a2a-migration-symlink-{}", std::process::id()));
        let legacy = root.join("legacy");
        let outside = root.join("outside");
        let home = root.join("home");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(&outside, "outside").unwrap();
        std::fs::write(legacy.join("hub.db"), "db").unwrap();
        symlink(&outside, legacy.join("linked")).unwrap();
        assert!(migrate_legacy(&home, &legacy).is_err());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "outside");
        let _ = std::fs::remove_dir_all(root);
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::*;

    #[test]
    fn the_live_legacy_refusal_keeps_its_prefix_and_names_safe_context_only() {
        assert!(
            LEGACY_LIVE_REFUSAL
                .starts_with("legacy dispatcher is still running; migration refused")
        );
        assert!(LEGACY_LIVE_REFUSAL.contains("destination_touched=false"));
        assert!(!LEGACY_LIVE_REFUSAL.contains('/'));
    }
}
