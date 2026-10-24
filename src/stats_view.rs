//! `oc stats`: one row per agent, biggest token user first, with a bar for its share of the tokens.
//! A value that is not known stays blank instead of printing `n/a`.
use crate::trace::{AgentStats, ModelStats, tok};

const BAR: usize = 20;
const PALETTE: [u8; 6] = [111, 141, 80, 114, 179, 210];

/// ANSI colouring that can be switched off; text is padded before it is painted so columns line up.
struct Paint(bool);

impl Paint {
    fn cell(&self, code: &str, text: &str, width: usize, right: bool) -> String {
        let padded = if right { format!("{text:>width$}") } else { format!("{text:<width$}") };
        if !self.0 || text.is_empty() {
            return padded;
        }
        format!("\x1b[{code}m{padded}\x1b[0m")
    }
}

fn total_tokens(s: &AgentStats) -> i64 {
    s.tokens_in.unwrap_or(0) + s.tokens_out.unwrap_or(0)
}

fn has_usage(s: &AgentStats) -> bool {
    s.tokens_in.is_some() || s.tokens_out.is_some()
}

fn blank_zero(n: i64) -> String {
    if n == 0 { String::new() } else { n.to_string() }
}

fn tokens_cell(i: Option<i64>, o: Option<i64>) -> String {
    if i.is_none() && o.is_none() {
        return String::new();
    }
    format!("{}/{}", tok(i.unwrap_or(0)), tok(o.unwrap_or(0)))
}

fn cost_cell(c: Option<f64>) -> String {
    c.map_or(String::new(), |c| format!("~${c:.4}"))
}

/// `850ms`, `12.3s`, `4m 05s`, `2h 03m`, `1d 04h`.
fn duration(ms: i64) -> String {
    let secs = ms / 1000;
    match secs {
        _ if ms < 1000 => format!("{ms}ms"),
        0..=59 => format!("{:.1}s", ms as f64 / 1000.0),
        60..=3599 => format!("{}m {:02}s", secs / 60, secs % 60),
        3600..=86399 => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d {:02}h", secs / 86400, secs % 86400 / 3600),
    }
}

fn avg_cell(ms: Option<i64>) -> String {
    ms.map_or(String::new(), duration)
}

/// `(filled, rest)` of a bar that is `share` (0..=1) full; any usage shows at least one block.
fn bar_parts(share: f64) -> (String, String) {
    let full = ((share * BAR as f64).round() as usize).clamp(1, BAR);
    ("█".repeat(full), "░".repeat(BAR - full))
}

/// Green when light, yellow when medium, red when this agent is most of the usage.
fn heat(share: f64) -> &'static str {
    match share {
        s if s >= 0.66 => "31",
        s if s >= 0.33 => "33",
        _ => "32",
    }
}

fn agent_code(name: &str) -> String {
    let i = name.bytes().map(usize::from).sum::<usize>() % PALETTE.len();
    format!("38;5;{}", PALETTE[i])
}

fn bar_cell(p: &Paint, tokens: i64, top: i64) -> String {
    if tokens <= 0 || top <= 0 {
        return " ".repeat(BAR);
    }
    let share = tokens as f64 / top as f64;
    let (full, rest) = bar_parts(share);
    let paint = |code: &str, s: String| if p.0 { format!("\x1b[{code}m{s}\x1b[0m") } else { s };
    format!("{}{}", paint(heat(share), full), paint("90", rest))
}

