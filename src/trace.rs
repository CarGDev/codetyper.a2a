//! `oc trace` / `oc stats`: read-only views over the `events` table and the tasks.
//! Tokens and cost are NULL unless something recorded them; they print as `n/a`.
use crate::hub::db::TaskRow;
use chrono::DateTime;
use rusqlite::Connection;

#[derive(Debug, Clone)]
pub struct Event {
    pub ts: String,
    pub kind: String,
    pub agent: Option<String>,
    pub detail: Option<String>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub cost_usd: Option<f64>,
}

fn ms(ts: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|d| d.timestamp_millis())
}

fn secs(ms: i64) -> String {
    format!("{:.3}s", ms as f64 / 1000.0)
}

fn cost(e: &Event) -> String {
    match (e.tokens_in, e.tokens_out, e.cost_usd) {
        (None, None, None) => "n/a".into(),
        (i, o, c) => format!(
            "{}/{} tok, {}",
            i.map_or("n/a".into(), tok),
            o.map_or("n/a".into(), tok),
            c.map_or("n/a".into(), |v| format!("${v:.4}"))
        ),
    }
}

/// Resolve an id or unique prefix to a task row.
pub fn find_task(conn: &Connection, prefix: &str) -> Result<TaskRow, String> {
    let mut rows = crate::hub::db::tasks(conn, "WHERE id LIKE ?||'%' LIMIT 3", &[prefix])?;
    match rows.len() {
        0 => Err(format!("no task matches \"{prefix}\"")),
        1 => Ok(rows.remove(0)),
        _ => Err(format!(
            "\"{prefix}\" is ambiguous ({} tasks match); use more characters",
            rows.len()
        )),
    }
}

