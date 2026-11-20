//! The full-screen `init`: a menu, the key typed masked and checked before
//! it is saved, a Slurm script picked and its wrapper shown before it is
//! written, and the check view.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use carbontracker_providers::config_file as config;
use carbontracker_providers::{ApiKey, KeyCheck, KeyedService};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Padding, Paragraph, Wrap};
use ratatui::Frame;

use super::check_view::CheckView;
use super::{SPINNER, dim};
use crate::commands::slurm::{self, Prepared};

/// What the app relies on, swapped in tests.
pub struct Deps<'a> {
    pub config: PathBuf,
    pub check_key: Arc<dyn Fn(&ApiKey) -> KeyCheck + Send + Sync>,
    pub open_check: Box<dyn FnMut() -> Result<CheckView, String> + 'a>,
    /// Where Slurm scripts are looked for.
    pub here: PathBuf,
    /// The log directory the wrapper passes on; `None` for `--no-log`.
    pub log_dir: Option<PathBuf>,
}

enum Screen {
    Menu,
    Key { error: Option<String> },
    Checking,
    Unreachable { why: String },
    Pick,
    Path { error: Option<String> },
    Preview(Prepared),
    Done(String),
    Check(Box<CheckView>),
    Failed(String),
}

pub struct App<'a> {
    deps: Deps<'a>,
    screen: Screen,
    input: String,
    menu: ListState,
    scripts: Vec<PathBuf>,
    pick: ListState,
    tx: Sender<KeyCheck>,
    rx: Receiver<KeyCheck>,
    /// What was done, printed once the screen is gone.
    pub log: Vec<String>,
    pub quit: bool,
}

const ITEMS: usize = 4;