pub fn render(stats: &[AgentStats], color: bool) -> String {
    let p = Paint(color);
    // a row with nothing to show (the `user` and `dispatcher` senders) is left out
    let mut rows: Vec<&AgentStats> = stats.iter().filter(|s| has_usage(s) || s.received > 0).collect();
    rows.sort_by(|a, b| total_tokens(b).cmp(&total_tokens(a)).then_with(|| a.agent.cmp(&b.agent)));
    let top = rows.first().map_or(0, |s| total_tokens(s));
    let head = |s: &str, w: usize, right: bool| p.cell("1;36", s, w, right);
    let mut out = format!(
        "{} {} {} {} {} {} {} {}\n",
        head("agent", 20, false),
        head("tokens in/out", 15, true),
        head("share", BAR, false),
        head("cost", 10, true),
        head("recv", 5, true),
        head("done", 5, true),
        head("fail", 5, true),
        head("avg", 8, true),
    );
    for s in rows {
        let fail = blank_zero(s.failed);
        out.push_str(&format!(
            "{} {} {} {} {} {} {} {}\n",
            p.cell(&format!("1;{}", agent_code(&s.agent)), &s.agent, 20, false),
            p.cell("37", &tokens_cell(s.tokens_in, s.tokens_out), 15, true),
            bar_cell(&p, total_tokens(s), top),
            p.cell("33", &cost_cell(s.cost_usd), 10, true),
            p.cell("90", &blank_zero(s.received), 5, true),
            p.cell("32", &blank_zero(s.completed), 5, true),
            p.cell("1;31", &fail, 5, true),
            p.cell("90", &avg_cell(s.avg_ms), 8, true),
        ));
    }
    if !stats.iter().any(has_usage) {
        return out;
    }
    let sum = |f: fn(&AgentStats) -> Option<i64>| Some(stats.iter().filter_map(f).sum::<i64>());
    // fold from +0.0: an empty f64 sum is -0.0 and printed as `~$-0.0000`
    let cost = stats.iter().filter_map(|s| s.cost_usd).fold(0.0, |a, c| a + c);
    out.push_str(&format!(
        "{} {} {} {}\n",
        p.cell("1", "total", 20, false),
        p.cell("1", &tokens_cell(sum(|s| s.tokens_in), sum(|s| s.tokens_out)), 15, true),
        " ".repeat(BAR),
        p.cell("1;33", &cost_cell(Some(cost)), 10, true),
    ));
    out.push_str(&p.cell("90", "cost is opencode's figure when it charges, otherwise an estimate from the published price table", 0, false));
    out.push('\n');
    out
}

const OPENCODE_NOTE: &str = "cost is opencode's figure when it charges, otherwise an estimate from the published price table";
const CLAUDE_NOTE: &str = "tokens in include cache (input + cache creation + cache read); tasks are sessions; cost is an estimate from the published price table, blank for models it does not list";

/// The tools `oc stats --model` knows, in print order: `(source, title, footnote)`.
const SOURCES: [(&str, &str, &str); 2] = [("opencode", "opencode", OPENCODE_NOTE), ("claude-code", "claude code", CLAUDE_NOTE)];

/// `oc stats --model`: the same table per model, one titled table per source (opencode, then claude code), each with
/// its own total; sources without usage are left out. The name column grows to the longest model id.
pub fn render_models(stats: &[ModelStats], color: bool) -> String {
    let p = Paint(color);
    let tables: Vec<String> = SOURCES
        .iter()
        .filter_map(|(source, title, note)| {
            let rows: Vec<&ModelStats> = stats.iter().filter(|s| s.source == *source).collect();
            (!rows.is_empty()).then(|| format!("{}\n{}", p.cell("1", title, 0, false), model_table(&rows, &p, note)))
        })
        .collect();
    if tables.is_empty() {
        return model_table(&[], &p, OPENCODE_NOTE);
    }
    tables.join("\n")
}

