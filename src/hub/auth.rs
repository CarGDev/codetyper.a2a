//! Connection-scoped caller authentication for the hub.
//!
//! The trusted runtime issues one signed credential per orchestrator connection and hands it to
//! the hub child at launch (one-shot 0600 file, read once and unlinked). The hub verifies it once;
//! tool arguments, `from`, `_meta` and environment values never establish identity.
//! Enforcement is on when a trusted-keys file exists in the runtime home; with no keys file the
//! hub keeps its legacy behavior (`Gate::Open`). With keys but no valid credential every
//! orchestrator-role action is denied (`Gate::Enforced(None)`).
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

pub const ISSUER: &str = "codetypera2a-runtime";
pub const AUDIENCE: &str = "codetyper-hub";
pub const PRIMARY_AGENT: &str = "orchestrator";
pub const FORBIDDEN_TARGET: &str = "gentle-orchestrator";
pub const CLOCK_SKEW_SECS: i64 = 60;
pub const MAX_TTL_SECS: i64 = 15 * 60;
pub const REGISTRY_CAPACITY: usize = 1024;
const MAX_CREDENTIAL_BYTES: usize = 4096;
const MAX_KEYS: usize = 2;
const PREFIX: &str = "a2a1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthError {
    Missing,
    Malformed,
    UnknownKey,
    BadSignature,
    WrongIssuer,
    WrongAudience,
    UnsupportedVersion,
    WrongAgent,
    Expired,
    NotYetValid,
    TtlTooLong,
    Replayed,
    Revoked,
    RegistryFull,
    SessionNotLive,
}

impl AuthError {
    /// Stable, non-secret category shown to callers.
    pub fn category(self) -> &'static str {
        match self {
            Self::Missing => "credential missing",
            Self::Malformed => "credential malformed",
            Self::UnknownKey => "credential key unknown",
            Self::BadSignature => "credential signature invalid",
            Self::WrongIssuer => "credential issuer rejected",
            Self::WrongAudience => "credential audience rejected",
            Self::UnsupportedVersion => "credential version unsupported",
            Self::WrongAgent => "credential agent is not the primary orchestrator",
            Self::Expired => "credential expired",
            Self::NotYetValid => "credential not yet valid",
            Self::TtlTooLong => "credential lifetime too long",
            Self::Replayed => "credential replayed",
            Self::Revoked => "credential revoked",
            Self::RegistryFull => "credential registry full",
            Self::SessionNotLive => "primary session could not be validated",
        }
    }
}

#[derive(Deserialize)]
struct Claims {
    v: u8,
    iss: String,
    aud: String,
    sub: String,
    agent: String,
    project: String,
    iat: i64,
    exp: i64,
    jti: String,
    kid: String,
}

/// Immutable identity captured once per connection. Fields are private on purpose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedCaller {
    agent: String,
    project: String,
    session: String,
    exp: i64,
}

impl AuthenticatedCaller {
    pub fn agent(&self) -> &str {
        &self.agent
    }
}

pub struct TrustedKeys {
    keys: Vec<(String, VerifyingKey)>,
    revoked: HashSet<String>,
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

impl TrustedKeys {
    /// Lines of `<kid> <64 hex chars>`; `#` comments allowed. Current plus previous key only.
    pub fn parse(keys: &str, revoked: &str) -> Result<Self, AuthError> {
        let mut parsed = Vec::new();
        for line in keys
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            let mut fields = line.split_whitespace();
            let (Some(kid), Some(hex), None) = (fields.next(), fields.next(), fields.next()) else {
                return Err(AuthError::Malformed);
            };
            if parsed.iter().any(|(known, _)| known == kid) {
                return Err(AuthError::Malformed);
            }
            let raw: [u8; 32] = unhex(hex)
                .and_then(|b| b.try_into().ok())
                .ok_or(AuthError::Malformed)?;
            let key = VerifyingKey::from_bytes(&raw).map_err(|_| AuthError::Malformed)?;
            parsed.push((kid.to_string(), key));
        }
        if parsed.is_empty() || parsed.len() > MAX_KEYS {
            return Err(AuthError::Malformed);
        }
        let revoked = revoked
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        Ok(Self {
            keys: parsed,
            revoked,
        })
    }
}

/// In-process replay registry: bounded, never persisted, expired entries pruned first.
#[derive(Default)]
pub struct Registry {
    seen: Mutex<HashMap<String, i64>>,
}