pub fn events_for(conn: &Connection, task_id: &str) -> Result<Vec<Event>, String> {
    let mut st = conn
        .prepare("SELECT ts, kind, agent, detail, tokens_in, tokens_out, cost_usd FROM events WHERE task_id = ? ORDER BY id")
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map([task_id], |r| {
            Ok(Event {
                ts: r.get(0)?,
                kind: r.get(1)?,
                agent: r.get(2)?,
                detail: r.get(3)?,
                tokens_in: r.get(4)?,
                tokens_out: r.get(5)?,
                cost_usd: r.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.flatten().collect())
}

pub fn format_trace(t: &TaskRow, events: &[Event]) -> String {
    let mut out = format!(
        "task {}\n  {} -> {}   state: {}\n  created: {}   updated: {}\n",
        t.id, t.from_agent, t.to_agent, t.state, t.created_at, t.updated_at
    );
    if events.is_empty() {
        out.push_str("  no events recorded for this task\n");
        return out;
    }
    let first = ms(&events[0].ts).unwrap_or(0);
    out.push_str("\ntimeline:\n");
    for e in events {
        let off = ms(&e.ts).map_or(0, |v| v - first);
        out.push_str(&format!(
            "  {}  +{:<9} {:<16} {:<14} {}   [{}]\n",
            e.ts,
            secs(off),
            e.kind,
            e.agent.as_deref().unwrap_or("-"),
            e.detail.as_deref().unwrap_or(""),
            cost(e)
        ));
    }
    let last = events.last().and_then(|e| ms(&e.ts)).unwrap_or(first);
    out.push_str(&format!("\ntotal duration: {}\n", secs(last - first)));
    out
}

#[derive(Debug, Default, PartialEq)]
pub struct AgentStats {
    pub agent: String,
    pub received: i64,
    pub completed: i64,
    pub failed: i64,
    pub delivered: i64,
    pub released: i64,
    pub avg_ms: Option<i64>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub cost_usd: Option<f64>,
}

pub fn agent_stats(conn: &Connection) -> Result<Vec<AgentStats>, String> {
    let mut st = conn
        .prepare(
            "SELECT a.name,
               (SELECT COUNT(*) FROM tasks t WHERE t.to_agent = a.name),
               (SELECT COUNT(*) FROM tasks t WHERE t.to_agent = a.name AND t.state = 'completed'),
               (SELECT COUNT(*) FROM tasks t WHERE t.to_agent = a.name AND t.state = 'failed'),
               (SELECT COUNT(*) FROM events e WHERE e.agent = a.name AND e.kind = 'nudge_delivered'),
               (SELECT COUNT(*) FROM events e WHERE e.agent = a.name AND e.kind = 'nudge_released'),
               (SELECT SUM(e.cost_usd) FROM events e WHERE e.agent = a.name AND e.kind = 'usage'),
               (SELECT SUM(e.tokens_in) FROM events e WHERE e.agent = a.name AND e.kind = 'usage'),
               (SELECT SUM(e.tokens_out) FROM events e WHERE e.agent = a.name AND e.kind = 'usage')
             FROM (SELECT to_agent AS name FROM tasks UNION SELECT agent FROM events WHERE agent IS NOT NULL) a
             ORDER BY a.name",
        )
        .map_err(|e| e.to_string())?;
    let mut stats: Vec<AgentStats> = st
        .query_map([], |r| {
            Ok(AgentStats {
                agent: r.get(0)?,
                received: r.get(1)?,
                completed: r.get(2)?,
                failed: r.get(3)?,
                delivered: r.get(4)?,
                released: r.get(5)?,
                avg_ms: None,
                cost_usd: r.get(6)?,
                tokens_in: r.get(7)?,
                tokens_out: r.get(8)?,
            })
        })
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    for s in &mut stats {
        let mut st = conn
            .prepare("SELECT created_at, updated_at FROM tasks WHERE to_agent = ? AND state IN ('completed','failed','canceled','rejected')")
            .map_err(|e| e.to_string())?;
        let d: Vec<i64> = st
            .query_map([&s.agent], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?
            .flatten()
            .filter_map(|(a, b)| Some(ms(&b)? - ms(&a)?))
            .collect();
        s.avg_ms = (!d.is_empty()).then(|| d.iter().sum::<i64>() / d.len() as i64);
    }
    Ok(stats)
}

/// `999`, `1.5k`, `2.3M`, `4.0B`.
pub(crate) fn tok(n: i64) -> String {
    let (div, suffix) = match n.abs() {
        1_000_000_000.. => (1e9, "B"),
        1_000_000.. => (1e6, "M"),
        1_000.. => (1e3, "k"),
        _ => return n.to_string(),
    };
    format!("{:.1}{suffix}", n as f64 / div)
}

/// One row of `oc stats --model`: everything the usage events recorded for one model.
#[derive(Debug, Clone, Default)]
pub struct ModelStats {
    /// `opencode` (also the NULL `events.source` of the dispatcher's rows) or `claude-code`.
    pub source: String,
    pub model: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cost_usd: Option<f64>,
    pub turns: i64,
    pub tasks: i64,
    pub agents: i64,
}

/// Usage events grouped by source and model; rows written before the `model` column existed (or by an
/// older dispatcher) are attributed from their `detail` JSON, and rows with none are `unknown`.
pub fn model_stats(conn: &Connection) -> Result<Vec<ModelStats>, String> {
    let mut st = conn
        .prepare(
            "SELECT COALESCE(NULLIF(source, ''), 'opencode'),
               COALESCE(NULLIF(model, ''), CASE WHEN json_valid(detail) THEN NULLIF(json_extract(detail, '$.model'), '') END, 'unknown'),
               COALESCE(SUM(tokens_in), 0), COALESCE(SUM(tokens_out), 0), SUM(cost_usd),
               COUNT(*), COUNT(DISTINCT task_id), COUNT(DISTINCT agent)
             FROM events WHERE kind = 'usage' GROUP BY 1, 2 ORDER BY 1, 2",
        )
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map([], |r| {
            Ok(ModelStats {
                source: r.get(0)?,
                model: r.get(1)?,
                tokens_in: r.get(2)?,
                tokens_out: r.get(3)?,
                cost_usd: r.get(4)?,
                turns: r.get(5)?,
                tasks: r.get(6)?,
                agents: r.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    Ok(rows)
}

pub fn format_stats(stats: &[AgentStats]) -> String {
    crate::stats_view::render(stats, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_get_k_m_b_suffixes() {
        assert_eq!(tok(999), "999");
        assert_eq!(tok(1_500), "1.5k");
        assert_eq!(tok(770_800), "770.8k");
        assert_eq!(tok(2_300_000), "2.3M");
        assert_eq!(tok(4_000_000_000), "4.0B");
    }

    fn task() -> TaskRow {
        TaskRow {
            id: "abcdef12-0000".into(),
            context_id: "c".into(),
            from_agent: "orchestrator".into(),
            to_agent: "mentor".into(),
            state: "completed".into(),
            status_message: None,
            history: "[]".into(),
            artifacts: "[]".into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:03.000Z".into(),
            requester_seen_at: None,
            directory: None,
            from_directory: None,
            from_session: None,
            version: 0,
            claimed_by: None,
            priority: 0,
            explicit_dir: false,
        }
    }

    fn ev(ts: &str, kind: &str, agent: &str, detail: &str) -> Event {
        Event {
            ts: ts.into(),
            kind: kind.into(),
            agent: Some(agent.into()),
            detail: Some(detail.into()),
            tokens_in: None,
            tokens_out: None,
            cost_usd: None,
        }
    }

    #[test]
    fn trace_shows_header_offsets_total_and_na() {
        let evs = vec![
            ev(
                "2026-01-01T00:00:00.000Z",
                "task_created",
                "orchestrator",
                "mentor",
            ),
            ev(
                "2026-01-01T00:00:01.234Z",
                "nudge_delivered",
                "mentor",
                "assignee",
            ),
            ev(
                "2026-01-01T00:00:03.000Z",
                "state_change",
                "mentor",
                "completed",
            ),
        ];
        let out = format_trace(&task(), &evs);
        assert!(out.contains("orchestrator -> mentor"), "{out}");
        assert!(out.contains("+1.234s"), "{out}");
        assert!(out.contains("total duration: 3.000s"), "{out}");
        assert!(out.contains("n/a"), "{out}");
    }

    #[test]
    fn trace_without_events_says_so() {
        assert!(format_trace(&task(), &[]).contains("no events recorded"));
    }

    #[test]
    fn model_stats_groups_usage_events_by_their_model() {
        use crate::hub::db;
        let dir = std::env::temp_dir().join(format!("a2a-trace-models-{}", std::process::id()));
        let conn = db::open(&dir.join("t.db")).unwrap();
        let sonnet = r#"{"model":"github-copilot/claude-sonnet-5.5"}"#;
        let model = Some("github-copilot/claude-sonnet-5.5");
        db::record_usage(&conn, "t1", "c", "build", 100, 10, Some(0.5), model, sonnet);
        db::record_usage(&conn, "t1", "c", "build", 50, 5, None, model, sonnet);
        db::record_usage(&conn, "t2", "c", "plan", 25, 2, Some(0.25), model, sonnet);
        db::record_usage(&conn, "t3", "c", "mentor", 7, 1, None, None, r#"{"model":null}"#);
        db::record_usage(&conn, "t4", "c", "mentor", 3, 1, None, None, "{}");
        db::record_event(&conn, Some("t1"), Some("c"), "task_created", Some("build"), Some("x"));
        let mut m = model_stats(&conn).unwrap();
        m.sort_by(|a, b| a.model.cmp(&b.model));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(m.len(), 2, "{m:?}");
        let (known, unknown) = (&m[0], &m[1]);
        assert_eq!(known.model, "github-copilot/claude-sonnet-5.5");
        assert_eq!((known.tokens_in, known.tokens_out, known.turns, known.tasks, known.agents), (175, 17, 3, 2, 2));
        assert_eq!(known.cost_usd, Some(0.75));
        assert_eq!(unknown.model, "unknown");
        assert_eq!((unknown.tokens_in, unknown.turns, unknown.tasks, unknown.agents), (10, 2, 2, 1));
        assert_eq!(unknown.cost_usd, None);
    }

    #[test]
    fn model_stats_splits_by_source_and_reads_a_null_source_as_opencode() {
        use crate::hub::db;
        let dir = std::env::temp_dir().join(format!("a2a-trace-source-{}", std::process::id()));
        let conn = db::open(&dir.join("t.db")).unwrap();
        let m = Some("claude-sonnet-5-5");
        db::record_usage(&conn, "t1", "c", "build", 100, 10, None, m, "{}"); // dispatcher row: NULL source
        let claude = |reference, session| db::SourcedUsage { source: "claude-code", reference, ts: "t", session, tokens_in: 7, tokens_out: 1, cost_usd: Some(0.25), model: "claude-sonnet-5-5", detail: "{}" };
        db::record_usage_from(&conn, &claude("m1", "s1"));
        db::record_usage_from(&conn, &claude("m2", "s2"));
        let got: Vec<(String, String, i64, i64, i64)> =
            model_stats(&conn).unwrap().into_iter().map(|s| (s.source, s.model, s.tokens_in, s.turns, s.tasks)).collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got, [("claude-code".into(), "claude-sonnet-5-5".into(), 14, 2, 2), ("opencode".into(), "claude-sonnet-5-5".into(), 100, 1, 1)]);
    }

    #[test]
    fn model_stats_prefers_the_column_and_falls_back_to_detail_for_older_dispatcher_rows() {
        use crate::hub::db;
        let dir = std::env::temp_dir().join(format!("a2a-trace-fallback-{}", std::process::id()));
        let conn = db::open(&dir.join("t.db")).unwrap();
        db::record_usage(&conn, "t1", "c", "build", 100, 10, None, Some("a/column"), r#"{"model":"a/stale"}"#);
        // a binary of schema 2 knows no `model` column
        conn.execute(
            "INSERT INTO events (ts, task_id, context_id, kind, agent, detail, tokens_in, tokens_out) VALUES (?,?,?,'usage',?,?,?,?)",
            rusqlite::params![db::now(), "t2", "c", "plan", r#"{"model":"a/detail"}"#, 5, 1],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (ts, task_id, context_id, kind, agent, detail, tokens_in, tokens_out) VALUES (?,?,?,'usage',?,?,?,?)",
            rusqlite::params![db::now(), "t3", "c", "plan", "not json", 2, 1],
        )
        .unwrap();
        let mut m = model_stats(&conn).unwrap();
        m.sort_by(|a, b| a.model.cmp(&b.model));
        let _ = std::fs::remove_dir_all(&dir);
        let got: Vec<(&str, i64)> = m.iter().map(|s| (s.model.as_str(), s.tokens_in)).collect();
        assert_eq!(got, vec![("a/column", 100), ("a/detail", 5), ("unknown", 2)]);
    }
}