fn model_table(rows: &[&ModelStats], p: &Paint, note: &str) -> String {
    let total = |s: &ModelStats| s.tokens_in + s.tokens_out;
    let mut rows: Vec<&ModelStats> = rows.to_vec();
    rows.sort_by(|a, b| total(b).cmp(&total(a)).then_with(|| a.model.cmp(&b.model)));
    let top = rows.first().map_or(0, |s| total(s));
    let width = rows.iter().map(|s| s.model.chars().count()).max().unwrap_or(0).max(20);
    let head = |s: &str, w: usize, right: bool| p.cell("1;36", s, w, right);
    let mut out = format!(
        "{} {} {} {} {} {} {}\n",
        head("model", width, false),
        head("tokens in/out", 15, true),
        head("share", BAR, false),
        head("cost", 10, true),
        head("turns", 5, true),
        head("tasks", 5, true),
        head("agents", 6, true),
    );
    if rows.is_empty() {
        out.push_str(&p.cell("90", "no usage recorded yet", 0, false));
        out.push('\n');
        return out;
    }
    for s in &rows {
        out.push_str(&format!(
            "{} {} {} {} {} {} {}\n",
            p.cell(&format!("1;{}", agent_code(&s.model)), &s.model, width, false),
            p.cell("37", &tokens_cell(Some(s.tokens_in), Some(s.tokens_out)), 15, true),
            bar_cell(p, total(s), top),
            p.cell("33", &cost_cell(s.cost_usd), 10, true),
            p.cell("90", &blank_zero(s.turns), 5, true),
            p.cell("90", &blank_zero(s.tasks), 5, true),
            p.cell("90", &blank_zero(s.agents), 6, true),
        ));
    }
    // fold from +0.0: an empty f64 sum is -0.0 and printed as `~$-0.0000`
    let cost = rows.iter().filter_map(|s| s.cost_usd).fold(0.0, |a, c| a + c);
    out.push_str(&format!(
        "{} {} {} {} {}\n",
        p.cell("1", "total", width, false),
        p.cell("1", &tokens_cell(Some(rows.iter().map(|s| s.tokens_in).sum()), Some(rows.iter().map(|s| s.tokens_out).sum())), 15, true),
        " ".repeat(BAR),
        p.cell("1;33", &cost_cell(Some(cost)), 10, true),
        p.cell("1", &blank_zero(rows.iter().map(|s| s.turns).sum()), 5, true),
    ));
    out.push_str(&p.cell("90", note, 0, false));
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str, i: Option<i64>, o: Option<i64>, cost: Option<f64>) -> AgentStats {
        AgentStats { agent: name.into(), tokens_in: i, tokens_out: o, cost_usd: cost, ..Default::default() }
    }

    #[test]
    fn agents_are_listed_from_most_tokens_to_least() {
        let out = render(&[agent("small", Some(10), Some(1), None), agent("big", Some(1_000_000), Some(5), Some(1.0)), AgentStats { agent: "none".into(), received: 1, ..Default::default() }], false);
        let (big, small, none) = (out.find("big").unwrap(), out.find("small").unwrap(), out.find("none").unwrap());
        assert!(big < small && small < none, "{out}");
    }

    #[test]
    fn durations_use_the_largest_unit() {
        assert_eq!(duration(850), "850ms");
        assert_eq!(duration(12_300), "12.3s");
        assert_eq!(duration(226_300), "3m 46s");
        assert_eq!(duration(10_180_400), "2h 49m");
        assert_eq!(duration(14_371_200 * 10), "1d 15h");
    }

    #[test]
    fn unknown_values_are_blank_not_na() {
        let out = render(&[AgentStats { agent: "mentor".into(), received: 2, completed: 1, ..Default::default() }], false);
        assert!(out.contains("mentor") && !out.contains("n/a"), "{out}");
    }

    #[test]
    fn the_biggest_agent_has_a_full_bar_and_the_total_sums_up() {
        let out = render(&[agent("a", Some(1_500_000), Some(2_000), Some(0.6)), agent("b", Some(500), Some(100), None)], false);
        assert!(out.contains(&"█".repeat(BAR)), "{out}");
        assert!(out.contains("1.5M/2.0k") && out.contains("500/100") && out.contains("~$0.6000"), "{out}");
        assert!(out.lines().any(|l| l.starts_with("total") && l.contains("1.5M/2.1k")), "{out}");
    }

    #[test]
    fn colour_is_only_added_when_asked_for() {
        let s = [agent("a", Some(10), Some(1), Some(0.1))];
        assert!(!render(&s, false).contains('\x1b'));
        assert!(render(&s, true).contains("\x1b["));
    }

    #[test]
    fn an_unpriced_total_is_zero_not_negative_zero() {
        let out = render(&[agent("a", Some(10), Some(1), None)], false);
        assert!(out.contains("~$0.0000") && !out.contains("-0.0"), "{out}");
    }

    fn model(name: &str, i: i64, o: i64, cost: Option<f64>) -> ModelStats {
        ModelStats { source: "opencode".into(), model: name.into(), tokens_in: i, tokens_out: o, cost_usd: cost, turns: 2, tasks: 1, agents: 1 }
    }

    fn claude(name: &str, i: i64, o: i64) -> ModelStats {
        ModelStats { source: "claude-code".into(), ..model(name, i, o, None) }
    }

    /// The table without its title line: what `oc stats --model` printed before the split by source.
    fn without_title(out: &str) -> &str {
        out.split_once('\n').unwrap().1
    }

    #[test]
    fn each_source_gets_its_own_titled_table_with_its_own_total_and_footnote() {
        let out = render_models(&[claude("claude-sonnet-5-5", 9_000, 100), model("big", 1_500_000, 2_000, Some(0.6)), model("small", 10, 1, None)], false);
        let (oc, cc) = (out.find("opencode\n").unwrap(), out.find("claude code\n").unwrap());
        assert!(oc < cc && out.starts_with("opencode\n"), "{out}");
        let totals: Vec<&str> = out.lines().filter(|l| l.starts_with("total")).collect();
        assert!(totals.len() == 2 && totals[0].contains("1.5M/2.0k") && totals[1].contains("9.0k/100"), "{out}");
        assert!(out[..cc].contains("opencode's figure") && !out[..cc].contains("include cache"), "{out}");
        assert!(out[cc..].contains("include cache") && !out[cc..].contains("opencode's figure"), "{out}");
        assert!(!out[..cc].contains("claude-sonnet-5-5") && !out[cc..].contains("big"), "{out}");
    }

    #[test]
    fn the_opencode_table_is_what_the_single_table_used_to_be() {
        let rows = [model("small", 10, 1, None), model("big", 1_500_000, 2_000, Some(0.6))];
        let with_claude = render_models(&[rows[0].clone(), rows[1].clone(), claude("c", 5, 5)], false);
        let alone = render_models(&rows, false);
        let table = without_title(&alone);
        assert!(table.starts_with("model") && table.ends_with("published price table\n"), "{alone}");
        assert!(without_title(&with_claude).starts_with(table), "{with_claude}");
    }

    #[test]
    fn an_empty_source_is_skipped_and_only_claude_usage_prints_only_its_table() {
        let out = render_models(&[claude("c", 5, 5)], false);
        assert!(out.starts_with("claude code\n") && !out.contains("opencode") && !out.contains("no usage"), "{out}");
    }

    #[test]
    fn models_are_listed_from_most_tokens_to_least_with_a_total() {
        let out = render_models(&[model("small", 10, 1, None), model("big", 1_500_000, 2_000, Some(0.6)), model("unknown", 5, 0, None)], false);
        let (big, small, unknown) = (out.find("big").unwrap(), out.find("small").unwrap(), out.find("unknown").unwrap());
        assert!(big < small && small < unknown, "{out}");
        assert!(out.contains(&"█".repeat(BAR)) && out.contains("~$0.6000"), "{out}");
        assert!(out.lines().any(|l| l.starts_with("total") && l.contains("1.5M/2.0k")), "{out}");
    }

    #[test]
    fn a_long_model_name_is_not_cut_and_columns_still_line_up() {
        let long = "github-copilot/claude-sonnet-5.5-with-a-very-long-suffix";
        let out = render_models(&[model(long, 100, 10, None), model("a", 1, 1, None)], false);
        assert!(out.contains(long), "{out}");
        let col: Vec<_> = out.lines().filter_map(|l| l.chars().position(|c| c == '█')).collect();
        assert!(col.len() == 2 && col[0] == col[1], "{out}");
    }

    #[test]
    fn models_without_usage_say_so() {
        let out = render_models(&[], false);
        assert!(out.starts_with("model") && out.contains("no usage recorded yet") && !out.contains("total"), "{out}");
    }

    #[test]
    fn model_colour_is_only_added_when_asked_for() {
        let s = [model("a", 10, 1, Some(0.1))];
        assert!(!render_models(&s, false).contains('\x1b'));
        assert!(render_models(&s, true).contains("\x1b["));
    }
}
