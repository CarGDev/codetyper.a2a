//! Wake an interactive Claude Code session that runs in a tmux pane.
//!
//! A headless `claude -p` cannot inject a prompt into an open session, so the `SessionStart` hook
//! records the pane the session lives in and the Claude backend types the nudge there instead.
//! Everything read back from disk or tmux is validated: ids and pane ids have a fixed shape, a
//! pane must still exist, and nothing is typed while the session is busy.
use crate::sys;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAX_ID_LEN: usize = 128;
const MAX_TEXT_CHARS: usize = 600;
const BUSY_MARKER: &str = "esc to interrupt";
const TAIL_LINES: &str = "-15";

fn safe_session(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn safe_pane(pane: &str) -> bool {
    pane.strip_prefix('%')
        .is_some_and(|n| !n.is_empty() && n.len() <= 9 && n.chars().all(|c| c.is_ascii_digit()))
}

fn pane_file(runs: &Path, session: &str) -> PathBuf {
    runs.join(format!("{session}.pane"))
}

/// Remember the pane of an interactive session. Headless runs never record one.
pub fn record_pane(runs: &Path, session: &str, pane: &str) -> bool {
    if !safe_session(session) || !safe_pane(pane) {
        return false;
    }
    if crate::paths::ensure_private_dir(runs).is_err() {
        return false;
    }
    let path = pane_file(runs, session);
    std::fs::write(&path, pane).is_ok() && crate::paths::ensure_private_file(&path).is_ok()
}

/// The recorded pane, only if it still exists.
pub fn pane_for(runs: &Path, session: &str) -> Option<String> {
    if !safe_session(session) {
        return None;
    }
    let pane = std::fs::read_to_string(pane_file(runs, session)).ok()?;
    let pane = pane.trim();
    if !safe_pane(pane) {
        return None;
    }
    let live = sys::tmux(&["display-message", "-p", "-t", pane, "#{pane_id}"]).ok()?;
    (live.trim() == pane).then(|| pane.to_string())
}

/// One short line: no control characters, no newlines, bounded length.
pub fn one_line(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    flat.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_TEXT_CHARS)
        .collect()
}

fn busy(screen: &str) -> bool {
    screen.to_lowercase().contains(BUSY_MARKER)
}

const PROMPT_MARK: char = '\u{276f}'; // the prompt character of Claude Code's input box
const FOOTER_MARKS: [&str; 3] = ["shift+tab", "? for shortcuts", "bypass permissions"];

fn is_rule(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.chars().all(|c| c == '\u{2500}' || c == '-')
}

/// Claude Code's own footer is on screen. Without it the pane may be a bare shell, where a pasted
/// nudge plus Enter would run as a command, so nothing is ever typed.
pub fn looks_like_claude(screen: &str) -> bool {
    let lower = screen.to_lowercase();
    let has_footer = FOOTER_MARKS.iter().any(|mark| lower.contains(mark));
    let has_prompt = screen
        .lines()
        .any(|l| l.trim_start().starts_with(PROMPT_MARK));
    has_footer && has_prompt
}

/// Is there already text in the input box (the user typing, or an earlier nudge not submitted)?
/// Everything between the last prompt line and the footer counts as input.
pub fn prompt_has_text(screen: &str) -> bool {
    let lines: Vec<&str> = screen.lines().collect();
    let Some(at) = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with(PROMPT_MARK))
    else {
        return false;
    };
    let first = lines[at]
        .trim_start()
        .trim_start_matches(PROMPT_MARK)
        .trim();
    if !first.is_empty() {
        return true;
    }
    lines[at + 1..]
        .iter()
        .take_while(|l| !FOOTER_MARKS.iter().any(|m| l.to_lowercase().contains(m)) && !is_rule(l))
        .any(|l| !l.trim().is_empty())
}

/// Minimum gap between two nudges to the same pane: a second one typed while the first is still
/// being taken in garbles the input box.
const COOLDOWN: Duration = Duration::from_secs(8);
const SUBMIT_DELAY: Duration = Duration::from_millis(250);
const SETTLE: Duration = Duration::from_millis(700);
const BUFFER: &str = "a2a-wake";
const NUDGE_PROBE_CHARS: usize = 24;

static LAST: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

fn cooling_down(pane: &str, now: Instant, last: &HashMap<String, Instant>) -> bool {
    last.get(pane)
        .is_some_and(|at| now.duration_since(*at) < COOLDOWN)
}

fn screen_of(pane: &str) -> Result<String, String> {
    sys::tmux(&["capture-pane", "-p", "-t", pane, "-S", TAIL_LINES])
}

