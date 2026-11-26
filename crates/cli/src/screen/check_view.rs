//! The full-screen check view: the emission rate and one line per kind,
//! following the readouts; with `-v`, every candidate and its reason.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;

use super::live::{Live, Status, value_width};
use super::run::Feed;
use super::{SPINNER, dim};
use crate::commands::How;

pub struct CheckView {
    pub live: Live,
    feed: Option<Feed>,
    verbose: bool,
    table: TableState,
}

impl CheckView {
    pub fn new(live: Live, feed: Feed, verbose: bool) -> Self {
        Self { live, feed: Some(feed), verbose, table: TableState::default().with_selected(Some(0)) }
    }

    /// Takes everything the run sent since the last call.
    pub fn pump(&mut self) {
        if let Some(feed) = &self.feed {
            while let Ok(msg) = feed.rx.try_recv() {
                self.live.apply(msg);
            }
        }
    }

    /// Whether `key` closes the view; arrows move the candidate table.
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        let rows = self.live.candidates.len();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
            KeyCode::Down if rows > 0 => {
                let next = self.table.selected().map_or(0, |row| (row + 1).min(rows - 1));
                self.table.select(Some(next));
                false
            }
            KeyCode::Up => {
                let previous = self.table.selected().map_or(0, |row| row.saturating_sub(1));
                self.table.select(Some(previous));
                false
            }
            _ => false,
        }
    }

    /// Stops the run and returns what the view last showed, as plain text.
    pub fn close(mut self) -> String {
        self.pump();
        let started = self.live.started();
        if let Some(feed) = self.feed.take() {
            feed.stop(started);
        }
        self.live.snapshot(self.verbose)
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect, tick: usize) {
        let kinds = self.live.lines();
        let width = value_width(&kinds);
        // The last column takes what the others leave; its text is wrapped to it.
        let by_width = usize::from(area.width).saturating_sub(10 + width + 1 + 10).max(1);
        let rows: Vec<Row> = kinds
            .iter()
            .map(|line| {
                let (value, how) = match line.value.as_str() {
                    "…" if !self.live.started() => {
                        (Span::styled(format!("{} probing…", SPINNER[tick % SPINNER.len()]), dim()), Span::raw(""))
                    }
                    "none" if line.how.is_none() => {
                        (Span::styled("none", Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)), Span::raw(""))
                    }
                    value => (
                        Span::styled(value.to_owned(), Style::new().add_modifier(Modifier::BOLD)),
                        line.how.map_or(Span::raw(""), |how| Span::styled(how.word(), Style::new().fg(colour(how)))),
                    ),
                };
                let mut by: Vec<Line> =
                    wrap(&line.by, by_width).into_iter().map(|text| Line::from(Span::styled(text, dim()))).collect();
                if let Some(failure) = &line.failure {
                    let red = Style::new().fg(Color::Red);
                    by.extend(wrap(&format!("! failed: {failure}"), by_width).into_iter().map(|text| Line::styled(text, red)));
                }
                let height = by.len().max(1) as u16;
                Row::new([
                    Cell::from(Span::styled(line.label, dim())),
                    Cell::from(value),
                    Cell::from(how),
                    Cell::from(Text::from(by)),
                ])
                .height(height)
            })
            .collect();
        let kinds_height: u16 = kinds
            .iter()
            .map(|line| {
                let failure = line.failure.as_ref().map_or(0, |failure| wrap(&format!("! failed: {failure}"), by_width).len());
                (wrap(&line.by, by_width).len().max(1) + failure) as u16
            })
            .sum();
        // The same columns as the printed snapshot: label, value, how, then the sources.
        let kind_widths =
            [Constraint::Length(10), Constraint::Length(width as u16 + 1), Constraint::Length(10), Constraint::Fill(1)];

        let mut rest: Vec<Line> = Vec::new();
        for warning in self.live.warnings() {
            rest.push(Line::from(""));
            for (index, text) in warning.lines().enumerate() {
                let style = if index == 0 { Style::new().fg(Color::Yellow) } else { dim() };
                rest.push(Line::from(Span::styled(text.to_owned(), style)));
            }
        }
        if !self.verbose {
            rest.push(Line::from(""));
            rest.push(Line::from(Span::styled("check -v lists every candidate and why it was or wasn't chosen", dim())));
        }
        let rest_height = rest.len() as u16 + 1;
        let [head, kinds_area, rest_area, table, reason] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(kinds_height),
            Constraint::Length(rest_height),
            Constraint::Min(if self.verbose { 3 } else { 0 }),
            Constraint::Length(if self.verbose { 3 } else { 0 }),
        ])
        .areas(area);
        frame.render_widget(Paragraph::new(vec![self.headline(tick), Line::from("")]), head);
        frame.render_widget(Table::new(rows, kind_widths).column_spacing(0), kinds_area);
        frame.render_widget(Paragraph::new(rest).wrap(Wrap { trim: false }), rest_area);
        if !self.verbose {
            return;
        }

        let rows = self.live.candidates.iter().map(|candidate| {
            let style = match candidate.status {
                Status::Serving => Style::new().fg(Color::Green),
                Status::Declined(_) => Style::new().fg(Color::Red),
                Status::Standby | Status::Probing => Style::new(),
                Status::NotAsked => dim(),
            };
            Row::new([
                Cell::from(candidate.name),
                Cell::from(Span::styled(candidate.status.word(), style)),
                Cell::from(candidate.status.reason().to_owned()),
            ])
        });
        let widths = [Constraint::Length(26), Constraint::Length(10), Constraint::Fill(1)];
        let table_widget = Table::new(rows, widths)
            .header(Row::new(["candidate", "status", "reason"]).style(dim()))
            .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(table_widget, table, &mut self.table);
        let selected = self.table.selected().and_then(|row| self.live.candidates.get(row));
        if let Some(candidate) = selected {
            let text = format!("{}: {} — {}", candidate.name, candidate.status.word(), candidate.status.reason());
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), reason);
        }
    }

    fn headline(&self, tick: usize) -> Line<'static> {
        let text = self.live.headline();
        if self.live.error.is_some() {
            return Line::from(Span::styled(text, Style::new().fg(Color::Red)));
        }
        if self.live.rate().is_none() {
            return Line::from(Span::styled(format!("{} {text}", SPINNER[tick % SPINNER.len()]), dim()));
        }
        Line::from(Span::styled(text, Style::new().add_modifier(Modifier::BOLD)))
    }
}

