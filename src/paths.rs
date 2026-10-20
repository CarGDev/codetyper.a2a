//! Where things live. Everything hangs off the opencode config dir, which `init` populates.
use std::path::PathBuf;

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// `$OPENCODE_CONFIG_DIR`, else `~/.config/opencode`.
pub fn config_dir() -> PathBuf {
    env_path("OPENCODE_CONFIG_DIR").unwrap_or_else(|| {
        let home = env_path("HOME").unwrap_or_else(|| PathBuf::from("."));
        home.join(".config").join("opencode")
    })
}

pub fn a2a_dir() -> PathBuf {
    config_dir().join("a2a")
}

/// `$A2A_DB`, else `<config>/a2a/hub.db`.
pub fn db_path() -> PathBuf {
    env_path("A2A_DB").unwrap_or_else(|| a2a_dir().join("hub.db"))
}

/// `$A2A_AGENT_DIR`, else `<config>/agents` (the opencode v2 location), falling back to an
/// existing legacy `<config>/agent`.
pub fn agent_dir() -> PathBuf {
    env_path("A2A_AGENT_DIR").unwrap_or_else(|| {
        let (new, old) = (config_dir().join("agents"), config_dir().join("agent"));
        if !new.exists() && old.exists() { old } else { new }
    })
}

pub fn policy_path() -> PathBuf {
    env_path("A2A_POLICY").unwrap_or_else(|| a2a_dir().join("model-policy.json"))
}

pub fn opencode_config_path() -> PathBuf {
    config_dir().join("opencode.jsonc")
}

pub fn dispatcher_pid_path() -> PathBuf {
    env_path("A2A_DISPATCHER_PID").unwrap_or_else(|| {
        let mut p = db_path().into_os_string();
        p.push(".dispatcher.pid");
        PathBuf::from(p)
    })
}

pub fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