impl Registry {
    pub fn register(&self, jti: &str, exp: i64, now: i64) -> Result<(), AuthError> {
        let mut seen = self.seen.lock().map_err(|_| AuthError::Replayed)?;
        seen.retain(|_, until| *until + CLOCK_SKEW_SECS >= now);
        if seen.contains_key(jti) {
            return Err(AuthError::Replayed);
        }
        if seen.len() >= REGISTRY_CAPACITY {
            return Err(AuthError::RegistryFull);
        }
        seen.insert(jti.to_string(), exp);
        Ok(())
    }
}

/// Verify a credential once. `session_live(sub, project)` must consult the hub's own session rows.
pub fn authenticate_connection(
    credential: &str,
    keys: &TrustedKeys,
    registry: &Registry,
    now: i64,
    session_live: &dyn Fn(&str, &str) -> bool,
) -> Result<AuthenticatedCaller, AuthError> {
    if credential.is_empty() {
        return Err(AuthError::Missing);
    }
    if credential.len() > MAX_CREDENTIAL_BYTES {
        return Err(AuthError::Malformed);
    }
    let mut parts = credential.trim().split('.');
    let (Some(PREFIX), Some(payload_hex), Some(sig_hex), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(AuthError::Malformed);
    };
    let payload = unhex(payload_hex).ok_or(AuthError::Malformed)?;
    let sig_bytes: [u8; 64] = unhex(sig_hex)
        .and_then(|b| b.try_into().ok())
        .ok_or(AuthError::Malformed)?;
    let claims: Claims = serde_json::from_slice(&payload).map_err(|_| AuthError::Malformed)?;
    let (_, key) = keys
        .keys
        .iter()
        .find(|(kid, _)| *kid == claims.kid)
        .ok_or(AuthError::UnknownKey)?;
    key.verify_strict(&payload, &Signature::from_bytes(&sig_bytes))
        .map_err(|_| AuthError::BadSignature)?;
    check_claims(&claims, now)?;
    if keys.revoked.contains(&claims.jti) {
        return Err(AuthError::Revoked);
    }
    if !session_live(&claims.sub, &claims.project) {
        return Err(AuthError::SessionNotLive);
    }
    registry.register(&claims.jti, claims.exp, now)?;
    Ok(AuthenticatedCaller {
        agent: claims.agent,
        project: claims.project,
        session: claims.sub,
        exp: claims.exp,
    })
}

fn check_claims(c: &Claims, now: i64) -> Result<(), AuthError> {
    if c.v != 1 {
        return Err(AuthError::UnsupportedVersion);
    }
    if c.iss != ISSUER {
        return Err(AuthError::WrongIssuer);
    }
    if c.aud != AUDIENCE {
        return Err(AuthError::WrongAudience);
    }
    if c.agent != PRIMARY_AGENT {
        return Err(AuthError::WrongAgent);
    }
    if c.sub.is_empty() || c.jti.is_empty() || c.project.is_empty() {
        return Err(AuthError::Malformed);
    }
    if c.exp.saturating_sub(c.iat) > MAX_TTL_SECS || c.exp <= c.iat {
        return Err(AuthError::TtlTooLong);
    }
    if c.iat > now + CLOCK_SKEW_SECS {
        return Err(AuthError::NotYetValid);
    }
    if now > c.exp + CLOCK_SKEW_SECS {
        return Err(AuthError::Expired);
    }
    Ok(())
}

/// Per-connection authorization state, decided once at connection setup.
#[derive(Clone, Debug)]
pub enum Gate {
    /// No trusted-keys file: legacy behavior.
    Open,
    /// Keys configured: orchestrator-role actions need the authenticated caller.
    Enforced(Option<AuthenticatedCaller>),
}

