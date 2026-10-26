//! Pure backend selection. Identity is supplied by the transport adapter, never a tool argument.

use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendKind {
    Opencode,
    Claude,
}

impl BackendKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Opencode => "opencode",
            Self::Claude => "claude",
        }
    }

    pub fn stored_name(self) -> Option<&'static str> {
        (self == Self::Claude).then_some("claude")
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim() {
            "opencode" => Some(Self::Opencode),
            "claude" => Some(Self::Claude),
            _ => None,
        }
    }

    /// `NULL` and an absent backend column are the legacy OpenCode representation.
    pub fn from_stored(name: Option<&str>) -> Option<Self> {
        match name.map(str::trim) {
            None | Some("") => Some(Self::Opencode),
            Some(name) => Self::from_name(name),
        }
    }
}

/// Authenticated metadata parsed by the MCP adapter. `Unknown` includes absent, malformed, and
/// conflicting metadata; it is deliberately never treated as Claude.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallerIdentity {
    Unknown,
    Known {
        runtime: BackendKind,
        session_id: Option<String>,
    },
}

impl CallerIdentity {
    /// Construct identity from the target-owned process marker. Ordinary request metadata must
    /// never be used as a substitute for this boundary.
    pub fn from_runtime_env() -> Self {
        let Ok(runtime) = std::env::var("A2A_RUNTIME") else {
            return Self::Unknown;
        };
        if runtime.len() > 32 || runtime.trim() != runtime {
            return Self::Unknown;
        }
        let session_id = match std::env::var("A2A_SESSION_ID") {
            Ok(value) if !value.is_empty() && value.len() <= 256 => Some(value),
            Ok(_) => return Self::Unknown,
            Err(_) => None,
        };
        Self::from_transport(Some(&runtime), session_id)
    }

    pub fn from_transport(runtime: Option<&str>, session_id: Option<String>) -> Self {
        runtime
            .and_then(BackendKind::from_name)
            .map_or(Self::Unknown, |runtime| Self::Known {
                runtime,
                session_id,
            })
    }

    /// Parse transport-owned MCP metadata. The adapter must pass `_meta`, not `arguments`, here.
    /// Multiple aliases are accepted for protocol adapters, but conflicting values are rejected.
    pub fn from_mcp_meta(meta: Option<&Value>) -> Self {
        let Some(Value::Object(meta)) = meta else {
            return Self::Unknown;
        };
        let mut runtimes = Vec::new();
        for key in ["runtime", "backend", "client"] {
            let Some(value) = meta.get(key) else { continue };
            let Some(value) = value.as_str() else {
                return Self::Unknown;
            };
            runtimes.push(value);
        }
        if runtimes.windows(2).any(|pair| pair[0] != pair[1]) {
            return Self::Unknown;
        }
        let mut session_id = Vec::new();
        for key in ["sessionId", "session_id"] {
            let Some(value) = meta.get(key) else { continue };
            let Some(value) = value.as_str() else {
                return Self::Unknown;
            };
            session_id.push(value);
        }
        if session_id.windows(2).any(|pair| pair[0] != pair[1]) {
            return Self::Unknown;
        }
        Self::from_transport(
            runtimes.first().copied(),
            session_id.first().map(|s| (*s).to_owned()),
        )
    }

    /// Merge optional adapter metadata without allowing it to establish or replace identity.
    pub fn merge_mcp_meta(&self, meta: Option<&Value>) -> Self {
        let Some(Value::Object(values)) = meta else {
            return self.clone();
        };
        if !["runtime", "backend", "client", "sessionId", "session_id"]
            .iter()
            .any(|key| values.contains_key(*key))
        {
            return self.clone();
        }
        let metadata = Self::from_mcp_meta(meta);
        let Self::Known {
            runtime,
            session_id,
        } = self
        else {
            return Self::Unknown;
        };
        let Self::Known {
            runtime: metadata_runtime,
            session_id: metadata_session,
        } = metadata
        else {
            return Self::Unknown;
        };
        if runtime != &metadata_runtime
            || session_id.is_some() && metadata_session.is_some() && session_id != &metadata_session
        {
            return Self::Unknown;
        }
        Self::Known {
            runtime: *runtime,
            session_id: session_id.clone().or(metadata_session),
        }
    }

    pub fn is_claude(&self) -> bool {
        self.runtime() == Some(BackendKind::Claude)
    }

    fn runtime(&self) -> Option<BackendKind> {
        match self {
            Self::Known { runtime, .. } => Some(*runtime),
            Self::Unknown => None,
        }
    }

    fn session(&self) -> Option<&str> {
        match self {
            Self::Known { session_id, .. } => session_id.as_deref(),
            Self::Unknown => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolutionSource {
    AgentOverride,
    GlobalOverride,
    Stored,
    AutoOpencode,
    AutoClaude,
    SafeDefault,
}

impl ResolutionSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::AgentOverride => "agent-override",
            Self::GlobalOverride => "global-override",
            Self::Stored => "stored",
            Self::AutoOpencode => "auto-opencode",
            Self::AutoClaude => "auto-claude",
            Self::SafeDefault => "safe-default",
        }
    }
}

pub struct ResolveInput<'a> {
    pub agent: &'a str,
    pub per_agent: Option<&'a str>,
    pub global: Option<&'a str>,
    /// `None` means no stored association; `Some(None)` means an existing NULL OpenCode row.
    pub stored: Option<Option<&'a str>>,
    pub explicit_session: Option<&'a str>,
    pub identity: &'a CallerIdentity,
    pub detected_opencode_session: Option<&'a str>,
    pub claude_available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolution {
    pub backend: BackendKind,
    pub source: ResolutionSource,
    pub caller_session: Option<String>,
    pub diagnostic: String,
}

