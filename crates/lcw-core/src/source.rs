//! A source file handed to a [`crate::adapter::LanguageAdapter`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// An in-memory source file: its path plus full text. Adapters parse a whole
/// batch at once so cross-file call resolution is possible.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub path: PathBuf,
    pub text: String,
}

impl SourceFile {
    pub fn new(path: impl Into<PathBuf>, text: impl Into<String>) -> Self {
        SourceFile {
            path: path.into(),
            text: text.into(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// One line of a [`SourceSnippet`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLine {
    pub number: u32,
    pub text: String,
}

/// A slice of source read from disk, with the node's span highlighted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSnippet {
    pub file: String,
    /// First line returned (1-based).
    pub start_line: u32,
    /// Last line returned (1-based, inclusive).
    pub end_line: u32,
    /// The node's span inside the returned slice (1-based, inclusive).
    pub highlight_start: u32,
    pub highlight_end: u32,
    pub lines: Vec<SourceLine>,
}

/// Files with at most this many lines are returned whole (no clipping).
pub const SNIPPET_WHOLE_FILE_MAX: u32 = 500;

/// Pick `[lo, hi]` (1-based inclusive) to return, always covering the full
/// highlight `[hl_start, hl_end]` when possible. Long functions are anchored at
/// `hl_start` so the signature and body opening stay visible.
fn snippet_window(
    file_len: usize,
    hl_start: usize,
    hl_end: usize,
    context: u32,
    max_lines: u32,
) -> (usize, usize) {
    if file_len == 0 {
        return (1, 0);
    }
    if file_len <= SNIPPET_WHOLE_FILE_MAX as usize {
        return (1, file_len);
    }

    let max = max_lines as usize;
    let mut lo = hl_start.saturating_sub(context as usize).max(1);
    let mut hi = (hl_end + context as usize).min(file_len);
    lo = lo.min(hl_start);
    hi = hi.max(hl_end);

    let span = hi - lo + 1;
    if span <= max {
        return (lo, hi);
    }

    let hl_len = hl_end - hl_start + 1;
    if hl_len > max {
        lo = hl_start;
        hi = (hl_start + max - 1).min(file_len);
        return (lo, hi);
    }

    let mut extra = span - max;
    let drop_before = extra.min(hl_start.saturating_sub(lo));
    lo += drop_before;
    extra -= drop_before;
    let drop_after = extra.min(hi.saturating_sub(hl_end));
    hi -= drop_after;
    (lo, hi)
}

/// Read lines from `path` around the node's `[start, end]` span (1-based,
/// inclusive). `context` pads before/after; the result is capped at `max_lines`
/// (the full highlight is kept; context is trimmed, then long spans anchor at
/// `start`). Files with at most [`SNIPPET_WHOLE_FILE_MAX`] lines are returned
/// whole. Returns `None` when the file is missing or the range is invalid.
pub fn read_snippet(
    path: &str,
    start: u32,
    end: u32,
    context: u32,
    max_lines: u32,
) -> Option<SourceSnippet> {
    if path.is_empty() || start == 0 {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let file_lines: Vec<&str> = text.lines().collect();
    if file_lines.is_empty() {
        return None;
    }

    let hl_start = (start as usize).min(file_lines.len()).max(1);
    let hl_end_line = if end >= start && end > 0 {
        end
    } else {
        start
    };
    let hl_end = (hl_end_line as usize).min(file_lines.len()).max(hl_start);

    let (lo, hi) = snippet_window(file_lines.len(), hl_start, hl_end, context, max_lines);

    let lines = file_lines[lo - 1..hi]
        .iter()
        .enumerate()
        .map(|(i, &text)| SourceLine {
            number: lo as u32 + i as u32,
            text: text.to_string(),
        })
        .collect();

    Some(SourceSnippet {
        file: path.to_string(),
        start_line: lo as u32,
        end_line: hi as u32,
        highlight_start: hl_start as u32,
        highlight_end: hl_end as u32,
        lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_lines(path: &std::path::Path, n: usize) {
        let mut f = std::fs::File::create(path).unwrap();
        for i in 1..=n {
            writeln!(f, "// line {i}").unwrap();
        }
    }

    #[test]
    fn read_snippet_returns_highlighted_range() {
        let dir = std::env::temp_dir().join("lcw_snippet_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.rs");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "// header").unwrap();
            writeln!(f, "fn one() {{}}").unwrap();
            writeln!(f, "fn two() {{").unwrap();
            writeln!(f, "    println!(\"hi\");").unwrap();
            writeln!(f, "}}").unwrap();
            writeln!(f, "// footer").unwrap();
        }

        let snip = read_snippet(path.to_str().unwrap(), 3, 5, 1, 80).unwrap();
        assert_eq!(snip.highlight_start, 3);
        assert_eq!(snip.highlight_end, 5);
        assert_eq!(snip.start_line, 1);
        assert_eq!(snip.end_line, 6);
        let two = snip.lines.iter().find(|l| l.number == 3).unwrap();
        assert_eq!(two.text, "fn two() {");
    }

    #[test]
    fn read_snippet_caps_long_files_at_function_start() {
        let dir = std::env::temp_dir().join("lcw_snippet_cap");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("long.rs");
        write_lines(&path, 600);

        let snip = read_snippet(path.to_str().unwrap(), 100, 150, 5, 20).unwrap();
        assert_eq!(snip.lines.len(), 20);
        assert_eq!(snip.highlight_start, 100);
        assert_eq!(snip.highlight_end, 150);
        assert_eq!(snip.start_line, 100);
        assert_eq!(snip.end_line, 119);
    }

    #[test]
    fn read_snippet_returns_whole_small_file() {
        let dir = std::env::temp_dir().join("lcw_snippet_whole");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("small.rs");
        write_lines(&path, 40);

        let snip = read_snippet(path.to_str().unwrap(), 10, 12, 0, 15).unwrap();
        assert_eq!(snip.start_line, 1);
        assert_eq!(snip.end_line, 40);
        assert_eq!(snip.lines.len(), 40);
        assert_eq!(snip.highlight_start, 10);
        assert_eq!(snip.highlight_end, 12);
    }

    #[test]
    fn read_snippet_keeps_full_highlight_when_context_trimmed() {
        let dir = std::env::temp_dir().join("lcw_snippet_hl");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fn_body.rs");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            for i in 1..=520 {
                writeln!(f, "// pad {i}").unwrap();
            }
            writeln!(f, "fn big() {{").unwrap();
            for i in 1..=30 {
                writeln!(f, "    let _ = {i};").unwrap();
            }
            writeln!(f, "}}").unwrap();
        }

        let start = 521_u32;
        let end = 552_u32;
        let snip = read_snippet(path.to_str().unwrap(), start, end, 5, 40).unwrap();
        assert_eq!(snip.highlight_start, start);
        assert_eq!(snip.highlight_end, end);
        assert!(snip.start_line <= start);
        assert!(snip.end_line >= end);
        assert!(snip.lines.len() <= 40);
        assert_eq!(
            snip.end_line - snip.start_line + 1,
            snip.lines.len() as u32
        );
    }
}
