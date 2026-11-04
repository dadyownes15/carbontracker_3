//! The record format: versioned JSON lines, one [Record] per line, starting
//! with a [Header]. The types are independent of the engine's.
//!
//! # Versioning
//!
//! - Readers accept every format up to [FORMAT] and reject newer ones.
//! - Within a format, only new kinds and fields may be added; readers skip
//!   unknown ones ([Line::Unknown]).
//! - Removing or renaming a field, or changing its unit or meaning, requires
//!   a new format.
//! - Each format has fixtures in `tests/fixtures/records/v<N>/` that later
//!   releases must still read.

mod v1;

pub use v1::*;

/// The format version written, and the newest one read.
pub const FORMAT: u32 = 1;

/// A record's text as its lines, header first.
///
/// A last line without its newline was cut off by a killed writer and is
/// dropped. Fails on a newer format, a line that does not decode, or a
/// missing header, naming which.
pub fn read(text: &str) -> Result<Vec<Record>, String> {
    let mut lines: Vec<&str> = text.split_inclusive('\n').collect();
    if lines.last().is_some_and(|line| !line.ends_with('\n')) {
        lines.pop();
    }
    let first = lines.first().ok_or("no run header")?;
    if let Ok(header) = serde_json::from_str::<serde_json::Value>(first)
        && let Some(format) = header.get("format").and_then(serde_json::Value::as_u64)
        && format > u64::from(FORMAT)
    {
        return Err(format!("written in format {format}; this release reads up to format {FORMAT}"));
    }
    let records = lines
        .iter()
        .enumerate()
        .map(|(index, line)| serde_json::from_str(line).map_err(|error| format!("line {}: {error}", index + 1)))
        .collect::<Result<Vec<Record>, _>>()?;
    match records.first() {
        Some(Record { line: Line::Run(_), .. }) => Ok(records),
        _ => Err("no run header".to_owned()),
    }
}
