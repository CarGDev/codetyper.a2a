//! `codetypera2a models` (also offered by `init`): choose the model of each agent from the models
//! opencode actually has, in a small terminal UI. Choices are saved as `agents.<name>.model`
//! (`"provider/model"`, optional `#variant`) in `opencode.jsonc`, which opencode applies to every
//! agent, including those defined by markdown files; the dispatcher passes it on when it opens a
//! session. Recommendations are by role: cheap and fast for light work, balanced for everyday coding,
//! deep for design, and another model family for reviewers so they do not share the author's blind spots.
use crate::{opencode, paths};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

// ---- recommendations ------------------------------------------------------------------------

/// Preference lists, best first; the first one opencode has wins. Model ids, without the provider.
const CHEAP: &[&str] = &["claude-haiku-5.5", "gpt-6-luna", "gpt-5.6-luna", "claude-haiku-4.5", "gpt-5.4-nano", "mai-code-1.1-flash"];
const BALANCED: &[&str] = &["claude-sonnet-5.5", "claude-sonnet-5", "gpt-5.6-terra", "claude-sonnet-4.6"];
const DEEP: &[&str] = &["claude-opus-5.5", "gpt-6.1-sol", "claude-opus-5", "claude-sonnet-5.5", "claude-sonnet-5"];
const OTHER_FAMILY: &[&str] = &["gpt-6.1-sol", "gpt-6-sol", "gpt-5.6-terra", "gpt-5.3-codex", "claude-sonnet-5.5"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    Cheap,
    Balanced,
    Deep,
    OtherFamily,
}

impl Tier {
    fn list(self) -> &'static [&'static str] {
        match self {
            Tier::Cheap => CHEAP,
            Tier::Balanced => BALANCED,
            Tier::Deep => DEEP,
            Tier::OtherFamily => OTHER_FAMILY,
        }
    }

    pub fn why(self) -> &'static str {
        match self {
            Tier::Cheap => "light work (routing, running commands, summaries, lookups): fast and cheap",
            Tier::Balanced => "everyday coding and agentic work: capable without the top price",
            Tier::Deep => "design and long-horizon reasoning: worth the strongest model",
            Tier::OtherFamily => "reviewing: a different model family than the one that wrote the code",
        }
    }
}

pub fn tier_for(agent: &str) -> Option<Tier> {
    Some(match agent {
        "brain" | "test-runner" | "summarizer" | "doc-lookup" | "git-historian" | "pr-commenter" | "mentor" => Tier::Cheap,
        "orchestrator" | "plan" | "build" | "code-debugger" | "owasp-guardian" | "security-scanner" | "test-case-generator"
        | "react-frontend-expert" | "typescript-mcp-expert" | "api-design-architect" | "software-architecture-expert"
        | "playwright-tester" => Tier::Balanced,
        "architect" => Tier::Deep,
        "code-reviewer" | "pr-reviewer" => Tier::OtherFamily,
        _ => return None,
    })
}

/// `provider/model#variant` -> `model` (what the preference lists use).
fn model_id(full: &str) -> &str {
    let base = full.split('#').next().unwrap_or(full);
    base.split_once('/').map(|(_, m)| m).unwrap_or(base)
}

/// The best model of `tier` that `available` (`provider/model` strings) contains; GitHub Copilot first.
pub fn recommend(tier: Tier, available: &[String]) -> Option<String> {
    for id in tier.list() {
        let hits: Vec<&String> = available.iter().filter(|m| model_id(m) == *id).collect();
        if let Some(m) = hits.iter().find(|m| m.starts_with("github-copilot/")).or_else(|| hits.first()) {
            return Some((*m).clone());
        }
    }
    None
}

