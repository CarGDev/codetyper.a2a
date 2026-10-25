//! Claude Code usage for `oc stats --model`: the transcripts under `<claude dir>/projects/**.jsonl` hold one
//! `assistant` line per content block with the message's `usage`; this turns them into `claude-code` usage events.
//!
//! One event per message id. A message streamed over several lines repeats its id (and usage), so within one read
//! the largest `output_tokens` wins; across runs the first stored row wins (`events.ref` is unique per source).
//! Each file is read from the byte offset where the last run stopped, only up to its last newline, so a line that
//! is still being written is picked up by the next run.
use crate::hub::db::{self, SourcedUsage};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const SOURCE: &str = "claude-code";

/// Anthropic's published prompt-cache multipliers of the base input price: a read costs 0.1x, a 5-minute write 1.25x,
/// a 1-hour write 2x.
const CACHE_READ_X: f64 = 0.1;
const CACHE_WRITE_5M_X: f64 = 1.25;
const CACHE_WRITE_1H_X: f64 = 2.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub id: String,
    pub ts: String,
    pub session: String,
    pub model: String,
    /// Non-cached input only (`input_tokens`); cache traffic is kept apart.
    pub input: i64,
    pub output: i64,
    pub cache_write: i64,
    /// The part of `cache_write` that was a 1-hour write; the rest is priced as a 5-minute write.
    pub cache_write_1h: i64,
    pub cache_read: i64,
}

impl Turn {
    /// USD estimate from the published price table and the cache multipliers; None when the model has no price.
    pub fn cost_usd(&self) -> Option<f64> {
        let (pin, pout) = crate::models::price(&price_name(&self.model))?;
        let write_5m = self.cache_write - self.cache_write_1h;
        let input_side = self.input as f64
            + self.cache_read as f64 * CACHE_READ_X
            + write_5m as f64 * CACHE_WRITE_5M_X
            + self.cache_write_1h as f64 * CACHE_WRITE_1H_X;
        Some((input_side * pin + self.output as f64 * pout) / 1e6)
    }
}

/// `claude-sonnet-5-5-20261001` -> `claude-sonnet-5.5`: the price table spells versions with a dot and has no dates.
fn price_name(model: &str) -> String {
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let undated = match model.rsplit_once('-') {
        Some((head, date)) if date.len() == 8 && all_digits(date) => head,
        _ => model,
    };
    match undated
        .rsplit_once('-')
        .and_then(|(head, minor)| Some((head.rsplit_once('-')?, minor)))
    {
        Some(((front, major), minor)) if all_digits(major) && all_digits(minor) => {
            format!("{front}-{major}.{minor}")
        }
        _ => undated.to_string(),
    }
}

/// The usage of one transcript line; None for anything that is not an `assistant` message with usage and a real model.
pub fn parse_line(line: &str) -> Option<Turn> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v["type"] != "assistant" {
        return None;
    }
    let msg = &v["message"];
    let model = msg["model"]
        .as_str()
        .filter(|m| !m.is_empty() && *m != "<synthetic>")?;
    let id = msg["id"].as_str().or_else(|| v["uuid"].as_str())?;
    let usage = msg["usage"].as_object()?;
    let n = |k: &str| usage.get(k).and_then(Value::as_i64).unwrap_or(0);
    Some(Turn {
        id: id.to_string(),
        ts: v["timestamp"].as_str().unwrap_or_default().to_string(),
        session: v["sessionId"].as_str().unwrap_or_default().to_string(),
        model: model.to_string(),
        input: n("input_tokens"),
        output: n("output_tokens"),
        cache_write: n("cache_creation_input_tokens"),
        cache_write_1h: msg["usage"]["cache_creation"]["ephemeral_1h_input_tokens"]
            .as_i64()
            .unwrap_or(0)
            .clamp(0, n("cache_creation_input_tokens")),
        cache_read: n("cache_read_input_tokens"),
    })
}

/// The turns of the complete lines in `bytes` (one per message id, largest output wins) and how many bytes were consumed.
pub fn parse_chunk(bytes: &[u8]) -> (Vec<Turn>, usize) {
    let Some(end) = bytes.iter().rposition(|b| *b == b'\n').map(|i| i + 1) else {
        return (Vec::new(), 0);
    };
    let mut turns: Vec<Turn> = Vec::new();
    for line in bytes[..end].split(|b| *b == b'\n') {
        let Some(t) = parse_line(&String::from_utf8_lossy(line)) else {
            continue;
        };
        match turns.iter_mut().find(|k| k.id == t.id) {
            Some(known) if known.output >= t.output => {}
            Some(known) => *known = t,
            None => turns.push(t),
        }
    }
    (turns, end)
}

