//! The full-screen `check` and `init`, on a terminal. Piped, the commands
//! print plain text instead and never come here.

pub mod check_view;
pub mod init_app;
pub mod live;
pub mod run;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use carbontracker_providers::check_key;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::style::{Modifier, Style};
use ratatui::DefaultTerminal;

use crate::args::Args;
use check_view::CheckView;
use init_app::{App, Deps};
use crate::commands::candidate_of;
use live::{KeyNote, Live};

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// How long a frame waits for a key before drawing again.
const FRAME: Duration = Duration::from_millis(100);

pub fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// Whether both stdin and stdout are terminals, so a full screen can open.
pub fn available() -> bool {
    use std::io::IsTerminal;
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

fn open_check(args: &Args) -> Result<CheckView, String> {
    let keys = args
        .keys()?
        .iter()
        .map(|(service, (key, _))| KeyNote {
            service: service.name(),
            hint: key.hint().to_string(),
            candidate: candidate_of(*service),
        })
        .collect();
    let feed = run::start(args)?;
    Ok(CheckView::new(Live::new(args.pue()).with_keys(keys), feed, args.verbose))
}

/// The check view until the user closes it; then what it showed, on stdout.
pub fn check(args: &Args) -> Result<i32, String> {
    let mut view = open_check(args)?;
    let mut terminal = ratatui::init();
    let result = check_loop(&mut terminal, &mut view);
    ratatui::restore();
    result.map_err(|error| format!("the terminal failed: {error}"))?;
    print!("{}", view.close());
    Ok(0)
}

fn check_loop(terminal: &mut DefaultTerminal, view: &mut CheckView) -> io::Result<()> {
    let mut tick = 0usize;
    loop {
        view.pump();
        terminal.draw(|frame| view.draw(frame, frame.area(), tick))?;
        if event::poll(FRAME)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && view.on_key(key)
        {
            return Ok(());
        }
        tick = tick.wrapping_add(1);
    }
}

/// The init app until the user quits; then what it did, on stdout.
pub fn init(args: &Args, config: PathBuf) -> Result<i32, String> {
    let here = std::env::current_dir().map_err(|error| format!("cannot read the current folder: {error}"))?;
    let deps = Deps {
        config,
        check_key: Arc::new(check_key),
        open_check: Box::new(|| open_check(args)),
        here,
        log_dir: args.log_dir().map(|dir| dir.to_owned()),
    };
    let mut app = App::new(deps);
    let mut terminal = ratatui::init();
    let result = init_loop(&mut terminal, &mut app);
    ratatui::restore();
    for line in &app.log {
        println!("{}", line.trim_end());
    }
    result.map_err(|error| format!("the terminal failed: {error}"))?;
    Ok(0)
}

fn init_loop(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    let mut tick = 0usize;
    while !app.quit {
        app.pump();
        terminal.draw(|frame| app.draw(frame, tick))?;
        if event::poll(FRAME)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.on_key(key);
        }
        tick = tick.wrapping_add(1);
    }
    Ok(())
}
