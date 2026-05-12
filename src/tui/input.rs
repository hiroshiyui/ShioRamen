// SPDX-License-Identifier: GPL-3.0-or-later

pub(super) fn split_path(path: &str) -> (String, String) {
    match path.rfind('/') {
        Some(p) => (path[..=p].to_string(), path[p + 1..].to_string()),
        None => (String::new(), path.to_string()),
    }
}

/// Returns the byte offset of the start of each line (split by `\n`).
pub(super) fn line_starts(s: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in s.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// Returns `(line_index, byte_column_within_line)` for the given cursor byte offset.
pub(super) fn cursor_line_col(s: &str, cursor: usize) -> (usize, usize) {
    let starts = line_starts(s);
    let line = starts.partition_point(|&st| st <= cursor).saturating_sub(1);
    (line, cursor - starts[line])
}

/// Return the byte index of the start of the Unicode codepoint that ends at `pos`.
/// Safe to use as a cursor position or slice boundary.
pub(super) fn char_start_before(s: &str, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut i = pos - 1;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Return the byte index just past the Unicode codepoint that starts at `pos`.
pub(super) fn char_end_at(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    let mut i = pos + 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

pub(super) fn prev_word(s: &str, pos: usize) -> usize {
    // Work on char boundaries to avoid panicking on multi-byte UTF-8 input.
    let before = &s[..pos];
    let trimmed = before.trim_end_matches(' ');
    match trimmed.rfind(' ') {
        Some(i) => i + 1,
        None => 0,
    }
}

pub(super) fn next_word(s: &str, pos: usize) -> usize {
    let after = &s[pos..];
    let skip_non_space = after.find(' ').unwrap_or(after.len());
    let rest = &after[skip_non_space..];
    let skip_space = rest.len() - rest.trim_start_matches(' ').len();
    pos + skip_non_space + skip_space
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_path_with_slash() {
        let (dir, prefix) = split_path("src/ma");
        assert_eq!(dir, "src/");
        assert_eq!(prefix, "ma");
    }

    #[test]
    fn split_path_no_slash() {
        let (dir, prefix) = split_path("main");
        assert_eq!(dir, "");
        assert_eq!(prefix, "main");
    }

    #[test]
    fn split_path_trailing_slash() {
        let (dir, prefix) = split_path("src/");
        assert_eq!(dir, "src/");
        assert_eq!(prefix, "");
    }
}