impl<'a> App<'a> {
    pub fn new(deps: Deps<'a>) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            deps,
            screen: Screen::Menu,
            input: String::new(),
            menu: ListState::default().with_selected(Some(0)),
            scripts: Vec::new(),
            pick: ListState::default().with_selected(Some(0)),
            tx,
            rx,
            log: Vec::new(),
            quit: false,
        }
    }

    fn saved_hint(&self) -> Option<String> {
        let keys = config::load(&self.deps.config).ok()?;
        keys.get(&KeyedService::ElectricityMaps).map(|key| key.hint().to_string())
    }

    /// Takes a finished key check and any readouts for the check view.
    pub fn pump(&mut self) {
        if let Ok(result) = self.rx.try_recv() {
            self.key_checked(result);
        }
        if let Screen::Check(view) = &mut self.screen {
            view.pump();
        }
    }

    /// Waits for the key check in flight; for tests.
    #[cfg(test)]
    fn wait(&mut self) {
        let result = self.rx.recv().unwrap();
        self.key_checked(result);
    }

    fn key_checked(&mut self, result: KeyCheck) {
        let key = self.input.trim().to_owned();
        self.screen = match result {
            KeyCheck::Works(zones) => match self.save(&key) {
                Ok(()) => {
                    self.log.push(format!("Key works: {}. Saved to {}", zone_summary(&zones), self.deps.config.display()));
                    self.input.clear();
                    self.open_check()
                }
                Err(error) => Screen::Failed(error),
            },
            KeyCheck::Refused(why) => Screen::Key {
                error: Some(format!("{why}; nothing saved. Edit the key and press Enter, or Esc to go back.")),
            },
            KeyCheck::Unreachable(why) => Screen::Unreachable { why },
        };
    }

    fn save(&self, key: &str) -> Result<(), String> {
        config::save(&self.deps.config, KeyedService::ElectricityMaps, key)
    }

    fn open_check(&mut self) -> Screen {
        match (self.deps.open_check)() {
            Ok(view) => Screen::Check(Box::new(view)),
            Err(error) => Screen::Failed(error),
        }
    }

    fn start_key_check(&mut self) -> Screen {
        let check = self.deps.check_key.clone();
        let tx = self.tx.clone();
        let key = ApiKey::new(self.input.trim());
        std::thread::spawn(move || {
            let _ = tx.send(check(&key));
        });
        Screen::Checking
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        let ctrl_c = key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        if let Screen::Check(view) = &mut self.screen {
            if view.on_key(key) {
                let Screen::Check(view) = std::mem::replace(&mut self.screen, Screen::Menu) else { unreachable!() };
                self.log.push(view.close());
            }
            return;
        }
        if ctrl_c {
            self.quit = true;
            return;
        }
        let screen = std::mem::replace(&mut self.screen, Screen::Menu);
        self.screen = self.next(screen, key.code);
    }

    fn next(&mut self, screen: Screen, code: KeyCode) -> Screen {
        match screen {
            Screen::Menu => match code {
                KeyCode::Up => {
                    self.menu.select_previous();
                    Screen::Menu
                }
                KeyCode::Down => {
                    let next = self.menu.selected().map_or(0, |item| (item + 1).min(ITEMS - 1));
                    self.menu.select(Some(next));
                    Screen::Menu
                }
                KeyCode::Enter => self.choose(self.menu.selected().unwrap_or(0)),
                KeyCode::Char(digit @ '1'..='4') => self.choose(digit as usize - '1' as usize),
                KeyCode::Char('q') | KeyCode::Esc => {
                    self.quit = true;
                    Screen::Menu
                }
                _ => Screen::Menu,
            },
            Screen::Key { error } => match code {
                KeyCode::Esc => Screen::Menu,
                KeyCode::Enter if self.input.trim().is_empty() => {
                    Screen::Key { error: Some("Type a key, or Esc to go back.".into()) }
                }
                KeyCode::Enter => self.start_key_check(),
                code => {
                    edit(&mut self.input, code);
                    Screen::Key { error }
                }
            },
            Screen::Checking => Screen::Checking,
            Screen::Unreachable { why } => match code {
                KeyCode::Char('s') => {
                    let key = self.input.trim().to_owned();
                    match self.save(&key) {
                        Ok(()) => {
                            self.log.push(format!("! could not check the key ({why}); saved unchecked to {}", self.deps.config.display()));
                            self.input.clear();
                            self.open_check()
                        }
                        Err(error) => Screen::Failed(error),
                    }
                }
                KeyCode::Enter => self.start_key_check(),
                KeyCode::Esc => {
                    self.input.clear();
                    Screen::Menu
                }
                _ => Screen::Unreachable { why },
            },
            Screen::Pick => match code {
                KeyCode::Up => {
                    self.pick.select_previous();
                    Screen::Pick
                }
                KeyCode::Down => {
                    let next = self.pick.selected().map_or(0, |item| (item + 1).min(self.scripts.len()));
                    self.pick.select(Some(next));
                    Screen::Pick
                }
                KeyCode::Enter => match self.scripts.get(self.pick.selected().unwrap_or(0)).cloned() {
                    Some(script) => self.preview(&script),
                    None => {
                        self.input.clear();
                        Screen::Path { error: None }
                    }
                },
                KeyCode::Esc => Screen::Menu,
                _ => Screen::Pick,
            },
            Screen::Path { error } => match code {
                KeyCode::Esc => Screen::Menu,
                KeyCode::Enter => {
                    let path = self.deps.here.join(self.input.trim());
                    match slurm::prepare(self.deps.log_dir.as_deref(), &path) {
                        Ok(mut prepared) => {
                            prepared.shown = slurm_shown(&prepared.target, &self.deps.here);
                            Screen::Preview(prepared)
                        }
                        Err(error) => Screen::Path { error: Some(error) },
                    }
                }
                code => {
                    edit(&mut self.input, code);
                    Screen::Path { error }
                }
            },
            Screen::Preview(prepared) => match code {
                KeyCode::Enter => match slurm::write(&prepared) {
                    Ok(message) => {
                        self.log.push(message.clone());
                        Screen::Done(message)
                    }
                    Err(error) => Screen::Failed(error),
                },
                KeyCode::Esc => Screen::Menu,
                _ => Screen::Preview(prepared),
            },
            Screen::Done(_) | Screen::Failed(_) => Screen::Menu,
            Screen::Check(view) => Screen::Check(view),
        }
    }

    fn choose(&mut self, item: usize) -> Screen {
        self.menu.select(Some(item));
        match item {
            0 => {
                self.input.clear();
                Screen::Key { error: None }
            }
            1 => {
                self.scripts = slurm::find(&self.deps.here);
                self.pick.select(Some(0));
                if self.scripts.is_empty() {
                    self.input.clear();
                    let error = format!("No Slurm scripts found under {}. Type a path instead.", self.deps.here.display());
                    Screen::Path { error: Some(error) }
                } else {
                    Screen::Pick
                }
            }
            2 => self.open_check(),
            _ => {
                self.quit = true;
                Screen::Menu
            }
        }
    }

    fn preview(&mut self, script: &Path) -> Screen {
        match slurm::prepare(self.deps.log_dir.as_deref(), script) {
            Ok(mut prepared) => {
                prepared.shown = slurm_shown(&prepared.target, &self.deps.here);
                Screen::Preview(prepared)
            }
            Err(error) => Screen::Failed(error),
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, tick: usize) {
        let area = frame.area();
        let title = Line::from(vec![
            Span::styled("carbontracker ", Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled("init", Style::new().add_modifier(Modifier::BOLD)),
        ]);
        let status = match self.saved_hint() {
            Some(hint) => format!("Electricity Maps key {hint} saved"),
            None => "No Electricity Maps key saved".to_owned(),
        };
        let [head, body, foot] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(5), Constraint::Length(1)]).areas(area);
        frame.render_widget(Paragraph::new(vec![title, Line::from(Span::styled(status, dim()))]), head);

        let (heading, keys) = self.labels();
        let block = Block::default().borders(Borders::ALL).title(heading).padding(Padding::horizontal(1));
        let inner = block.inner(body);
        frame.render_widget(block, body);
        frame.render_widget(Paragraph::new(Span::styled(keys, dim())), foot);

        let hint = self.saved_hint();
        match &mut self.screen {
            Screen::Menu => {
                let key_item = match hint {
                    Some(hint) => format!("Change Electricity Maps API key (now {hint})"),
                    None => "Add Electricity Maps API key".to_owned(),
                };
                let items = [key_item, "Attach carbontracker to a Slurm script".into(), "Check what would be measured".into(), "Quit".into()];
                let items = items.iter().enumerate().map(|(i, item)| ListItem::new(format!("{}) {item}", i + 1)));
                let list = List::new(items)
                    .highlight_symbol("❯ ")
                    .highlight_style(Style::new().fg(Color::Green).add_modifier(Modifier::BOLD));
                frame.render_stateful_widget(list, inner, &mut self.menu);
            }
            Screen::Key { error } => {
                let mut lines = vec![
                    Line::from("Paste or type your key. It is checked with Electricity Maps before it is saved."),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("› ", Style::new().fg(Color::Green)),
                        Span::raw("•".repeat(self.input.chars().count())),
                    ]),
                ];
                if let Some(error) = error {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(error.clone(), Style::new().fg(Color::Red))));
                }
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("Find it at app.electricitymaps.com, under API keys.", dim())));
                frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            }
            Screen::Checking => {
                let text = format!("{} Checking the key with Electricity Maps…", SPINNER[tick % SPINNER.len()]);
                frame.render_widget(Paragraph::new(text), inner);
            }
            Screen::Unreachable { why } => {
                let lines = vec![
                    Line::from(Span::styled(format!("Could not check the key ({why})."), Style::new().fg(Color::Yellow))),
                    Line::from("The key may still be fine; it can be saved without the check."),
                ];
                frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            }
            Screen::Pick => {
                let here = &self.deps.here;
                let mut items: Vec<ListItem> =
                    self.scripts.iter().map(|script| ListItem::new(slurm::describe(script, here))).collect();
                items.push(ListItem::new(Span::styled("Type a path…", Style::new().add_modifier(Modifier::ITALIC))));
                let list = List::new(items)
                    .highlight_symbol("❯ ")
                    .highlight_style(Style::new().fg(Color::Green).add_modifier(Modifier::BOLD));
                frame.render_stateful_widget(list, inner, &mut self.pick);
            }
            Screen::Path { error } => {
                let mut lines = vec![
                    Line::from("Path to the Slurm script:"),
                    Line::from(vec![Span::styled("› ", Style::new().fg(Color::Green)), Span::raw(self.input.clone())]),
                ];
                if let Some(error) = error {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(error.clone(), Style::new().fg(Color::Red))));
                }
                frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            }
            Screen::Preview(prepared) => {
                let mut lines: Vec<Line> = prepared
                    .warnings
                    .iter()
                    .map(|warning| Line::from(Span::styled(format!("! {warning}"), Style::new().fg(Color::Yellow))))
                    .collect();
                if prepared.replacing {
                    lines.push(Line::from(Span::styled("Replaces the wrapper written before.", Style::new().fg(Color::Yellow))));
                }
                if !lines.is_empty() {
                    lines.push(Line::from(""));
                }
                lines.extend(prepared.text.lines().map(|line| Line::from(Span::styled(line.to_owned(), dim()))));
                frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            }
            Screen::Done(message) => {
                let lines: Vec<Line> = message.lines().map(|line| Line::from(line.to_owned())).collect();
                frame.render_widget(Paragraph::new(lines), inner);
            }
            Screen::Failed(error) => {
                frame.render_widget(
                    Paragraph::new(Span::styled(error.clone(), Style::new().fg(Color::Red))).wrap(Wrap { trim: false }),
                    inner,
                );
            }
            Screen::Check(view) => view.draw(frame, inner, tick),
        }
    }

    fn labels(&self) -> (String, &'static str) {
        match &self.screen {
            Screen::Menu => (" What would you like to do? ".into(), "1–4 or ↑↓ Enter choose · q quit"),
            Screen::Key { .. } => (" Electricity Maps API key ".into(), "Enter check and save · Esc back"),
            Screen::Checking => (" Electricity Maps API key ".into(), "Ctrl-C quit"),
            Screen::Unreachable { .. } => {
                (" Electricity Maps API key ".into(), "s save unchecked · Enter try again · Esc back")
            }
            Screen::Pick => (format!(" Which Slurm script? ({} found) ", self.scripts.len()), "↑↓ move · Enter choose · Esc back"),
            Screen::Path { .. } => (" Slurm script ".into(), "Enter preview · Esc back"),
            Screen::Preview(prepared) => (format!(" Will write {} ", prepared.shown.display()), "Enter write · Esc back"),
            Screen::Done(_) => (" Done ".into(), "any key: back to the menu"),
            Screen::Check(_) => (" carbontracker check ".into(), "q or Esc back to the menu"),
            Screen::Failed(_) => (" Error ".into(), "any key: back to the menu"),
        }
    }
}

