//! Terminal colour for the console's lines, painted from the same data the
//! plain text is written from; only a console on a terminal calls these, so
//! files and pipes stay plain.

use super::{How, PREFIX, Row, SourceLine, Summary, qualified};

fn paint(code: &str, text: &str) -> String {
    format!("\x1b[{code}m{text}\x1b[0m")
}

/// The console's prefix, dimmed.
pub fn prefix() -> String {
    format!("{} ", paint("2", PREFIX.trim_end()))
}

/// A warning in yellow, an error in red; any other line as it is.
pub fn line(text: &str) -> String {
    if text.starts_with("WARNING - ") {
        paint("33", text)
    } else if text.starts_with("ERROR - ") {
        paint("31", text)
    } else {
        text.to_owned()
    }
}

/// A setup line: its label dimmed, `live` and `stated` green, `fallback`
/// red, the provider's brackets dimmed.
pub fn source_line(line: &SourceLine) -> String {
    let kind = match line.kind {
        "" => " ".repeat(10),
        kind => paint("2", &format!("{kind:<10}")),
    };
    let (name, qualifier) = match &line.provider {
        Some(provider) => {
            let (name, qualifier) = qualified(provider);
            (Some(name), qualifier)
        }
        None => (None, None),
    };
    let mut what = line.what.clone();
    if let Some(qualifier) = qualifier {
        what = format!("{what} {qualifier}");
    }
    if let Some(name) = name {
        what = format!("{what} {}", paint("2", &format!("({name})")));
    }
    match line.how {
        Some(how) => {
            let word = how.word();
            let pad = " ".repeat(10 - word.len());
            format!("{kind} {}{pad} {what}", paint(how_colour(how), word))
        }
        None => format!("{kind} {what}"),
    }
}

/// The ANSI colour for how a value is obtained.
fn how_colour(how: How) -> &'static str {
    match how {
        How::Live | How::Stated => "32",
        How::Fallback => "31",
    }
}

/// The summary block: the title bold, labels dimmed, figures bold, notes
/// dimmed.
pub fn summary(block: &Summary) -> String {
    let mut out = paint("1", &block.title);
    for row in &block.rows {
        out.push_str("\n\t");
        match row {
            Row::Field { label, figure, note } => {
                let label = format!("{label}:");
                let gap = " ".repeat(12usize.saturating_sub(label.len()));
                out.push_str(&format!("{}{gap}{}", paint("2", &label), paint("1", figure)));
                if let Some(note) = note {
                    out.push_str(&format!(" {}", paint("2", note)));
                }
            }
            Row::Heading(text) => out.push_str(&paint("2", text)),
            Row::Text(text) => out.push_str(text),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dim(text: &str) -> String {
        format!("\x1b[2m{text}\x1b[0m")
    }

    fn bold(text: &str) -> String {
        format!("\x1b[1m{text}\x1b[0m")
    }

    fn colour(code: &str, text: &str) -> String {
        format!("\x1b[{code}m{text}\x1b[0m")
    }

    /// AC/OUT-38
    #[test]
    fn the_prefix_is_dimmed_and_warnings_and_errors_stand_out() {
        assert_eq!(prefix(), dim("CarbonTracker:") + " ");
        assert_eq!(line("WARNING - no samples"), colour("33", "WARNING - no samples"));
        assert_eq!(line("ERROR - it failed"), colour("31", "ERROR - it failed"));
        assert_eq!(line("ready. Starting sleep 2"), "ready. Starting sleep 2");
    }

    /// AC/OUT-38
    #[test]
    fn a_setup_line_dims_its_label_colours_how_and_dims_the_provider() {
        let line = |kind, how, what: &str, provider: Option<&str>| SourceLine {
            kind,
            how,
            what: what.to_owned(),
            provider: provider.map(str::to_owned),
        };
        assert_eq!(
            source_line(&line("power", Some(How::Live), "system", Some("macmon"))),
            format!("{} {}       system {}", dim("power     "), colour("32", "live"), dim("(macmon)"))
        );
        assert_eq!(
            source_line(&line("", Some(How::Fallback), "cpu", Some("generic-cpu"))),
            format!("           {}   cpu {}", colour("31", "fallback"), dim("(generic-cpu)"))
        );
        assert_eq!(
            source_line(&line("intensity", Some(How::Live), "GB", Some("neso (direct CO2)"))),
            format!("{} {}       GB direct CO2 {}", dim("intensity "), colour("32", "live"), dim("(neso)"))
        );
        assert_eq!(source_line(&line("power", None, "nothing found", None)), format!("{} nothing found", dim("power     ")));
    }

    /// AC/OUT-38
    #[test]
    fn the_summary_dims_labels_and_shows_its_figures() {
        let field = |label, figure: &str, note| Row::Field { label, figure: figure.to_owned(), note };
        let block = Summary {
            title: "sleep 2 summary:".to_owned(),
            rows: vec![
                field("Time", "0:00:02", None),
                field("Intensity", "105 gCO2eq/kWh", Some("(average)")),
                field("CO2eq", "0.00156 g", None),
                Row::Heading("This is equivalent to:"),
                Row::Text("0.0000146 km travelled by car".to_owned()),
                field("Logs", "./logs/run-1", None),
            ],
        };
        let expected = [
            bold("sleep 2 summary:"),
            format!("\t{}       {}", dim("Time:"), bold("0:00:02")),
            format!("\t{}  {} {}", dim("Intensity:"), bold("105 gCO2eq/kWh"), dim("(average)")),
            format!("\t{}      {}", dim("CO2eq:"), bold("0.00156 g")),
            format!("\t{}", dim("This is equivalent to:")),
            "\t0.0000146 km travelled by car".to_owned(),
            format!("\t{}       {}", dim("Logs:"), bold("./logs/run-1")),
        ]
        .join("\n");
        assert_eq!(summary(&block), expected);
        assert_eq!(
            block.plain(),
            "sleep 2 summary:\n\tTime:       0:00:02\n\tIntensity:  105 gCO2eq/kWh (average)\n\tCO2eq:      0.00156 g\n\
             \tThis is equivalent to:\n\t0.0000146 km travelled by car\n\tLogs:       ./logs/run-1"
        );
    }
}