impl Gate {
    /// Re-checked on every gated call: the credential must still be inside its validity window and
    /// the primary session must still be live, so expiry, logout and a session that registers
    /// after the hub started are all honored without trusting connection-setup state.
    fn current(
        &self,
        now: i64,
        live: &dyn Fn(&str, &str) -> bool,
    ) -> Result<Option<&AuthenticatedCaller>, &'static str> {
        let Gate::Enforced(caller) = self else {
            return Ok(None);
        };
        let caller = caller
            .as_ref()
            .ok_or("primary session could not be validated")?;
        if now > caller.exp + CLOCK_SKEW_SECS {
            return Err("credential expired");
        }
        if !live(&caller.session, &caller.project) {
            return Err("primary session could not be validated");
        }
        Ok(Some(caller))
    }

    pub fn may_create(
        &self,
        from: &str,
        request_dir: Option<&str>,
        now: i64,
        live: &dyn Fn(&str, &str) -> bool,
    ) -> Result<(), String> {
        let Some(caller) = self
            .current(now, live)
            .map_err(|e| format!("task creation denied: {e}"))?
        else {
            return Ok(());
        };
        if caller.agent != from {
            return Err("task creation denied: caller is not the primary orchestrator".into());
        }
        let dir = request_dir.ok_or("task creation denied: project directory required")?;
        if !super::project::same_project(&caller.project, dir) {
            return Err(
                "task creation denied: project does not match the authenticated session".into(),
            );
        }
        Ok(())
    }

    /// Acting as the task's requester (re-open by reply, cancel) needs the same authenticated agent.
    pub fn may_act_as_requester(
        &self,
        requester: &str,
        who: &str,
        now: i64,
        live: &dyn Fn(&str, &str) -> bool,
    ) -> Result<(), String> {
        let Some(caller) = self
            .current(now, live)
            .map_err(|e| format!("denied: {e}"))?
        else {
            return Ok(());
        };
        if caller.agent == requester && who == requester {
            return Ok(());
        }
        Err("denied: caller is not the primary orchestrator".into())
    }

    /// Build the gate for a connection. No keys file means `Open`; any other problem with the
    /// keys, revocation list or credential means `Enforced(None)` (fail closed). The session row is
    /// checked per call (`current`), not here, so start-up order does not matter.
    pub fn load(home: &Path, credential_file: Option<&Path>, now: i64) -> Gate {
        let dir = home.join("auth");
        let keys_text = match std::fs::read_to_string(dir.join("trusted-keys")) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Gate::Open,
            Err(_) => return Gate::Enforced(None),
        };
        let revoked = match std::fs::read_to_string(dir.join("revoked-jti")) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(_) => return Gate::Enforced(None),
        };
        let Ok(keys) = TrustedKeys::parse(&keys_text, &revoked) else {
            return Gate::Enforced(None);
        };
        let Some(text) = credential_file.and_then(|f| take_one_shot(f, home)) else {
            return Gate::Enforced(None);
        };
        match authenticate_connection(&text, &keys, &Registry::default(), now, &|_, _| true) {
            Ok(caller) => Gate::Enforced(Some(caller)),
            Err(denied) => {
                eprintln!("a2a-hub: credential rejected: {}", denied.category());
                Gate::Enforced(None)
            }
        }
    }
}