fn valid(name: Option<&str>) -> Option<BackendKind> {
    name.and_then(BackendKind::from_name)
}

pub fn resolve_backend(input: ResolveInput<'_>) -> Resolution {
    let explicit = input.explicit_session.map(str::to_owned);
    let mut reasons = Vec::new();
    let choose = |backend: BackendKind, source: ResolutionSource, diagnostic: &str| Resolution {
        backend,
        source,
        caller_session: explicit.clone(),
        diagnostic: diagnostic.to_owned(),
    };
    if let Some(kind) = valid(input.per_agent) {
        return choose(kind, ResolutionSource::AgentOverride, "agent override");
    }
    if input.per_agent.is_some() {
        reasons.push("invalid agent override");
    }
    if let Some(kind) = valid(input.global) {
        return choose(kind, ResolutionSource::GlobalOverride, "global override");
    }
    if input.global.is_some() {
        reasons.push("invalid global override");
    }
    if let Some(kind) = input.stored.and_then(BackendKind::from_stored) {
        return choose(kind, ResolutionSource::Stored, "stored association");
    }
    if input.stored.is_some() {
        reasons.push("invalid stored backend");
    }
    if input.identity.runtime() == Some(BackendKind::Claude) && input.claude_available {
        return choose(
            BackendKind::Claude,
            ResolutionSource::AutoClaude,
            "auto-claude",
        );
    }
    if input.identity.runtime() == Some(BackendKind::Claude) {
        reasons.push("claude unavailable");
    }
    if input.identity.runtime() == Some(BackendKind::Opencode) {
        let mut result = choose(
            BackendKind::Opencode,
            ResolutionSource::AutoOpencode,
            "auto-opencode",
        );
        result.caller_session = explicit
            .or_else(|| input.identity.session().map(str::to_owned))
            .or_else(|| input.detected_opencode_session.map(str::to_owned));
        return result;
    }
    let diagnostic = if reasons.is_empty() {
        "safe default".to_owned()
    } else {
        reasons.join("; ")
    };
    let mut result = choose(
        BackendKind::Opencode,
        ResolutionSource::SafeDefault,
        &diagnostic,
    );
    result.caller_session = explicit.or_else(|| input.detected_opencode_session.map(str::to_owned));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    static UNKNOWN: CallerIdentity = CallerIdentity::Unknown;

    fn input<'a>() -> ResolveInput<'a> {
        ResolveInput {
            agent: "x",
            per_agent: None,
            global: None,
            stored: None,
            explicit_session: None,
            identity: &UNKNOWN,
            detected_opencode_session: None,
            claude_available: true,
        }
    }

    #[test]
    fn precedence_and_stored_conversion() {
        let identity = CallerIdentity::from_transport(Some("claude"), Some("opaque".into()));
        let mut value = input();
        value.per_agent = Some("claude");
        value.global = Some("opencode");
        value.identity = &identity;
        assert_eq!(
            resolve_backend(value).source,
            ResolutionSource::AgentOverride
        );
        assert_eq!(BackendKind::from_stored(None), Some(BackendKind::Opencode));
        assert_eq!(BackendKind::Claude.stored_name(), Some("claude"));
    }

    #[test]
    fn unknown_invalid_and_unavailable_are_safe() {
        let mut value = input();
        value.per_agent = Some("secret");
        value.explicit_session = Some("opaque-session");
        let result = resolve_backend(value);
        assert_eq!(result.source, ResolutionSource::SafeDefault);
        assert!(
            !result.diagnostic.contains("secret") && !result.diagnostic.contains("opaque-session")
        );
        let identity = CallerIdentity::from_transport(Some("claude"), None);
        let mut value = input();
        value.identity = &identity;
        value.claude_available = false;
        assert_eq!(resolve_backend(value).source, ResolutionSource::SafeDefault);
    }

    #[test]
    fn metadata_conflicts_become_unknown_and_explicit_session_wins() {
        assert_eq!(
            CallerIdentity::from_mcp_meta(Some(
                &serde_json::json!({"runtime":"claude", "backend":"opencode"})
            )),
            CallerIdentity::Unknown
        );
        assert_eq!(
            CallerIdentity::from_mcp_meta(Some(
                &serde_json::json!({"runtime":"claude", "backend":123})
            )),
            CallerIdentity::Unknown
        );
        let identity = CallerIdentity::from_transport(Some("opencode"), Some("detected".into()));
        let mut value = input();
        value.identity = &identity;
        value.explicit_session = Some("explicit");
        value.detected_opencode_session = Some("detected");
        assert_eq!(
            resolve_backend(value).caller_session.as_deref(),
            Some("explicit")
        );
    }

    #[test]
    fn trusted_identity_rejects_conflicting_adapter_metadata() {
        let identity = CallerIdentity::from_transport(Some("claude"), None);
        assert_eq!(
            identity.merge_mcp_meta(Some(&serde_json::json!({"runtime": "claude"}))),
            identity
        );
        assert_eq!(
            identity.merge_mcp_meta(Some(&serde_json::json!({"runtime": "opencode"}))),
            CallerIdentity::Unknown
        );
        assert_eq!(
            CallerIdentity::Unknown.merge_mcp_meta(Some(&serde_json::json!({"runtime": "claude"}))),
            CallerIdentity::Unknown
        );
    }

    #[test]
    fn marker_diagnostics_are_categories_only() {
        let identity = CallerIdentity::from_transport(Some("claude"), None);
        let mut value = input();
        value.identity = &identity;
        value.claude_available = false;
        let result = resolve_backend(value);
        assert_eq!(result.source, ResolutionSource::SafeDefault);
        assert!(!result.diagnostic.contains("session"));
    }
}