/// Every `*.jsonl` under `<claude dir>/projects`, subagent transcripts included. Unreadable folders are skipped.
fn transcripts(claude_dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut todo = vec![claude_dir.join("projects")];
    while let Some(dir) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for path in entries.flatten().map(|e| e.path()) {
            match path.is_dir() {
                true => todo.push(path),
                false if path.extension().is_some_and(|x| x == "jsonl") => found.push(path),
                false => {}
            }
        }
    }
    found.sort();
    found
}

/// The bytes of `path` from `offset`; a file shorter than `offset` was replaced and is read from the start.
fn read_from(path: &Path, offset: u64) -> std::io::Result<(Vec<u8>, u64)> {
    let mut f = std::fs::File::open(path)?;
    let start = if f.metadata()?.len() < offset {
        0
    } else {
        offset
    };
    f.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    Ok((bytes, start))
}

fn stored_offset(conn: &Connection, path: &str) -> u64 {
    let sql = "SELECT byte_offset FROM ingest_offsets WHERE source = ? AND path = ?";
    conn.query_row(sql, [SOURCE, path], |r| r.get::<_, i64>(0))
        .optional()
        .ok()
        .flatten()
        .unwrap_or(0)
        .max(0) as u64
}

fn ingest_file(conn: &Connection, path: &Path) -> Result<usize, String> {
    let key = path.display().to_string();
    // a file that vanished (or cannot be read) between listing and reading is skipped
    let Ok((bytes, start)) = read_from(path, stored_offset(conn, &key)) else {
        return Ok(0);
    };
    let (turns, used) = parse_chunk(&bytes);
    let added = turns
        .iter()
        .filter(|t| {
            let detail = serde_json::json!({ "model": t.model, "input": t.input, "output": t.output, "cache_write": t.cache_write, "cache_write_1h": t.cache_write_1h, "cache_read": t.cache_read });
            let session = if t.session.is_empty() { path.file_stem().and_then(|s| s.to_str()).unwrap_or_default() } else { &t.session };
            let row = SourcedUsage { source: SOURCE, reference: &t.id, ts: &t.ts, session, tokens_in: t.input, tokens_out: t.output, cache_read: Some(t.cache_read), cache_write: Some(t.cache_write), cost_usd: t.cost_usd(), model: &t.model, detail: &detail.to_string() };
            db::record_usage_from(conn, &row)
        })
        .count();
    conn.execute(
        "INSERT INTO ingest_offsets (source, path, byte_offset, updated_at) VALUES (?,?,?,?)
         ON CONFLICT(source, path) DO UPDATE SET byte_offset = excluded.byte_offset, updated_at = excluded.updated_at",
        rusqlite::params![SOURCE, key, (start + used as u64) as i64, db::now()],
    )
    .map_err(|e| e.to_string())?;
    Ok(added)
}