/// Published GitHub Copilot token prices, USD per 1M tokens input/output (docs.github.com, 2026-10).
pub fn price_hint(model: &str) -> &'static str {
    match model_id(model) {
        "claude-haiku-5.5" | "gpt-6-luna" => "$0.10 / $0.50",
        "gpt-5.6-luna" | "mai-code-1.1-flash" => "$0.20 / $1.20",
        "gpt-5.4-nano" => "$0.20 / $1.25",
        "gemini-3.8-flash" => "$0.75 / $3.75",
        "claude-haiku-4.5" => "$1 / $5",
        "gpt-5.3-codex" => "$1.75 / $14",
        "claude-sonnet-5" | "claude-sonnet-5.5" | "gpt-6-sol" | "gpt-6.1-sol" | "gemini-4-argon" => "$2 / $10",
        "gpt-5.6-terra" => "$2 / $12",
        "grok-4.5" | "grok-4.6" | "grok-4.7" => "$2 / $6",
        "claude-sonnet-4.6" | "kimi-k3" => "$3 / $15",
        "claude-opus-5.5" | "gpt-5.6-sol" => "$4 / $20",
        "claude-opus-4.8" | "claude-opus-5" => "$5 / $25",
        "claude-fable-5" | "claude-fable-5.1" | "gpt-6-astra" => "$10 / $50",
        _ => "",
    }
}

// ---- config ---------------------------------------------------------------------------------

