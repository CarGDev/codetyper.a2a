use super::deep::DeepList;
use super::tiers::{Tier, effective_tier, price_hint, recommend};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
    },
};
use std::collections::BTreeMap;
use std::time::Duration;

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

/// `(agent, description, model opencode resolved for it)`. opencode takes the model from the agent
/// file first, so `sync_agent_files` keeps the file in step with the config override.
pub type AgentEntry = (String, String, Option<String>);

pub fn build_rows(
    agents: Vec<AgentEntry>,
    current: &BTreeMap<String, String>,
    available: &[String],
    deep: &DeepList,
) -> Vec<AgentRow> {
    let mut rows: Vec<AgentRow> = agents
        .into_iter()
        .map(|(agent, description, resolved)| {
            let tier = effective_tier(&agent, deep);
            let cur = current.get(&agent).cloned().or(resolved);
            let base = |m: &str| m.split('#').next().unwrap_or(m).to_string();
            AgentRow {
                rec: tier.and_then(|t| recommend(t, available)),
                tier,
                unavailable: cur
                    .as_deref()
                    .is_some_and(|m| !available.iter().any(|a| *a == base(m))),
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
        App {
            rows,
            models,
            cursor: 0,
            picker: None,
            save: false,
            done: false,
            message: String::new(),
        }
    }

    /// Models matching the picker filter; `None` is the "no override" entry.
    fn picker_items(&self) -> Vec<Option<String>> {
        let f = self
            .picker
            .as_ref()
            .map(|p| p.filter.to_lowercase())
            .unwrap_or_default();
        let mut v = vec![None];
        v.extend(
            self.models
                .iter()
                .filter(|m| f.is_empty() || m.to_lowercase().contains(&f))
                .cloned()
                .map(Some),
        );
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
                    self.picker = Some(Picker {
                        filter: String::new(),
                        sel: 0,
                    });
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
        let Some(p) = self.picker.as_mut() else {
            return;
        };
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
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(5),
        Constraint::Length(4),
    ])
    .areas(f.area());
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Agent models",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(format!(
                "{} agents, {} models available, {} unsaved change(s)",
                app.rows.len(),
                app.models.len(),
                app.changed()
            )),
        ]),
        head,
    );

    let header = Row::new(["", "Agent", "Model", "Recommended", "$/1M in / out"])
        .style(Style::default().add_modifier(Modifier::BOLD));
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
            let style = if changed {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            Row::new([
                Cell::from(if changed { "*" } else { " " }),
                Cell::from(r.agent.clone()),
                Cell::from(model).style(if missing {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default()
                }),
                Cell::from(r.rec.clone().unwrap_or_default()),
                Cell::from(r.choice.as_deref().map(price_hint).unwrap_or("")),
            ])
            .style(style)
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Length(30),
            Constraint::Min(30),
            Constraint::Min(28),
            Constraint::Length(14),
        ],
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
            None => format!(
                "{}: no recommendation (your own agent); pick any model or leave the default",
                r.agent
            ),
        })
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(vec![
            Line::from(hint),
            Line::from(Span::styled(
                app.message.clone(),
                Style::default().fg(Color::Green),
            )),
            Line::from(
                "up/down move   enter pick   r recommended   c clear   u undo   s save   q quit",
            ),
        ])
        .block(Block::default().borders(Borders::TOP)),
        foot,
    );

    if let Some(p) = &app.picker {
        let area = centered(f.area(), 70, f.area().height.saturating_sub(4).min(24));
        f.render_widget(Clear, area);
        let agent = app
            .rows
            .get(app.cursor)
            .map(|r| r.agent.as_str())
            .unwrap_or("");
        let items = app.picker_items();
        let list: Vec<ListItem> = items
            .iter()
            .map(|m| match m {
                None => ListItem::new(DEFAULT_LABEL),
                Some(m) => {
                    let star = if app.rows.get(app.cursor).and_then(|r| r.rec.as_deref())
                        == Some(m.as_str())
                    {
                        " (recommended)"
                    } else {
                        ""
                    };
                    ListItem::new(format!("{m}  {}{star}", price_hint(m)))
                }
            })
            .collect();
        let list = List::new(list)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" model for {agent}   filter: {}_ ", p.filter)),
            )
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        let mut ls =
            ListState::default().with_selected(Some(p.sel.min(items.len().saturating_sub(1))));
        f.render_stateful_widget(list, area, &mut ls);
    }
}

pub(super) fn run_tui(app: &mut App) -> std::io::Result<()> {
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
    fn keys_drive_the_picker() {
        let models = avail(&[
            "github-copilot/claude-haiku-5.5",
            "github-copilot/claude-sonnet-5.5",
            "github-copilot/gpt-6.1-sol",
        ]);
        let rows = build_rows(
            vec![
                ("brain".into(), "".into(), None),
                ("code-reviewer".into(), "".into(), None),
            ],
            &BTreeMap::new(),
            &models,
            &DeepList::default(),
        );
        let mut app = App::new(rows, models);
        assert_eq!(app.rows[0].agent, "brain");
        // pick by typing a filter: enter opens, type "sol", enter chooses the first match
        app.key(key(KeyCode::Enter));
        for c in "sol".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        app.key(key(KeyCode::Down)); // index 0 is "(no override)", 1 is the first match
        app.key(key(KeyCode::Enter));
        assert_eq!(
            app.rows[0].choice.as_deref(),
            Some("github-copilot/gpt-6.1-sol")
        );
        assert_eq!(app.changed(), 1);
        // r applies every recommendation, u undoes, c clears
        app.key(key(KeyCode::Char('r')));
        assert_eq!(
            app.rows[0].choice.as_deref(),
            Some("github-copilot/claude-haiku-5.5")
        );
        assert_eq!(
            app.rows[1].choice.as_deref(),
            Some("github-copilot/claude-sonnet-5.5")
        );
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
        let rows = build_rows(
            vec![("orchestrator".into(), "".into(), None)],
            &BTreeMap::new(),
            &models,
            &DeepList::default(),
        );
        let mut app = App::new(rows, models);
        let mut term = Terminal::new(TestBackend::new(110, 24)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("orchestrator") && text.contains("Agent models"));
        app.key(key(KeyCode::Enter));
        term.draw(|f| draw(f, &app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("model for orchestrator") && text.contains("claude-haiku-5.5"));
    }
}
