//! The sinks this crate ships, and [standard_sinks]: the three every
//! frontend writes to.

mod console;
mod jsonl;

use std::fs::File;
use std::io::{self, BufWriter, Write};

pub use console::ConsoleSink;
pub use jsonl::JsonlSink;

use crate::adapters::Folder;
use crate::domain::item::Layout;
use crate::ports::Sink;

/// Where and how a run is shown to people.
pub struct Console {
    pub out: Box<dyn Write + Send>,
    pub verbosity: Verbosity,
    pub layout: Layout,
}

/// How much the console shows. Each level includes the ones before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verbosity {
    /// Warnings and errors.
    Quiet,
    /// Also what is measured, and the closing block.
    Normal,
    /// Also each probe step and every engine event.
    Everything,
}

/// The console, and with a folder, `output.log` (the console at normal
/// verbosity, in the same layout) and the record.
pub fn standard_sinks(console: Console, folder: Option<&Folder>) -> io::Result<Vec<Box<dyn Sink>>> {
    let logs = folder.map(|folder| folder.path().display().to_string());
    let mut sinks: Vec<Box<dyn Sink>> = vec![Box::new(ConsoleSink::new(
        console.out,
        console.verbosity,
        console.layout.clone(),
        logs.clone(),
    ))];
    if let Some(folder) = folder {
        let output = BufWriter::new(File::create_new(folder.output_log())?);
        sinks.push(Box::new(ConsoleSink::new(
            output,
            Verbosity::Normal,
            console.layout,
            logs,
        )));
        sinks.push(Box::new(JsonlSink::create(folder.record())));
    }
    Ok(sinks)
}