/// The opencode config, plus whether it is plain JSON (a JSONC file with comments is not rewritten).
pub fn load_config(root: &Path) -> Result<(Value, bool), String> {
    let path = root.join("opencode.jsonc");
    if !path.exists() {
        return Ok((json!({ "$schema": "https://opencode.ai/config.json" }), true));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    match serde_json::from_str(&text) {
        Ok(v) => Ok((v, true)),
        Err(_) => Ok((crate::model_guard::read_jsonc(&path).unwrap_or(Value::Null), false)),
    }
}

/// `agents.<name>.model` strings (v2 key `agents`, v1 key `agent`).
pub fn current_models(cfg: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for key in ["agent", "agents"] {
        if let Some(map) = cfg.get(key).and_then(Value::as_object) {
            for (name, v) in map {
                if let Some(m) = v.get("model").and_then(Value::as_str) {
                    out.insert(name.clone(), m.to_string());
                }
            }
        }
    }
    out
}

/// Set (`Some`) or remove (`None`) each agent's model, keeping everything else in its entry.
pub fn apply_models(cfg: &mut Value, changes: &BTreeMap<String, Option<String>>) {
    let key = if cfg.get("agents").is_some_and(Value::is_object) || !cfg.get("agent").is_some_and(Value::is_object) {
        "agents"
    } else {
        "agent"
    };
    let Some(obj) = cfg.as_object_mut() else { return };
    let table = obj.entry(key).or_insert_with(|| json!({}));
    let Some(table) = table.as_object_mut() else { return };
    for (name, model) in changes {
        match model {
            Some(m) => {
                let entry = table.entry(name.clone()).or_insert_with(|| json!({}));
                if let Some(e) = entry.as_object_mut() {
                    e.insert("model".into(), json!(m));
                }
            }
            None => {
                let empty = table.get_mut(name).and_then(Value::as_object_mut).map(|e| {
                    e.remove("model");
                    e.is_empty()
                });
                if empty == Some(true) {
                    table.remove(name);
                }
            }
        }
    }
    if table.is_empty() {
        obj.remove(key);
    }
}

fn save_config(root: &Path, cfg: &Value) -> Result<PathBuf, String> {
    let path = root.join("opencode.jsonc");
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    if path.exists() {
        std::fs::copy(&path, root.join("opencode.jsonc.bak-a2a-models")).map_err(|e| format!("cannot back up {}: {e}", path.display()))?;
    }
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

// ---- model state ----------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct AgentRow {
    pub agent: String,
    pub description: String,
    pub current: Option<String>,
    pub choice: Option<String>,
    pub tier: Option<Tier>,
    pub rec: Option<String>,
    /// The current model is not among the models this opencode has.
    pub unavailable: bool,
}

/// `(agent, description, model opencode resolved for it)`. The config override wins over the model
/// from the agent file, as in opencode.
pub type AgentEntry = (String, String, Option<String>);

pub fn build_rows(agents: Vec<AgentEntry>, current: &BTreeMap<String, String>, available: &[String]) -> Vec<AgentRow> {
    let mut rows: Vec<AgentRow> = agents
        .into_iter()
        .map(|(agent, description, resolved)| {
            let tier = tier_for(&agent);
            let cur = current.get(&agent).cloned().or(resolved);
            let base = |m: &str| m.split('#').next().unwrap_or(m).to_string();
            AgentRow {
                rec: tier.and_then(|t| recommend(t, available)),
                tier,
                unavailable: cur.as_deref().is_some_and(|m| !available.iter().any(|a| *a == base(m))),
                current: cur.clone(),
                choice: cur,
                description,
                agent,
            }
        })
        .collect();
    rows.sort_by(|a, b| a.agent.cmp(&b.agent));
    rows
}

struct Picker {
    filter: String,
    sel: usize,
}

pub struct App {
    pub rows: Vec<AgentRow>,
    pub models: Vec<String>,
    pub cursor: usize,
    picker: Option<Picker>,
    pub save: bool,
    pub done: bool,
    pub message: String,
}

const DEFAULT_LABEL: &str = "(no override: the agent file's or opencode's default)";

impl App {
    pub fn new(rows: Vec<AgentRow>, models: Vec<String>) -> Self {
        App { rows, models, cursor: 0, picker: None, save: false, done: false, message: String::new() }
    }

    /// Models matching the picker filter; `None` is the "no override" entry.
    fn picker_items(&self) -> Vec<Option<String>> {
        let f = self.picker.as_ref().map(|p| p.filter.to_lowercase()).unwrap_or_default();
        let mut v = vec![None];
        v.extend(self.models.iter().filter(|m| f.is_empty() || m.to_lowercase().contains(&f)).cloned().map(Some));
        v
    }

    pub fn changed(&self) -> usize {
        self.rows.iter().filter(|r| r.choice != r.current).count()
    }

    pub fn key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.done = true;
            return;
        }
        if self.picker.is_some() {
            self.picker_key(k.code);
        } else {
            self.list_key(k.code);
        }
    }

    fn list_key(&mut self, code: KeyCode) {
        self.message.clear();
        let last = self.rows.len().saturating_sub(1);
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.cursor = (self.cursor + 1).min(last),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = last,
            KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') => {
                if !self.rows.is_empty() {
                    self.picker = Some(Picker { filter: String::new(), sel: 0 });
                }
            }
            KeyCode::Char('r') => {
                let n = self.rows.iter().filter(|r| r.rec.is_some()).count();
                for r in self.rows.iter_mut().filter(|r| r.rec.is_some()) {
                    r.choice = r.rec.clone();
                }
                self.message = format!("recommended models set for {n} agents (not saved yet)");
            }
            KeyCode::Char('c') => {
                for r in &mut self.rows {
                    r.choice = None;
                }
                self.message = "all overrides cleared (not saved yet)".into();
            }
            KeyCode::Char('u') => {
                for r in &mut self.rows {
                    r.choice = r.current.clone();
                }
                self.message = "changes undone".into();
            }
            KeyCode::Char('s') => {
                self.save = true;
                self.done = true;
            }
            KeyCode::Char('q') | KeyCode::Esc => self.done = true,
            _ => {}
        }
    }

    fn picker_key(&mut self, code: KeyCode) {
        let n = self.picker_items().len();
        let Some(p) = self.picker.as_mut() else { return };
        match code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Up => p.sel = p.sel.saturating_sub(1),
            KeyCode::Down => p.sel = (p.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Backspace => {
                p.filter.pop();
                p.sel = 0;
            }
            KeyCode::Char(c) => {
                p.filter.push(c);
                p.sel = 0;
            }
            KeyCode::Enter => {
                let items = self.picker_items();
                let sel = self.picker.as_ref().map(|p| p.sel).unwrap_or(0);
                if let Some(item) = items.get(sel) {
                    if let Some(row) = self.rows.get_mut(self.cursor) {
                        row.choice = item.clone();
                    }
                }
                self.picker = None;
            }
            _ => {}
        }
    }
}

// ---- drawing --------------------------------------------------------------------------------

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