/// Put the nudge into the input box in one piece (a bracketed paste, not a stream of keys) and submit it.
/// `Ok(false)` = not now (mid-turn, input box in use, or another nudge just went in): retry later.
pub fn inject(pane: &str, text: &str) -> Result<bool, String> {
    if !safe_pane(pane) {
        return Err("invalid tmux pane".into());
    }
    let mut guard = LAST.lock().map_err(|_| "wake state poisoned")?;
    let last = guard.get_or_insert_with(HashMap::new);
    if cooling_down(pane, Instant::now(), last) {
        return Ok(false);
    }
    let before = screen_of(pane)?;
    if !looks_like_claude(&before) {
        return Err("the pane no longer shows a Claude Code session".into());
    }
    if busy(&before) || prompt_has_text(&before) {
        return Ok(false);
    }
    last.insert(pane.to_string(), Instant::now());
    sys::tmux(&["set-buffer", "-b", BUFFER, "--", &one_line(text)])?;
    sys::tmux(&["paste-buffer", "-p", "-d", "-b", BUFFER, "-t", pane])?;
    std::thread::sleep(SUBMIT_DELAY);
    sys::tmux(&["send-keys", "-t", pane, "Enter"])?;
    std::thread::sleep(SETTLE);
    let after = screen_of(pane)?;
    let probe: String = one_line(text).chars().take(NUDGE_PROBE_CHARS).collect();
    if prompt_has_text(&after) && after.contains(&probe) && !busy(&after) {
        // our own text is still sitting unsubmitted in the input box: clear it, never the user's
        sys::tmux(&["send-keys", "-t", pane, "C-u"])?;
        return Ok(false);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn ids_and_panes_must_have_the_expected_shape() {
        assert!(safe_session("8bf9ed69-1a0d-4d23-978a-7b84b5a150c7"));
        assert!(!safe_session("../etc/passwd") && !safe_session("") && !safe_session("a b"));
        assert!(safe_pane("%12") && !safe_pane("12") && !safe_pane("%1;rm") && !safe_pane("%"));
    }

    #[test]
    fn a_pane_round_trips_only_for_valid_input() {
        let dir = std::env::temp_dir().join(format!("a2a-wake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(record_pane(&dir, "s1", "%3"));
        assert_eq!(std::fs::read_to_string(dir.join("s1.pane")).unwrap(), "%3");
        assert!(!record_pane(&dir, "../x", "%3"));
        assert!(!record_pane(&dir, "s1", "$(id)"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nudge_text_is_flattened_and_bounded() {
        assert_eq!(one_line("a\nb\t c\u{1b}[31m"), "a b c [31m");
        assert_eq!(one_line(&"x".repeat(5000)).chars().count(), MAX_TEXT_CHARS);
    }

    #[test]
    fn a_running_turn_is_never_typed_into() {
        assert!(busy("✻ Working… (12s · esc to interrupt)"));
        assert!(!busy("> \n  ? for shortcuts"));
    }

    #[test]
    fn text_in_the_input_box_is_detected_and_an_empty_box_is_not() {
        let empty = "done 8:17 PM\n\u{2500}\u{2500}\u{2500}\n\u{276f} \n\u{2500}\u{2500}\u{2500}\n  bypass permissions on (shift+tab to cycle)";
        let typed = "x\n\u{276f} half a mess\n  bypass permissions on (shift+tab to cycle)";
        let wrapped = "x\n\u{276f} \na2a__get_task {id:\"1\"}, judge the result,\n  then continue\n  bypass permissions on (shift+tab to cycle)";
        assert!(!prompt_has_text(empty));
        assert!(prompt_has_text(typed));
        assert!(prompt_has_text(wrapped));
        assert!(!prompt_has_text("no prompt on screen"));
    }

    #[test]
    fn a_bare_shell_is_not_mistaken_for_claude() {
        assert!(looks_like_claude("x\n\u{276f} \n  ? for shortcuts"));
        assert!(looks_like_claude(
            "\u{276f} \n  bypass permissions on (shift+tab to cycle)"
        ));
        assert!(!looks_like_claude("user@host ~ % \n\u{276f} ls"));
        assert!(!looks_like_claude(
            "shift+tab to cycle (text from a file)\nuser@host ~ % "
        ));
    }

    #[test]
    fn a_pane_cools_down_after_a_nudge() {
        let now = Instant::now();
        let mut last = HashMap::new();
        assert!(!cooling_down("%1", now, &last));
        last.insert("%1".to_string(), now);
        assert!(cooling_down("%1", now + Duration::from_secs(1), &last));
        assert!(!cooling_down("%1", now + COOLDOWN, &last));
        assert!(!cooling_down("%2", now, &last));
    }

    /// Against a real tmux pane running `cat`: the nudge arrives whole, once, and is submitted.
    #[test]
    fn a_nudge_is_pasted_whole_into_a_real_pane() {
        if Command::new("tmux").arg("-V").output().is_err() {
            return; // tmux is not installed here
        }
        let name = format!("a2a-wake-test-{}", std::process::id());
        let started = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &name,
                "-x",
                "200",
                "-y",
                "20",
                "sh",
                "-c",
                "echo '\u{276f} '; echo '? for shortcuts'; cat",
            ])
            .status();
        if !started.is_ok_and(|s| s.success()) {
            return;
        }
        let pane = sys::tmux(&["display-message", "-p", "-t", &name, "#{pane_id}"]).unwrap();
        let pane = pane.trim().to_string();
        let text = "A2A: task 1234 that you sent to \"mentor\" is now completed. Call get_task {id:\"1234\"}, judge the result.";
        let sent = inject(&pane, text);
        std::thread::sleep(Duration::from_millis(300));
        let screen = screen_of(&pane).unwrap_or_default();
        let second = inject(&pane, text); // inside the cooldown
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &name])
            .status();
        assert_eq!(sent, Ok(true));
        assert_eq!(second, Ok(false), "a second nudge waits out the cooldown");
        assert!(
            screen.matches(text).count() >= 1,
            "nudge missing or garbled: {screen}"
        );
    }
}
