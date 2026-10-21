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
            i.map_or("n/a".into(), |v| v.to_string()),
            o.map_or("n/a".into(), |v| v.to_string()),
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

fn tok(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn usage_cell(i: Option<i64>, o: Option<i64>, c: Option<f64>) -> String {
    let tokens = match (i, o) {
        (None, None) => "n/a".to_string(),
        (i, o) => format!("{}/{}", tok(i.unwrap_or(0)), tok(o.unwrap_or(0))),
    };
    // cost is an estimate whenever opencode reports 0 (subscription providers): shown with `~`
    format!("{tokens} {}", c.map_or("n/a".into(), |c| format!("~${c:.4}")))
}

pub fn format_stats(stats: &[AgentStats]) -> String {
    let mut out = format!(
        "{:<20} {:>5} {:>5} {:>5} {:>9} {:>8} {:>10}  {}\n",
        "agent", "recv", "done", "fail", "delivered", "released", "avg", "tokens in/out, cost"
    );
    for s in stats {
        out.push_str(&format!(
            "{:<20} {:>5} {:>5} {:>5} {:>9} {:>8} {:>10}  {}\n",
            s.agent,
            s.received,
            s.completed,
            s.failed,
            s.delivered,
            s.released,
            s.avg_ms.map_or("n/a".into(), secs),
            usage_cell(s.tokens_in, s.tokens_out, s.cost_usd)
        ));
    }
    if stats.iter().any(|s| s.tokens_in.is_some() || s.tokens_out.is_some()) {
        let sum = |f: fn(&AgentStats) -> Option<i64>| Some(stats.iter().filter_map(f).sum::<i64>());
        let cost: f64 = stats.iter().filter_map(|s| s.cost_usd).sum();
        out.push_str(&format!(
            "{:<20} {:>5} {:>5} {:>5} {:>9} {:>8} {:>10}  {}\n",
            "total", "", "", "", "", "", "",
            usage_cell(sum(|s| s.tokens_in), sum(|s| s.tokens_out), Some(cost))
        ));
        out.push_str("(cost is opencode's figure when it charges, otherwise an estimate from the published price table)\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn stats_table_has_na_for_unknown() {
        let s = AgentStats {
            agent: "mentor".into(),
            received: 2,
            completed: 1,
            ..Default::default()
        };
        let out = format_stats(&[s]);
        assert!(out.contains("mentor") && out.contains("n/a"), "{out}");
    }

    #[test]
    fn stats_show_tokens_cost_and_a_total_row() {
        let a = AgentStats { agent: "mentor".into(), tokens_in: Some(1_500_000), tokens_out: Some(2_000), cost_usd: Some(0.6), ..Default::default() };
        let b = AgentStats { agent: "build".into(), tokens_in: Some(500), tokens_out: Some(100), cost_usd: None, ..Default::default() };
        let out = format_stats(&[a, b]);
        assert!(out.contains("1.5M/2.0k ~$0.6000"), "{out}");
        assert!(out.contains("500/100 n/a"), "{out}");
        assert!(out.contains("total") && out.contains("1.5M/2.1k"), "{out}");
    }
}
