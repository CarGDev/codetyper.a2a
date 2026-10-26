//! Which credentials a headless `claude -p` turn should use: your subscription (`claude.ai` login), an
//! `ANTHROPIC_API_KEY`, or whichever exists. Claude Code itself prefers the key whenever it is in the
//! environment, so a key left in the shell silently bills the API instead of the subscription.
//!
//! `A2A_CLAUDE_AUTH` = `auto` (default) | `subscription` | `api`. `auto` uses the subscription when
//! `claude auth status` shows one and the key otherwise; if that cannot be read the environment is left alone.
use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const KEY_VAR: &str = "ANTHROPIC_API_KEY";
const MODE_VAR: &str = "A2A_CLAUDE_AUTH";
const LEGACY_KEEP_KEY_VAR: &str = "A2A_CLAUDE_API_KEY";
const SUBSCRIPTION_METHOD: &str = "claude.ai";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Auto,
    Subscription,
    Api,
}

pub fn mode_from(value: Option<&str>, keep_key_flag: bool) -> Mode {
    match value.map(str::trim) {
        Some("subscription") => Mode::Subscription,
        Some("api") => Mode::Api,
        Some("auto") | None | Some("") if keep_key_flag => Mode::Api,
        _ => Mode::Auto,
    }
}

/// What `claude auth status` said, read with the key removed so it reports the login, not the key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Subscription {
    Yes,
    No,
    Unknown,
}

pub fn parse_status(json: &str) -> Subscription {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return Subscription::Unknown;
    };
    let logged_in = v["loggedIn"].as_bool().unwrap_or(false);
    match (logged_in, v["authMethod"].as_str()) {
        (true, Some(SUBSCRIPTION_METHOD)) => Subscription::Yes,
        (_, Some(_)) => Subscription::No,
        _ => Subscription::Unknown,
    }
}

/// Should the key be taken out of the child's environment?
pub fn strip_key(mode: Mode, subscription: Subscription, key_in_env: bool) -> bool {
    if !key_in_env {
        return false;
    }
    match mode {
        Mode::Api => false,
        Mode::Subscription => true,
        Mode::Auto => subscription == Subscription::Yes,
    }
}

const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

/// `claude auth status` with the key removed, bounded in time so a stuck CLI never stalls the dispatcher.
fn read_subscription(bin: &str) -> Subscription {
    let Ok(mut child) = Command::new(bin)
        .args(["auth", "status"])
        .env_remove(KEY_VAR)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Subscription::Unknown;
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return Subscription::Unknown,
            Ok(None) if started.elapsed() > STATUS_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Subscription::Unknown;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    let mut text = String::new();
    child
        .stdout
        .take()
        .map(|mut out| out.read_to_string(&mut text));
    parse_status(&text)
}

pub struct Choice {
    pub strip_key: bool,
    pub summary: String,
}

/// Decide once, at dispatcher start, and say why in one line for the log.
pub fn choose(bin: &str) -> Choice {
    let mode = mode_from(
        std::env::var(MODE_VAR).ok().as_deref(),
        std::env::var(LEGACY_KEEP_KEY_VAR).is_ok_and(|v| v == "1"),
    );
    let key_in_env = std::env::var(KEY_VAR).is_ok_and(|v| !v.is_empty());
    // Only a key in the environment can compete with the subscription, so only then is it worth asking.
    let subscription = if key_in_env && mode == Mode::Auto {
        read_subscription(bin)
    } else {
        Subscription::Unknown
    };
    let strip = strip_key(mode, subscription, key_in_env);
    let using = match (strip, key_in_env, subscription) {
        (true, _, _) => "subscription",
        (false, true, Subscription::Yes) => "API key (A2A_CLAUDE_AUTH=api)",
        (false, true, _) => "API key",
        (false, false, Subscription::Yes) => "subscription",
        (false, false, _) => "claude's own login",
    };
    Choice {
        strip_key: strip,
        summary: format!(
            "claude auth: mode={mode:?} subscription={subscription:?} api_key_in_env={key_in_env} -> {using}"
        ),
    }
}

/// The `apiKeySource` the run reported in its init event (`none` = subscription login).
pub fn reported_source(stream: &str) -> Option<String> {
    stream
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| v["apiKeySource"].as_str().map(String::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_is_read_from_the_environment_with_a_legacy_flag() {
        assert_eq!(mode_from(None, false), Mode::Auto);
        assert_eq!(mode_from(Some("subscription"), false), Mode::Subscription);
        assert_eq!(mode_from(Some("api"), false), Mode::Api);
        assert_eq!(mode_from(Some("bogus"), false), Mode::Auto);
        assert_eq!(mode_from(None, true), Mode::Api);
    }

    #[test]
    fn claude_auth_status_is_classified() {
        assert_eq!(
            parse_status(r#"{"loggedIn":true,"authMethod":"claude.ai"}"#),
            Subscription::Yes
        );
        assert_eq!(
            parse_status(r#"{"loggedIn":true,"authMethod":"api_key"}"#),
            Subscription::No
        );
        assert_eq!(
            parse_status(r#"{"loggedIn":false,"authMethod":"claude.ai"}"#),
            Subscription::No
        );
        assert_eq!(parse_status("not json"), Subscription::Unknown);
    }

    #[test]
    fn the_key_is_removed_only_when_the_subscription_should_win() {
        use Subscription::{No, Unknown, Yes};
        assert!(strip_key(Mode::Auto, Yes, true), "both: subscription wins");
        assert!(!strip_key(Mode::Auto, No, true), "only a key: use it");
        assert!(
            !strip_key(Mode::Auto, Unknown, true),
            "cannot tell: leave the environment alone"
        );
        assert!(strip_key(Mode::Subscription, No, true));
        assert!(!strip_key(Mode::Api, Yes, true));
        assert!(
            !strip_key(Mode::Auto, Yes, false),
            "no key, nothing to remove"
        );
    }

    #[test]
    fn the_run_reports_which_credential_it_used() {
        let log = "{\"type\":\"system\",\"hook_event\":\"x\"}\n{\"type\":\"system\",\"subtype\":\"init\",\"apiKeySource\":\"none\"}\n";
        assert_eq!(reported_source(log).as_deref(), Some("none"));
        assert_eq!(reported_source("{}"), None);
    }
}