pub fn draw(f: &mut Frame, app: &App) {
    let [head, body, foot] = Layout::vertical([Constraint::Length(2), Constraint::Min(5), Constraint::Length(4)]).areas(f.area());
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled("Agent models", Style::default().add_modifier(Modifier::BOLD))),
            Line::from(format!("{} agents, {} models available, {} unsaved change(s)", app.rows.len(), app.models.len(), app.changed())),
        ]),
        head,
    );

    let header = Row::new(["", "Agent", "Model", "Recommended", "$/1M in / out"]).style(Style::default().add_modifier(Modifier::BOLD));
    let rows: Vec<Row> = app
        .rows
        .iter()
        .map(|r| {
            let changed = r.choice != r.current;
            let missing = r.unavailable && r.choice == r.current;
            let model = match (&r.choice, missing) {
                (Some(m), true) => format!("{m}  (not available here)"),
                (Some(m), false) => m.clone(),
                (None, _) => "(default)".into(),
            };
            let style = if changed { Style::default().fg(Color::Yellow) } else { Style::default() };
            Row::new([
                Cell::from(if changed { "*" } else { " " }),
                Cell::from(r.agent.clone()),
                Cell::from(model).style(if missing { Style::default().fg(Color::Red) } else { Style::default() }),
                Cell::from(r.rec.clone().unwrap_or_default()),
                Cell::from(r.choice.as_deref().map(price_hint).unwrap_or("")),
            ])
            .style(style)
        })
        .collect();
    let table = Table::new(
        rows,
        [Constraint::Length(2), Constraint::Length(30), Constraint::Min(30), Constraint::Min(28), Constraint::Length(14)],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL))
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol("> ");
    let mut state = TableState::default().with_selected(Some(app.cursor));
    f.render_stateful_widget(table, body, &mut state);

    let hint = app
        .rows
        .get(app.cursor)
        .map(|r| match r.tier {
            Some(t) => format!("{}: {}", r.agent, t.why()),
            None => format!("{}: no recommendation (your own agent); pick any model or leave the default", r.agent),
        })
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(vec![
            Line::from(hint),
            Line::from(Span::styled(app.message.clone(), Style::default().fg(Color::Green))),
            Line::from("up/down move   enter pick   r recommended   c clear   u undo   s save   q quit"),
        ])
        .block(Block::default().borders(Borders::TOP)),
        foot,
    );

    if let Some(p) = &app.picker {
        let area = centered(f.area(), 70, f.area().height.saturating_sub(4).min(24));
        f.render_widget(Clear, area);
        let agent = app.rows.get(app.cursor).map(|r| r.agent.as_str()).unwrap_or("");
        let items = app.picker_items();
        let list: Vec<ListItem> = items
            .iter()
            .map(|m| match m {
                None => ListItem::new(DEFAULT_LABEL),
                Some(m) => {
                    let star = if app.rows.get(app.cursor).and_then(|r| r.rec.as_deref()) == Some(m.as_str()) { " (recommended)" } else { "" };
                    ListItem::new(format!("{m}  {}{star}", price_hint(m)))
                }
            })
            .collect();
        let list = List::new(list)
            .block(Block::default().borders(Borders::ALL).title(format!(" model for {agent}   filter: {}_ ", p.filter)))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        let mut ls = ListState::default().with_selected(Some(p.sel.min(items.len().saturating_sub(1))));
        f.render_stateful_widget(list, area, &mut ls);
    }
}