/// `target` relative to `here` when it is under it.
fn slurm_shown(target: &Path, here: &Path) -> PathBuf {
    let here = here.canonicalize().unwrap_or_else(|_| here.to_owned());
    target.strip_prefix(&here).map(Path::to_owned).unwrap_or_else(|_| target.to_owned())
}

fn edit(input: &mut String, code: KeyCode) {
    match code {
        KeyCode::Char(c) => input.push(c),
        KeyCode::Backspace => {
            input.pop();
        }
        _ => {}
    }
}

/// `2 zones (DK-DK1, DK-DK2)`, naming at most five.
pub fn zone_summary(zones: &[String]) -> String {
    const SHOWN: usize = 5;
    if zones.is_empty() {
        return "no zones".to_owned();
    }
    let noun = if zones.len() == 1 { "zone" } else { "zones" };
    let mut list = zones.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(", ");
    if zones.len() > SHOWN {
        list.push_str(&format!(", and {} more", zones.len() - SHOWN));
    }
    format!("{} {noun} ({list})", zones.len())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::screen::check_view::tests::text;
    use crate::screen::live::Live;
    use crate::screen::run::Feed;

    const SCRIPT: &str = "#!/bin/bash\n#SBATCH --job-name=train\n#SBATCH --partition=gpu\npython train.py\n";

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        app: App<'static>,
    }

    fn fixture(answers: Vec<KeyCheck>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let answers = Mutex::new(VecDeque::from(answers));
        let deps = Deps {
            config: root.join("config/carbontracker/config.toml"),
            check_key: Arc::new(move |_: &ApiKey| answers.lock().unwrap().pop_front().expect("one key check too many")),
            open_check: Box::new(|| {
                let (_tx, rx) = mpsc::channel();
                Ok(CheckView::new(Live::new(1.0), Feed::fake(rx), false))
            }),
            here: root.clone(),
            log_dir: Some(PathBuf::from("runs")),
        };
        Fixture { _dir: dir, root, app: App::new(deps) }
    }

    fn press(app: &mut App, keys: &str) {
        for c in keys.chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
    }

    fn key(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::from(code));
    }

    fn screen(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(110, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame, 0)).unwrap();
        text(&terminal)
    }

    fn saved(f: &Fixture) -> Option<String> {
        let text = std::fs::read_to_string(&f.app.deps.config).ok()?;
        Some(text.lines().find_map(|line| line.strip_prefix("electricity_maps = "))?.trim_matches('"').to_owned())
    }

    fn checking(app: &App) -> bool {
        matches!(app.screen, Screen::Checking)
    }

    fn on_check(app: &App) -> bool {
        matches!(app.screen, Screen::Check(_))
    }

    /// AC/FE-65, AC/FE-82, AC/FE-9
    #[test]
    fn the_menu_offers_each_action_and_a_saved_key_opens_check_then_comes_back() {
        let mut f = fixture(vec![KeyCheck::Works(vec!["DK-DK1".into()])]);
        let menu = screen(&mut f.app);
        for item in ["1) Add Electricity Maps API key", "2) Attach carbontracker to a Slurm script", "3) Check what would be measured", "4) Quit"] {
            assert!(menu.contains(item), "{item}: {menu}");
        }
        press(&mut f.app, "1");
        press(&mut f.app, "secret-c3d4");
        let typing = screen(&mut f.app);
        assert!(typing.contains("•••••••••••"), "masked: {typing}");
        assert!(!typing.contains("secret"), "{typing}");
        key(&mut f.app, KeyCode::Enter);
        assert!(checking(&f.app));
        f.app.wait();
        assert_eq!(saved(&f).as_deref(), Some("secret-c3d4"));
        assert!(on_check(&f.app), "check opens after saving");
        key(&mut f.app, KeyCode::Char('q'));
        assert!(screen(&mut f.app).contains("1) Change Electricity Maps API key (now …c3d4)"));
        press(&mut f.app, "3");
        assert!(on_check(&f.app));
        key(&mut f.app, KeyCode::Esc);
        press(&mut f.app, "4");
        assert!(f.app.quit);
        let log = f.app.log.join("\n");
        assert!(log.contains("Key works: 1 zone (DK-DK1). Saved to"), "{log}");
        assert!(log.contains("Probing this machine"), "the check view's snapshot: {log}");
        assert!(!log.contains("secret"));
    }

    /// AC/FE-67
    #[test]
    fn a_refused_key_is_asked_for_again_and_never_saved() {
        let refused = KeyCheck::Refused("Electricity Maps refused the key (401 Unauthorized)".into());
        let mut f = fixture(vec![refused, KeyCheck::Works(vec![])]);
        press(&mut f.app, "1");
        press(&mut f.app, "bad");
        key(&mut f.app, KeyCode::Enter);
        f.app.wait();
        assert!(screen(&mut f.app).contains("refused the key (401 Unauthorized); nothing saved"));
        assert_eq!(saved(&f), None);
        key(&mut f.app, KeyCode::Backspace);
        press(&mut f.app, "d-key");
        key(&mut f.app, KeyCode::Enter);
        f.app.wait();
        assert_eq!(saved(&f).as_deref(), Some("bad-key"));
    }

    /// AC/FE-68
    #[test]
    fn an_unreachable_api_offers_to_try_again_or_save_unchecked() {
        let down = || KeyCheck::Unreachable("unavailable: Electricity Maps: timed out".into());
        let mut f = fixture(vec![down(), down()]);
        press(&mut f.app, "1");
        press(&mut f.app, "key-2222");
        key(&mut f.app, KeyCode::Enter);
        f.app.wait();
        assert!(screen(&mut f.app).contains("Could not check the key (unavailable: Electricity Maps: timed out)"));
        key(&mut f.app, KeyCode::Enter);
        f.app.wait();
        assert_eq!(saved(&f), None, "trying again saves nothing");
        press(&mut f.app, "s");
        assert_eq!(saved(&f).as_deref(), Some("key-2222"));
        assert!(f.app.log.join("\n").contains("saved unchecked"));
        assert!(on_check(&f.app));
    }

    /// AC/FE-82, AC/FE-75
    #[test]
    fn a_picked_slurm_script_is_previewed_then_written() {
        let mut f = fixture(vec![]);
        std::fs::create_dir(f.root.join("jobs")).unwrap();
        std::fs::write(f.root.join("jobs/train.sh"), SCRIPT).unwrap();
        press(&mut f.app, "2");
        let pick = screen(&mut f.app);
        assert!(pick.contains("jobs/train.sh"), "{pick}");
        assert!(pick.contains("Type a path…"), "{pick}");
        key(&mut f.app, KeyCode::Enter);
        let preview = screen(&mut f.app);
        assert!(preview.contains("Will write jobs/train.ct.sh"), "{preview}");
        assert!(preview.contains("#SBATCH --job-name=train"), "{preview}");
        assert!(preview.contains("--log-dir runs"), "{preview}");
        assert!(!f.root.join("jobs/train.ct.sh").exists(), "nothing written before Enter");
        key(&mut f.app, KeyCode::Enter);
        assert!(f.root.join("jobs/train.ct.sh").exists());
        assert!(screen(&mut f.app).contains("sbatch jobs/train.ct.sh"));
        assert!(f.app.log.join("\n").contains("Wrote jobs/train.ct.sh"));
    }

    /// AC/FE-82
    #[test]
    fn a_typed_path_is_wrapped_and_a_bad_one_is_named() {
        let mut f = fixture(vec![]);
        press(&mut f.app, "2");
        let empty = screen(&mut f.app);
        assert!(empty.contains("No Slurm scripts found"), "{empty}");
        press(&mut f.app, "nope.sh");
        key(&mut f.app, KeyCode::Enter);
        assert!(screen(&mut f.app).contains("cannot read"));
        std::fs::write(f.root.join("eval.sh"), SCRIPT).unwrap();
        for _ in 0.."nope.sh".len() {
            key(&mut f.app, KeyCode::Backspace);
        }
        press(&mut f.app, "eval.sh");
        key(&mut f.app, KeyCode::Enter);
        key(&mut f.app, KeyCode::Enter);
        assert!(f.root.join("eval.ct.sh").exists());
    }

    #[test]
    fn many_zones_are_summarised() {
        let zones: Vec<String> = ["DE", "DK-DK1", "DK-DK2", "FR", "GB", "NL", "SE-SE3"].map(String::from).to_vec();
        assert_eq!(zone_summary(&zones), "7 zones (DE, DK-DK1, DK-DK2, FR, GB, and 2 more)");
    }
}