/// `text` broken into lines of at most `width` characters: at its own line
/// breaks, then after a comma or a space, and inside a word only when the
/// word alone is too long.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut current = String::new();
        for word in line.split_inclusive(' ') {
            if current.chars().count() + word.trim_end().chars().count() > width && !current.is_empty() {
                out.push(current.trim_end().to_owned());
                current.clear();
            }
            current.push_str(word);
            while current.trim_end().chars().count() > width {
                let head: String = current.chars().take(width).collect();
                current = current.chars().skip(width).collect();
                out.push(head);
            }
        }
        out.push(current.trim_end().to_owned());
    }
    out
}

fn colour(how: How) -> Color {
    match how {
        How::Live | How::Stated => Color::Green,
        How::Fallback => Color::Red,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::mpsc;

    use carbontracker::{Fidelity, PowerDomain, SampleKind};
    use carbontracker_providers::Verdict;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::screen::live::Msg;
    use crate::screen::live::tests::{em_key, forecast, intensity, power, readout};

    /// The screen's text, one line per row.
    pub(crate) fn text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let width = buffer.area.width as usize;
        buffer.content.chunks(width).map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>() + "\n").collect()
    }

    fn view(verbose: bool) -> (CheckView, mpsc::Sender<Msg>) {
        let (tx, rx) = mpsc::channel();
        (CheckView::new(Live::new(1.0), Feed::fake(rx), verbose), tx)
    }

    fn feed(tx: &mpsc::Sender<Msg>) {
        tx.send(Msg::Decided("nvml", Verdict::Serving(vec![]))).unwrap();
        tx.send(Msg::Decided("powermetrics", Verdict::Declined("macOS only; this host runs Linux".into()))).unwrap();
        tx.send(Msg::Decided("ip-geolocation", Verdict::Serving(vec![]))).unwrap();
        tx.send(Msg::Decided("electricity-maps", Verdict::Serving(vec![]))).unwrap();
        tx.send(Msg::Started {
            sources: vec![
                power(2, "nvml", PowerDomain::Gpu, SampleKind::Instant, Fidelity::Measured),
                intensity(3, "electricity-maps", Fidelity::Measured),
            ],
            forecast: Some(forecast()),
        })
        .unwrap();
        tx.send(readout(&[(2, 0, 100.0), (3, 0, 300.0)])).unwrap();
    }

    fn draw(view: &mut CheckView) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        view.pump();
        terminal.draw(|frame| view.draw(frame, frame.area(), 0)).unwrap();
        text(&terminal)
    }

    /// AC/FE-77
    #[test]
    fn the_view_shows_the_rate_and_each_kind_and_follows_readouts() {
        let (mut view, tx) = view(false);
        assert!(draw(&mut view).contains("probing…"));
        feed(&tx);
        let screen = draw(&mut view);
        assert!(screen.contains("Emitting 30.0 gCO2eq/h right now"), "{screen}");
        assert!(screen.contains("power     100.0 W"), "{screen}");
        assert!(screen.contains("intensity 300 gCO2eq/kWh"), "{screen}");
        assert!(screen.contains("gpu 100.0 W (nvml)"), "{screen}");
        assert!(screen.contains("DK-DK2 (electricity-maps)"), "{screen}");
        assert!(!screen.contains("powermetrics"), "candidates only with -v: {screen}");
        tx.send(readout(&[(3, 5, 100.0)])).unwrap();
        assert!(draw(&mut view).contains("Emitting 10.0 gCO2eq/h right now"));
    }

    /// AC/FE-77
    #[test]
    fn a_long_power_row_stays_in_its_column_on_a_narrow_screen() {
        let (mut view, tx) = view(false);
        let watts = |provider, by, domain| power(provider, by, domain, SampleKind::Instant, Fidelity::Measured);
        tx.send(Msg::Started {
            sources: vec![
                watts(1, "macmon", PowerDomain::System),
                watts(2, "ioreport", PowerDomain::Cpu),
                watts(3, "ioreport", PowerDomain::Gpu),
                watts(4, "ioreport", PowerDomain::Ane),
                watts(5, "ioreport", PowerDomain::Ram),
                intensity(6, "energinet", Fidelity::Measured),
            ],
            forecast: Some(forecast()),
        })
        .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(64, 20)).unwrap();
        view.pump();
        terminal.draw(|frame| view.draw(frame, frame.area(), 0)).unwrap();
        let screen = text(&terminal);
        let rows: Vec<&str> = screen.lines().collect();
        let power = rows.iter().position(|row| row.starts_with("power")).expect(&screen);
        let location = rows.iter().position(|row| row.starts_with("location")).expect(&screen);
        let start = |row: &str| row.chars().take_while(|c| *c == ' ').count();
        let column = rows[power].chars().collect::<String>().find("system").map(|byte| rows[power][..byte].chars().count()).expect(&screen);
        assert!(location > power + 2, "the ioreport parts take more than one line: {screen}");
        for row in &rows[power + 1..location] {
            assert_eq!(start(row), column, "every part starts in the column: {screen}");
        }
        assert!(screen.contains("ram"), "nothing is cut off: {screen}");
    }

    /// AC/FE-77
    #[test]
    fn q_esc_and_ctrl_c_close_the_view() {
        let (mut view, _tx) = view(false);
        assert!(view.on_key(KeyEvent::from(KeyCode::Char('q'))));
        assert!(view.on_key(KeyEvent::from(KeyCode::Esc)));
        assert!(view.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        assert!(!view.on_key(KeyEvent::from(KeyCode::Char('x'))));
    }

    /// AC/FE-79
    #[test]
    fn verbose_lists_every_candidate_with_the_selected_reason_in_full() {
        let (mut view, tx) = view(true);
        feed(&tx);
        draw(&mut view);
        view.on_key(KeyEvent::from(KeyCode::Down));
        let screen = draw(&mut view);
        assert!(screen.contains("candidate"), "{screen}");
        assert!(screen.contains("declined"), "{screen}");
        assert!(screen.contains("powermetrics: declined — macOS only; this host runs Linux"), "{screen}");
    }

    /// AC/FE-61, AC/FE-83
    #[test]
    fn without_verbose_a_declined_key_and_a_failed_read_still_show() {
        let (tx, rx) = mpsc::channel();
        let mut view = CheckView::new(Live::new(1.0).with_keys(vec![em_key()]), Feed::fake(rx), false);
        feed(&tx);
        // The key's candidate declined; a fallback serves instead.
        tx.send(Msg::Decided("electricity-maps", Verdict::Declined("zone DK-DK2: access denied: Electricity Maps refused the key (401 Unauthorized)".into()))).unwrap();
        let failed = |provider| {
            (
                carbontracker::SourceId::new(carbontracker::ProviderId(provider), 0),
                carbontracker::Observation::Failed(carbontracker::Failure::TimedOut),
            )
        };
        tx.send(Msg::Readout(std::sync::Arc::new(std::collections::HashMap::from([failed(2), failed(3)])))).unwrap();
        let screen = draw(&mut view);
        assert!(screen.contains("Electricity Maps refused key …1234 (401 Unauthorized)"), "{screen}");
        assert!(screen.contains("carbontracker init"), "{screen}");
        assert!(screen.contains("gpu (nvml): the read timed out"), "{screen}");
        assert!(screen.contains("(stale)"), "{screen}");
        let column = |label: &str| screen.lines().find(|line| line.starts_with(label)).unwrap().find(" live ");
        assert_eq!(column("power"), column("intensity"), "the stale value keeps the columns aligned: {screen}");
    }

    /// AC/FE-80
    #[test]
    fn closing_returns_what_the_view_last_showed() {
        let (mut view, tx) = view(false);
        feed(&tx);
        draw(&mut view);
        let plain = view.close();
        assert!(plain.starts_with("Emitting 30.0 gCO2eq/h right now"), "{plain}");
        assert!(plain.contains("  power      100.0 W"), "{plain}");
    }
}