fn run_tui(app: &mut App) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = (|| {
        while !app.done {
            terminal.draw(|f| draw(f, app))?;
            if event::poll(Duration::from_millis(250))? {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press {
                        app.key(k);
                    }
                }
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}

// ---- commands -------------------------------------------------------------------------------

/// Agents to configure: everything opencode lists (not hidden), else the installed templates.
fn agent_list(root: &Path) -> Vec<AgentEntry> {
    if let Ok(list) = opencode::agents(None) {
        let v: Vec<AgentEntry> =
            list.into_iter().filter(|a| !a.hidden).map(|a| (a.id, a.description, a.model.as_ref().map(model_string))).collect();
        if !v.is_empty() {
            return v;
        }
    }
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root.join("agents")) {
        for e in rd.flatten() {
            if let Some(n) = e.file_name().to_string_lossy().strip_suffix(".md") {
                v.push((n.to_string(), String::new(), frontmatter_model(&e.path())));
            }
        }
    }
    v
}

/// `{providerID, id, variant?}` -> `provider/id[#variant]`.
fn model_string(m: &Value) -> String {
    let base = format!("{}/{}", m["providerID"].as_str().unwrap_or(""), m["id"].as_str().unwrap_or(""));
    match m["variant"].as_str().filter(|v| !v.is_empty()) {
        Some(v) => format!("{base}#{v}"),
        None => base,
    }
}

/// The `model:` line of an agent file's frontmatter.
fn frontmatter_model(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let fm = text.splitn(3, "---\n").nth(1)?;
    fm.lines().find_map(|l| l.strip_prefix("model:")).map(|m| m.trim().trim_matches('"').to_string()).filter(|m| !m.is_empty())
}

/// After `init`: the installed agent files that name a model this opencode does not have. Best
/// effort (silent when opencode cannot be asked).
pub fn availability_note(root: &Path) -> Option<String> {
    let available = opencode::models().ok()?;
    let mut missing = Vec::new();
    for e in std::fs::read_dir(root.join("agents")).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().strip_suffix(".md")?.to_string();
        if let Some(m) = frontmatter_model(&e.path()) {
            if !available.iter().any(|a| *a == m.split('#').next().unwrap_or(&m)) {
                missing.push(format!("{name} ({m})"));
            }
        }
    }
    if missing.is_empty() {
        return None;
    }
    missing.sort();
    Some(format!(
        "{} agent(s) name a model this opencode does not have: {}. Run `codetypera2a models --recommended` to pick from the models you have.",
        missing.len(),
        missing.join(", ")
    ))
}

fn print_table(rows: &[AgentRow]) {
    println!("{:<30} {:<52} {:<40} {}", "agent", "model", "recommended", "$/1M in / out");
    for r in rows {
        let mut m = r.choice.clone().unwrap_or_else(|| "(default)".into());
        if r.unavailable && r.choice == r.current {
            m.push_str(" (not available here)");
        }
        println!("{:<30} {:<52} {:<40} {}", r.agent, m, r.rec.clone().unwrap_or_default(), r.choice.as_deref().map(price_hint).unwrap_or(""));
    }
}

fn commit(root: &Path, mut cfg: Value, strict: bool, rows: &[AgentRow]) -> Result<(), String> {
    let changes: BTreeMap<String, Option<String>> =
        rows.iter().filter(|r| r.choice != r.current).map(|r| (r.agent.clone(), r.choice.clone())).collect();
    if changes.is_empty() {
        println!("models: nothing changed");
        return Ok(());
    }
    apply_models(&mut cfg, &changes);
    if !strict {
        // comments or trailing commas would be lost: show what to add instead
        let mut only = Value::Object(Default::default());
        apply_models(&mut only, &changes);
        println!("models: opencode.jsonc has comments, so it was left untouched. Add this to it:\n{}", serde_json::to_string_pretty(&only).unwrap_or_default());
        return Ok(());
    }
    let path = save_config(root, &cfg)?;
    println!("models: {} agent model(s) saved in {} (backup: opencode.jsonc.bak-a2a-models)", changes.len(), path.display());
    for note in policy_violations(root, &changes) {
        println!("models: note: {note}");
    }
    println!("models: restart opencode (or `opencode reload`); new agent sessions use these models");
    Ok(())
}

/// Chosen models that the installed `a2a/model-policy.json` would flag in `check-models`.
fn policy_violations(root: &Path, changes: &BTreeMap<String, Option<String>>) -> Vec<String> {
    let Ok(policy) = crate::model_guard::load_policy(&root.join("a2a").join("model-policy.json")) else { return vec![] };
    changes
        .iter()
        .filter_map(|(agent, m)| {
            let m = m.as_ref()?;
            let c = crate::model_guard::check(m, Some(&policy));
            (!c.ok).then(|| format!("{agent}: {m} is not allowed by your model-policy.json ({}); `check-models` will flag it", c.reason.unwrap_or_default()))
        })
        .collect()
}