/// Read the new transcript lines under `claude_dir` into the hub; returns how many usage rows were added.
pub fn ingest(conn: &Connection, claude_dir: &Path) -> Result<usize, String> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| e.to_string())?;
    let run = || {
        transcripts(claude_dir)
            .iter()
            .try_fold(0, |sum, p| ingest_file(conn, p).map(|n| sum + n))
    };
    match run() {
        Ok(n) => conn
            .execute_batch("COMMIT")
            .map(|_| n)
            .map_err(|e| e.to_string()),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: &str, model: &str, out: i64) -> String {
        format!(
            r#"{{"type":"assistant","uuid":"u","sessionId":"s","timestamp":"t","message":{{"id":"{id}","model":"{model}","usage":{{"input_tokens":1,"cache_creation_input_tokens":2,"cache_read_input_tokens":3,"output_tokens":{out}}}}}}}"#
        )
    }

    #[test]
    fn a_line_becomes_a_turn_only_when_it_is_an_assistant_message_with_usage_and_a_real_model() {
        let t = parse_line(&line("m1", "claude-sonnet-5-5", 4)).unwrap();
        assert_eq!(
            (
                t.id.as_str(),
                t.input,
                t.cache_write,
                t.cache_read,
                t.output,
                t.session.as_str()
            ),
            ("m1", 1, 2, 3, 4, "s")
        );
        for skip in [
            line("m1", "<synthetic>", 1),
            r#"{"type":"user"}"#.into(),
            r#"{"type":"assistant","message":{"id":"m","model":"x"}}"#.into(),
            "{nope".into(),
            String::new(),
        ] {
            assert_eq!(parse_line(&skip), None, "{skip}");
        }
    }

    #[test]
    fn a_chunk_keeps_one_turn_per_id_with_the_largest_output_and_leaves_a_partial_line_unread() {
        let text = format!(
            "{}\n{}\n{}\nbad json\n{}",
            line("a", "m", 5),
            line("a", "m", 9),
            line("b", "m", 1),
            &line("c", "m", 1)[..20]
        );
        let (turns, used) = parse_chunk(text.as_bytes());
        let got: Vec<(&str, i64)> = turns.iter().map(|t| (t.id.as_str(), t.output)).collect();
        assert_eq!(got, [("a", 9), ("b", 1)]);
        assert_eq!(
            &text[used..],
            &line("c", "m", 1)[..20],
            "the unterminated tail is not consumed"
        );
        assert_eq!(parse_chunk(b"no newline yet"), (Vec::new(), 0));
    }

    #[test]
    fn model_names_map_onto_the_price_table_spelling() {
        assert_eq!(price_name("claude-sonnet-5-5"), "claude-sonnet-5.5");
        assert_eq!(price_name("claude-haiku-4-5-20251001"), "claude-haiku-4.5");
        assert_eq!(price_name("claude-opus-5"), "claude-opus-5");
        let priced = Turn {
            id: "m".into(),
            ts: String::new(),
            session: String::new(),
            model: "claude-sonnet-5-5".into(),
            input: 1_000_000,
            output: 1_000_000,
            cache_write: 1_000_000,
            cache_write_1h: 0,
            cache_read: 1_000_000,
        };
        // $2/$10 per 1M: input 2 + output 10 + read 0.1x 0.2 + 5-minute write 1.25x 2.5
        assert!((priced.cost_usd().unwrap() - (2.0 + 10.0 + 0.2 + 2.5)).abs() < 1e-9);
        let hour = Turn {
            cache_write_1h: 1_000_000,
            ..priced.clone()
        };
        assert!(
            (hour.cost_usd().unwrap() - (2.0 + 10.0 + 0.2 + 4.0)).abs() < 1e-9,
            "1-hour writes cost 2x"
        );
        assert_eq!(
            Turn {
                model: "claude-unlisted-1".into(),
                ..priced
            }
            .cost_usd(),
            None
        );
    }
}

#[cfg(test)]
mod session_fixture {
    use super::*;

    /// The shape of the screenshot session of `/usage` (claude-sonnet-5-5): one turn per bucket total, with the
    /// 5-minute / 1-hour write split the transcripts carry. Claude Code showed $18.52 for its own figures.
    #[test]
    fn the_screenshot_session_is_estimated_within_the_published_multipliers() {
        let line = r#"{"type":"assistant","sessionId":"s","timestamp":"t","message":{"id":"m","model":"claude-sonnet-5-5","usage":{"input_tokens":158200,"output_tokens":424500,"cache_read_input_tokens":80400000,"cache_creation_input_tokens":2000000,"cache_creation":{"ephemeral_5m_input_tokens":1400000,"ephemeral_1h_input_tokens":600000}}}}"#;
        let t = parse_line(line).unwrap();
        assert_eq!((t.cache_write, t.cache_write_1h), (2_000_000, 600_000));
        // 158.2k*2 + 424.5k*10 + 80.4M*0.2 + 1.4M*2.5 + 0.6M*4 per 1M = 0.3164 + 4.245 + 16.08 + 3.5 + 2.4
        let expected = 0.3164 + 4.245 + 16.08 + 3.5 + 2.4;
        assert!(
            (t.cost_usd().unwrap() - expected).abs() < 1e-6,
            "{:?}",
            t.cost_usd()
        );
        // same order of magnitude as Claude Code's $18.52: the estimate is ~1.43x, not tuned to match, because the
        // price table is GitHub Copilot's ($2/$10), which is not necessarily what Claude Code bills sonnet-5.5 at
        assert!((t.cost_usd().unwrap() / 18.52 - 1.0).abs() < 0.5);
        let haiku = Turn {
            model: "claude-haiku-5-5".into(),
            input: 149_100,
            output: 8_100,
            cache_write: 0,
            cache_write_1h: 0,
            cache_read: 0,
            ..t
        };
        assert!(
            (haiku.cost_usd().unwrap() - 0.0190).abs() < 5e-5,
            "{:?}",
            haiku.cost_usd()
        );
    }
}
