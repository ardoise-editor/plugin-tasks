//! The problems an output says: an error or a warning at a place.

use ardoise_plugin::lint::{Diagnostic, Severity};

use crate::engine::Location;

/// The problem a line of output says, if it says one. `before` is the line
/// before it: the compilers that give the place on a line of its own
/// (`--> src/a.rs:3:4`) said what is wrong just before.
pub fn of_line(line: &str, before: Option<&str>, place: &Location) -> Option<Diagnostic> {
    let severity_of = |text: &str| {
        let t = text.trim_start().to_lowercase();
        if t.starts_with("error") || t.contains(": error") || t.contains(" error ") {
            Some(Severity::Error)
        } else if t.starts_with("warning") || t.contains(": warning") {
            Some(Severity::Warning)
        } else {
            None
        }
    };
    let (text, severity) = if line.trim_start().starts_with("-->") {
        let b = before?;
        (b.to_string(), severity_of(b)?)
    } else {
        (line.to_string(), severity_of(line)?)
    };
    let message = text
        .trim()
        .trim_start_matches("error")
        .trim_start_matches("warning")
        .trim_start_matches([':', ' '])
        .to_string();
    let (row, col) = (place.line.saturating_sub(1), place.col.saturating_sub(1));
    Some(Diagnostic {
        line: row,
        col,
        end_line: row,
        end_col: col + 1,
        severity,
        message: if message.is_empty() { text } else { message },
        code: None,
        source: "Task".to_string(),
        fixes: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn at(line: usize, col: usize) -> Location {
        Location {
            path: PathBuf::from("/p/a.rs"),
            line,
            col,
        }
    }

    #[test]
    fn errors_and_warnings_become_problems_where_the_output_says() {
        let d = of_line("src/a.rs:3:5: error: bad thing", None, &at(3, 5)).unwrap();
        assert_eq!((d.line, d.col, d.severity), (2, 4, Severity::Error));
        // Rust: the message is the line before the place.
        let d = of_line("  --> src/a.rs:7:1", Some("warning: unused"), &at(7, 1)).unwrap();
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.message, "unused");
        assert!(of_line("src/a.rs:3: just a note", None, &at(3, 1)).is_none());
    }
}