/// Shared by `codetypera2a models` and `init`. `interactive`: show the UI when attached to a terminal.
pub fn configure(root: &Path, recommended: bool, show: bool, interactive: bool) -> Result<(), String> {
    let on_terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if !recommended && !show && !(interactive && on_terminal) {
        println!("models: not a terminal; run `codetypera2a models` to choose models, or `codetypera2a models --recommended`");
        return Ok(());
    }
    let available = opencode::models()?;
    let (cfg, strict) = load_config(root)?;
    let mut rows = build_rows(agent_list(root), &current_models(&cfg), &available);
    if rows.is_empty() {
        return Err("no agents found (run `codetypera2a init` first)".into());
    }
    if show {
        print_table(&rows);
        return Ok(());
    }
    if recommended {
        for r in rows.iter_mut().filter(|r| r.rec.is_some()) {
            r.choice = r.rec.clone();
        }
        print_table(&rows);
        return commit(root, cfg, strict, &rows);
    }
    let mut app = App::new(rows, available);
    run_tui(&mut app).map_err(|e| format!("terminal error: {e}"))?;
    if app.save {
        commit(root, cfg, strict, &app.rows)
    } else {
        println!("models: quit without saving");
        Ok(())
    }
}

fn config_root(args: &[String]) -> PathBuf {
    match args.iter().position(|a| a == "--dir") {
        Some(i) => args.get(i + 1).map(PathBuf::from).unwrap_or_else(paths::config_dir),
        None => paths::config_dir(),
    }
}