/// Read a credential file once. Checks run on the opened descriptor, not the path: a regular file,
/// owner-only, owned by the runtime home's owner; a symlink is refused. The path is always unlinked.
#[cfg(unix)]
fn take_one_shot(path: &Path, owner_of: &Path) -> Option<String> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let is_link = std::fs::symlink_metadata(path)
        .ok()?
        .file_type()
        .is_symlink();
    let opened = (!is_link).then(|| std::fs::File::open(path).ok()).flatten();
    let _ = std::fs::remove_file(path);
    let file = opened?;
    let meta = file.metadata().ok()?;
    let owner = std::fs::metadata(owner_of).ok()?.uid();
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 || meta.uid() != owner {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_CREDENTIAL_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

#[cfg(not(unix))]
fn take_one_shot(_path: &Path, _owner_of: &Path) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const NOW: i64 = 1_000_000;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn keys_for(signing: &SigningKey) -> TrustedKeys {
        let line = format!("k1 {}", hex(signing.verifying_key().as_bytes()));
        TrustedKeys::parse(&line, "").unwrap()
    }

    fn claims(jti: &str) -> serde_json::Value {
        serde_json::json!({"v":1,"iss":ISSUER,"aud":AUDIENCE,"sub":"s1","agent":"orchestrator",
            "project":"/p","iat":NOW,"exp":NOW+600,"jti":jti,"kid":"k1"})
    }

    fn sign(signing: &SigningKey, claims: &serde_json::Value) -> String {
        let payload = claims.to_string();
        let sig = signing.sign(payload.as_bytes());
        format!(
            "{PREFIX}.{}.{}",
            hex(payload.as_bytes()),
            hex(&sig.to_bytes())
        )
    }

    fn check(c: &serde_json::Value) -> Result<AuthenticatedCaller, AuthError> {
        let s = key(1);
        authenticate_connection(
            &sign(&s, c),
            &keys_for(&s),
            &Registry::default(),
            NOW,
            &|_, _| true,
        )
    }

    #[test]
    fn a_valid_credential_yields_the_primary_caller() {
        let caller = check(&claims("j1")).unwrap();
        assert_eq!(caller.agent(), "orchestrator");
    }

    #[test]
    fn invalid_credentials_are_each_denied_with_their_own_category() {
        let bad = |f: &dyn Fn(&mut serde_json::Value)| {
            let mut c = claims("j");
            f(&mut c);
            check(&c).unwrap_err()
        };
        assert_eq!(bad(&|c| c["agent"] = "build".into()), AuthError::WrongAgent);
        assert_eq!(
            bad(&|c| c["agent"] = "gentle-orchestrator".into()),
            AuthError::WrongAgent
        );
        assert_eq!(bad(&|c| c["iss"] = "x".into()), AuthError::WrongIssuer);
        assert_eq!(bad(&|c| c["aud"] = "x".into()), AuthError::WrongAudience);
        assert_eq!(bad(&|c| c["v"] = 2.into()), AuthError::UnsupportedVersion);
        assert_eq!(bad(&|c| c["kid"] = "zz".into()), AuthError::UnknownKey);
        assert_eq!(
            bad(&|c| {
                c["iat"] = (NOW - 1000).into();
                c["exp"] = (NOW - 120).into();
            }),
            AuthError::Expired
        );
        assert_eq!(
            bad(&|c| c["iat"] = (NOW + 500).into()),
            AuthError::NotYetValid
        );
        assert_eq!(
            bad(&|c| c["exp"] = (NOW + 10_000).into()),
            AuthError::TtlTooLong
        );
        assert_eq!(bad(&|c| c["jti"] = "".into()), AuthError::Malformed);
    }

    #[test]
    fn a_wrong_key_garbage_and_oversize_are_denied() {
        let c = claims("j");
        let forged = sign(&key(9), &c);
        assert_eq!(
            authenticate_connection(
                &forged,
                &keys_for(&key(1)),
                &Registry::default(),
                NOW,
                &|_, _| true
            ),
            Err(AuthError::BadSignature)
        );
        let keys = keys_for(&key(1));
        let r = Registry::default();
        assert_eq!(
            authenticate_connection("", &keys, &r, NOW, &|_, _| true),
            Err(AuthError::Missing)
        );
        assert_eq!(
            authenticate_connection("nope", &keys, &r, NOW, &|_, _| true),
            Err(AuthError::Malformed)
        );
        let big = "a".repeat(MAX_CREDENTIAL_BYTES + 1);
        assert_eq!(
            authenticate_connection(&big, &keys, &r, NOW, &|_, _| true),
            Err(AuthError::Malformed)
        );
    }

    #[test]
    fn replay_revocation_and_dead_sessions_are_denied() {
        let s = key(1);
        let keys = keys_for(&s);
        let cred = sign(&s, &claims("j1"));
        let reg = Registry::default();
        assert!(authenticate_connection(&cred, &keys, &reg, NOW, &|_, _| true).is_ok());
        assert_eq!(
            authenticate_connection(&cred, &keys, &reg, NOW, &|_, _| true),
            Err(AuthError::Replayed)
        );
        assert_eq!(
            authenticate_connection(
                &sign(&s, &claims("j2")),
                &keys,
                &Registry::default(),
                NOW,
                &|_, _| false
            ),
            Err(AuthError::SessionNotLive)
        );
        let line = format!("k1 {}", hex(s.verifying_key().as_bytes()));
        let revoking = TrustedKeys::parse(&line, "j3\n").unwrap();
        assert_eq!(
            authenticate_connection(
                &sign(&s, &claims("j3")),
                &revoking,
                &Registry::default(),
                NOW,
                &|_, _| true
            ),
            Err(AuthError::Revoked)
        );
    }

    #[test]
    fn the_registry_is_bounded_and_prunes_expired_entries() {
        let reg = Registry::default();
        for i in 0..REGISTRY_CAPACITY {
            reg.register(&format!("j{i}"), NOW + 600, NOW).unwrap();
        }
        assert_eq!(
            reg.register("extra", NOW + 600, NOW),
            Err(AuthError::RegistryFull)
        );
        assert!(reg.register("later", NOW + 5000, NOW + 2000).is_ok());
    }

    #[test]
    fn key_rotation_accepts_current_and_previous_only() {
        let (a, b) = (key(1), key(2));
        let text = format!(
            "k1 {}\nk2 {}",
            hex(a.verifying_key().as_bytes()),
            hex(b.verifying_key().as_bytes())
        );
        assert!(TrustedKeys::parse(&text, "").is_ok());
        let three = format!("{text}\nk3 {}", hex(key(3).verifying_key().as_bytes()));
        assert_eq!(
            TrustedKeys::parse(&three, "").err(),
            Some(AuthError::Malformed)
        );
        assert_eq!(TrustedKeys::parse("", "").err(), Some(AuthError::Malformed));
    }

    fn caller() -> AuthenticatedCaller {
        AuthenticatedCaller {
            agent: "orchestrator".into(),
            project: "/p".into(),
            session: "s1".into(),
            exp: NOW + 600,
        }
    }

    #[test]
    fn the_gate_requires_the_authenticated_primary_for_creation() {
        let ok = Gate::Enforced(Some(caller()));
        let live = |_: &str, _: &str| true;
        assert!(
            ok.may_create("orchestrator", Some("/p"), NOW, &live)
                .is_ok()
        );
        assert!(ok.may_create("build", Some("/p"), NOW, &live).is_err());
        assert!(
            ok.may_create("orchestrator", Some("/other"), NOW, &live)
                .is_err()
        );
        assert!(ok.may_create("orchestrator", None, NOW, &live).is_err());
        let none = Gate::Enforced(None);
        assert!(
            none.may_create("orchestrator", Some("/p"), NOW, &live)
                .is_err()
        );
        assert!(Gate::Open.may_create("anyone", None, NOW, &live).is_ok());
        assert!(
            ok.may_act_as_requester("orchestrator", "orchestrator", NOW, &live)
                .is_ok()
        );
        assert!(
            none.may_act_as_requester("orchestrator", "orchestrator", NOW, &live)
                .is_err()
        );
    }

    #[test]
    fn expiry_and_a_dead_session_are_rechecked_on_every_call() {
        let gate = Gate::Enforced(Some(caller()));
        let live = |_: &str, _: &str| true;
        let dead = |_: &str, _: &str| false;
        assert!(
            gate.may_create(
                "orchestrator",
                Some("/p"),
                NOW + 600 + CLOCK_SKEW_SECS,
                &live
            )
            .is_ok()
        );
        let err = gate
            .may_create(
                "orchestrator",
                Some("/p"),
                NOW + 601 + CLOCK_SKEW_SECS,
                &live,
            )
            .unwrap_err();
        assert!(err.contains("expired"), "{err}");
        assert!(
            gate.may_create("orchestrator", Some("/p"), NOW, &dead)
                .is_err()
        );
        assert!(
            gate.may_act_as_requester("orchestrator", "orchestrator", NOW, &dead)
                .is_err()
        );
    }

    #[test]
    fn duplicate_key_ids_and_extra_fields_are_rejected() {
        let h = hex(key(1).verifying_key().as_bytes());
        assert!(TrustedKeys::parse(&format!("k1 {h}\nk1 {h}"), "").is_err());
        assert!(TrustedKeys::parse(&format!("k1 {h} extra"), "").is_err());
        assert!(TrustedKeys::parse(&format!("k1\t{h}"), "").is_ok());
    }

    #[test]
    fn load_is_open_only_when_the_keys_file_is_absent() {
        let dir = std::env::temp_dir().join(format!("a2a-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("auth")).unwrap();
        assert!(matches!(Gate::load(&dir, None, NOW), Gate::Open));
        let s = key(1);
        let keys = dir.join("auth/trusted-keys");
        std::fs::write(&keys, format!("k1 {}", hex(s.verifying_key().as_bytes()))).unwrap();
        assert!(matches!(Gate::load(&dir, None, NOW), Gate::Enforced(None)));
        // an unreadable keys path (a directory) fails closed, never open
        std::fs::remove_file(&keys).unwrap();
        std::fs::create_dir(&keys).unwrap();
        assert!(matches!(Gate::load(&dir, None, NOW), Gate::Enforced(None)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
