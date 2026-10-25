//! The "Which agents should codetypera2a configure?" screen of `init`: a multi-select over `TARGETS`.
//! The state and key handling are plain data (`Picker`), so they are tested without a terminal.
use super::{Id, Probe, TARGETS, Target, detected};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::time::Duration;

pub struct Row {
    pub target: &'static Target,
    pub detected: bool,
    pub checked: bool,
}

pub struct Picker {
    pub rows: Vec<Row>,
    pub cursor: usize,
    pub done: bool,
    pub confirmed: bool,
}

impl Picker {
    /// Every target listed; the installable ones that were found start checked.
    pub fn new(probe: &Probe) -> Picker {
        let found = detected(probe);
        let rows = TARGETS
            .iter()
            .map(|t| Row {
                target: t,
                detected: found.contains(&t.id),
                checked: found.contains(&t.id),
            })
            .collect();
        let mut p = Picker {
            rows,
            cursor: 0,
            done: false,
            confirmed: false,
        };
        p.cursor = p.selectable().first().copied().unwrap_or(0);
        p
    }

    /// Row indexes that can be toggled; "coming soon" rows are skipped by the cursor.
    fn selectable(&self) -> Vec<usize> {
        (0..self.rows.len())
            .filter(|i| self.rows[*i].target.installable())
            .collect()
    }

    fn step(&mut self, delta: isize) {
        let ready = self.selectable();
        let Some(pos) = ready.iter().position(|i| *i == self.cursor) else {
            return;
        };
        let next = (pos as isize + delta).rem_euclid(ready.len() as isize) as usize;
        self.cursor = ready[next];
    }

    fn toggle_all(&mut self) {
        let ready = self.selectable();
        let all_on = ready.iter().all(|i| self.rows[*i].checked);
        ready
            .into_iter()
            .for_each(|i| self.rows[i].checked = !all_on);
    }

    pub fn key(&mut self, k: KeyEvent) {
        let ctrl_c = k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c');
        match k.code {
            _ if ctrl_c => self.done = true,
            KeyCode::Esc | KeyCode::Char('q') => self.done = true,
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::Char(' ') => {
                let row = &mut self.rows[self.cursor];
                row.checked = row.target.installable() && !row.checked;
            }
            KeyCode::Char('a') => self.toggle_all(),
            KeyCode::Enter => (self.done, self.confirmed) = (true, true),
            _ => {}
        }
    }

    pub fn selected(&self) -> Vec<Id> {
        self.rows
            .iter()
            .filter(|r| r.checked)
            .map(|r| r.target.id)
            .collect()
    }
}

fn lines(p: &Picker) -> Vec<Line<'static>> {
    let mut out = vec![
        Line::styled(
            "Which agents should codetypera2a configure?",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ];
    for (i, row) in p.rows.iter().enumerate() {
        let ready = row.target.installable();
        let glyph = if row.checked { "■" } else { "□" };
        let glyph_style = match (ready, row.checked) {
            (false, _) => Style::default().fg(Color::DarkGray),
            (true, true) => Style::default().fg(Color::Green),
            (true, false) => Style::default(),
        };
        let mut label = Style::default().fg(if ready { Color::Reset } else { Color::DarkGray });
        if i == p.cursor {
            label = label.add_modifier(Modifier::BOLD);
        }
        out.push(Line::from(vec![
            Span::raw(if i == p.cursor { "› " } else { "  " }),
            Span::styled(glyph, glyph_style),
            Span::raw(" "),
            Span::styled(row.target.label(row.detected), label),
        ]));
    }
    out.push(Line::raw(""));
    out.push(Line::styled(
        "↑/↓ move · space toggle · a all · enter confirm · esc cancel",
        Style::default().fg(Color::DarkGray),
    ));
    out
}

fn draw(f: &mut Frame, p: &Picker) {
    f.render_widget(Paragraph::new(lines(p)), f.area());
}

fn run_tui(p: &mut Picker) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = (|| {
        while !p.done {
            terminal.draw(|f| draw(f, p))?;
            if !event::poll(Duration::from_millis(250))? {
                continue;
            }
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    p.key(k);
                }
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}

/// Show the screen. `None` = cancelled (esc, q or ctrl-c).
pub fn pick(probe: &Probe) -> Result<Option<Vec<Id>>, String> {
    let mut p = Picker::new(probe);
    run_tui(&mut p).map_err(|e| format!("terminal error: {e}"))?;
    Ok(p.confirmed.then(|| p.selected()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn picker(bins: &'static [&'static str]) -> Picker {
        let probe = Probe {
            on_path: Box::new(move |b| bins.contains(&b)),
            exists: Box::new(|_: &Path| false),
            home: PathBuf::from("/h"),
        };
        Picker::new(&probe)
    }

    fn press(p: &mut Picker, code: KeyCode) {
        p.key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn text(p: &Picker) -> Vec<String> {
        lines(p)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn detected_targets_start_checked_and_the_cursor_on_the_first_ready_one() {
        let p = picker(&["claude"]);
        assert_eq!(p.selected(), [Id::Claude]);
        assert_eq!(p.cursor, 0);
        let t = text(&p);
        assert_eq!(t[0], "Which agents should codetypera2a configure?");
        assert_eq!(t[2], "› ■ Claude Code (detected) — global only");
        assert_eq!(t[3], "  □ opencode (not found) — global only");
        assert_eq!(t[4], "  □ Codex CLI (coming soon)");
        assert_eq!(t.len(), 7 + 4);
    }

    #[test]
    fn cursor_skips_coming_soon_rows_and_wraps() {
        let mut p = picker(&[]);
        press(&mut p, KeyCode::Down);
        assert_eq!(p.cursor, 1);
        press(&mut p, KeyCode::Char('j'));
        assert_eq!(p.cursor, 0, "wraps past the coming-soon rows");
        press(&mut p, KeyCode::Up);
        assert_eq!(p.cursor, 1);
        press(&mut p, KeyCode::Char('k'));
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn space_toggles_and_a_flips_all() {
        let mut p = picker(&[]);
        assert!(p.selected().is_empty());
        press(&mut p, KeyCode::Char(' '));
        assert_eq!(p.selected(), [Id::Claude]);
        press(&mut p, KeyCode::Char('a'));
        assert_eq!(p.selected(), [Id::Claude, Id::Opencode]);
        press(&mut p, KeyCode::Char('a'));
        assert!(p.selected().is_empty());
    }

    #[test]
    fn coming_soon_rows_cannot_be_selected() {
        let mut p = picker(&["codex"]);
        p.cursor = 2;
        press(&mut p, KeyCode::Char(' '));
        assert!(p.selected().is_empty());
        press(&mut p, KeyCode::Char('a'));
        assert_eq!(p.selected(), [Id::Claude, Id::Opencode]);
    }

    #[test]
    fn enter_confirms_and_esc_or_ctrl_c_cancel() {
        let mut p = picker(&["claude"]);
        press(&mut p, KeyCode::Enter);
        assert!(p.done && p.confirmed);
        for code in [KeyCode::Esc, KeyCode::Char('q')] {
            let mut p = picker(&["claude"]);
            press(&mut p, code);
            assert!(p.done && !p.confirmed);
        }
        let mut p = picker(&["claude"]);
        p.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(p.done && !p.confirmed);
    }
}