/// `codetypera2a models [--recommended] [--show] [--dir <path>]`
pub fn run(args: &[String]) -> i32 {
    let root = config_root(args);
    let flag = |f: &str| args.iter().any(|a| a == f);
    match configure(&root, flag("--recommended"), flag("--show"), true) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("models: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn avail(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn recommends_by_role_and_falls_back() {
        let all = avail(&["github-copilot/claude-haiku-5.5", "github-copilot/claude-sonnet-5.5", "github-copilot/claude-opus-5.5", "github-copilot/gpt-6.1-sol"]);
        assert_eq!(recommend(Tier::Cheap, &all).as_deref(), Some("github-copilot/claude-haiku-5.5"));
        assert_eq!(recommend(Tier::Balanced, &all).as_deref(), Some("github-copilot/claude-sonnet-5.5"));
        assert_eq!(recommend(Tier::Deep, &all).as_deref(), Some("github-copilot/claude-opus-5.5"));
        assert_eq!(recommend(Tier::OtherFamily, &all).as_deref(), Some("github-copilot/gpt-6.1-sol"));
        // no OpenAI model: reviewers fall back to the balanced Claude; nothing suitable: no recommendation
        let claude = avail(&["github-copilot/claude-sonnet-5.5"]);
        assert_eq!(recommend(Tier::OtherFamily, &claude).as_deref(), Some("github-copilot/claude-sonnet-5.5"));
        assert_eq!(recommend(Tier::Deep, &avail(&["x/unknown"])), None);
        // a direct provider works too, and GitHub Copilot is preferred when both exist
        let both = avail(&["anthropic/claude-sonnet-5.5", "github-copilot/claude-sonnet-5.5"]);
        assert_eq!(recommend(Tier::Balanced, &both).as_deref(), Some("github-copilot/claude-sonnet-5.5"));
        assert_eq!(recommend(Tier::Balanced, &avail(&["anthropic/claude-sonnet-5.5"])).as_deref(), Some("anthropic/claude-sonnet-5.5"));
    }

    #[test]
    fn policy_violations_are_reported_not_enforced() {
        let dir = std::env::temp_dir().join(format!("a2a-pol-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a2a")).unwrap();
        std::fs::write(dir.join("a2a/model-policy.json"), r#"{"default":"p/ok","allow":{"p/ok":"cheap"},"denyPatterns":["opus"]}"#).unwrap();
        let ch = BTreeMap::from([("architect".to_string(), Some("p/claude-opus-5.5".to_string())), ("brain".to_string(), Some("p/ok".to_string())), ("x".to_string(), None)]);
        let v = policy_violations(&dir, &ch);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].starts_with("architect:") && v[0].contains("opus"), "{v:?}");
        assert!(policy_violations(&dir.join("nowhere"), &ch).is_empty(), "no policy file, no notes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn roles_map_to_tiers() {
        assert_eq!(tier_for("architect"), Some(Tier::Deep));
        assert_eq!(tier_for("code-reviewer"), Some(Tier::OtherFamily));
        assert_eq!(tier_for("test-runner"), Some(Tier::Cheap));
        assert_eq!(tier_for("orchestrator"), Some(Tier::Balanced));
        assert_eq!(tier_for("someones-own-agent"), None);
    }

    #[test]
    fn config_changes_are_merged_not_replaced() {
        let mut cfg = json!({"model":"x/y","agents":{"mentor":{"model":"a/b","temperature":0.2},"plan":{"description":"d"}}});
        let mut ch = BTreeMap::new();
        ch.insert("mentor".to_string(), None); // remove model, keep temperature
        ch.insert("plan".to_string(), Some("github-copilot/claude-sonnet-5.5".to_string()));
        ch.insert("brain".to_string(), Some("github-copilot/claude-haiku-5.5#high".to_string()));
        apply_models(&mut cfg, &ch);
        assert_eq!(cfg["model"], "x/y");
        assert_eq!(cfg["agents"]["mentor"], json!({"temperature":0.2}));
        assert_eq!(cfg["agents"]["plan"], json!({"description":"d","model":"github-copilot/claude-sonnet-5.5"}));
        assert_eq!(cfg["agents"]["brain"]["model"], "github-copilot/claude-haiku-5.5#high");
        assert_eq!(current_models(&cfg).get("brain").map(String::as_str), Some("github-copilot/claude-haiku-5.5#high"));
        // removing the last field removes the entry, and an empty table is dropped
        let mut only = json!({"agents":{"a":{"model":"x/y"}}});
        apply_models(&mut only, &BTreeMap::from([("a".to_string(), None)]));
        assert!(only.get("agents").is_none());
        // a v1 `agent` table is kept as is
        let mut v1 = json!({"agent":{"plan":{"model":"p/m"}}});
        apply_models(&mut v1, &BTreeMap::from([("brain".to_string(), Some("p/n".to_string()))]));
        assert_eq!(v1["agent"]["brain"]["model"], "p/n");
        assert!(v1.get("agents").is_none());
    }

    #[test]
    fn keys_drive_the_picker() {
        let models = avail(&["github-copilot/claude-haiku-5.5", "github-copilot/claude-sonnet-5.5", "github-copilot/gpt-6.1-sol"]);
        let rows = build_rows(vec![("brain".into(), "".into(), None), ("code-reviewer".into(), "".into(), None)], &BTreeMap::new(), &models);
        let mut app = App::new(rows, models);
        assert_eq!(app.rows[0].agent, "brain");
        // pick by typing a filter: enter opens, type "sol", enter chooses the first match
        app.key(key(KeyCode::Enter));
        for c in "sol".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Down)); // index 0 is "(no override)", 1 is the first match
        app.key(key(KeyCode::Enter));
        assert_eq!(app.rows[0].choice.as_deref(), Some("github-copilot/gpt-6.1-sol"));
        assert_eq!(app.changed(), 1);
        // r applies every recommendation, u undoes, c clears
        app.key(key(KeyCode::Char('r')));
        assert_eq!(app.rows[0].choice.as_deref(), Some("github-copilot/claude-haiku-5.5"));
        assert_eq!(app.rows[1].choice.as_deref(), Some("github-copilot/gpt-6.1-sol"));
        app.key(key(KeyCode::Char('u')));
        assert_eq!(app.changed(), 0);
        app.key(key(KeyCode::Char('c')));
        assert!(app.rows.iter().all(|r| r.choice.is_none()));
        // escape closes the picker without choosing; q quits without saving; s saves
        app.key(key(KeyCode::Enter));
        app.key(key(KeyCode::Esc));
        assert!(!app.done);
        app.key(key(KeyCode::Char('q')));
        assert!(app.done && !app.save);
        let mut app2 = App::new(vec![], vec![]);
        app2.key(key(KeyCode::Char('s')));
        assert!(app2.done && app2.save);
    }

    #[test]
    fn the_screen_renders() {
        use ratatui::{Terminal, backend::TestBackend};
        let models = avail(&["github-copilot/claude-haiku-5.5"]);
        let rows = build_rows(vec![("orchestrator".into(), "".into(), None)], &BTreeMap::new(), &models);
        let mut app = App::new(rows, models);
        let mut term = Terminal::new(TestBackend::new(110, 24)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("orchestrator") && text.contains("Agent models"));
        app.key(key(KeyCode::Enter));
        term.draw(|f| draw(f, &app)).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("model for orchestrator") && text.contains("claude-haiku-5.5"));
    }

    fn templates() -> Vec<(String, Option<String>)> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("agents");
        let mut v: Vec<(String, Option<String>)> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| (e.path().file_stem().unwrap().to_string_lossy().into_owned(), frontmatter_model(&e.path())))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn every_template_carries_the_recommended_model_of_its_tier() {
        let t = templates();
        assert!(t.len() >= 21, "expected all agent templates, saw {}", t.len());
        for (name, model) in t {
            let tier = tier_for(&name).unwrap_or_else(|| panic!("{name}: no tier in tier_for()"));
            let want = format!("github-copilot/{}", tier.list()[0]);
            assert_eq!(model.as_deref(), Some(want.as_str()), "{name}: template model must be its tier's first choice");
        }
    }

    #[test]
    fn the_shipped_policy_allows_every_template_model_and_keeps_readonly_agents_cheap() {
        let policy = crate::model_guard::load_policy(&Path::new(env!("CARGO_MANIFEST_DIR")).join("model-policy.json")).unwrap();
        let t = templates();
        for (name, model) in &t {
            let m = model.as_deref().unwrap();
            let c = crate::model_guard::check(m, Some(&policy));
            assert!(c.ok, "{name}: {m} must be allowed by the shipped policy: {:?}", c.reason);
            assert_eq!(policy.agent_defaults.get(name).map(String::as_str), Some(m), "{name}: agentDefaults must match the template");
        }
        for name in &policy.read_only_agents {
            if let Some((_, Some(m))) = t.iter().find(|(n, _)| n == name) {
                assert_eq!(crate::model_guard::check(m, Some(&policy)).tier.as_deref(), Some("cheap"), "{name} is listed read-only, so it must use a cheap model");
            }
        }
    }

    #[test]
    fn current_model_comes_from_the_agent_file_unless_the_config_overrides_it() {
        let models = avail(&["github-copilot/claude-haiku-5.5", "github-copilot/claude-sonnet-5.5"]);
        let agents = vec![
            ("brain".to_string(), "".to_string(), Some("github-copilot/claude-haiku-5.5".to_string())),
            ("plan".to_string(), "".to_string(), Some("github-copilot/claude-opus-5.5".to_string())), // not available here
            ("mentor".to_string(), "".to_string(), Some("github-copilot/claude-haiku-5.5".to_string())),
        ];
        let cfg = BTreeMap::from([("mentor".to_string(), "github-copilot/claude-sonnet-5.5#high".to_string())]);
        let rows = build_rows(agents, &cfg, &models);
        let by = |n: &str| rows.iter().find(|r| r.agent == n).unwrap();
        assert_eq!(by("brain").current.as_deref(), Some("github-copilot/claude-haiku-5.5"));
        assert!(!by("brain").unavailable);
        assert!(by("plan").unavailable, "a model this opencode lacks is flagged");
        assert_eq!(by("mentor").current.as_deref(), Some("github-copilot/claude-sonnet-5.5#high"), "config wins");
        assert!(!by("mentor").unavailable, "a #variant does not make it unavailable");
    }
}
