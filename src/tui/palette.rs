// SPDX-License-Identifier: GPL-3.0-or-later

use ratatui::style::{Color, Modifier, Style};

use super::EntryKind;

// Solarized palette: https://ethanschoonover.com/solarized/
pub(super) const SOL_BASE02: Color = Color::Rgb(7, 54, 66);
pub(super) const SOL_BASE01: Color = Color::Rgb(88, 110, 117);
pub(super) const SOL_BASE2: Color = Color::Rgb(238, 232, 213);
pub(super) const SOL_YELLOW: Color = Color::Rgb(181, 137, 0);
pub(super) const SOL_ORANGE: Color = Color::Rgb(203, 75, 22);
pub(super) const SOL_RED: Color = Color::Rgb(220, 50, 47);
pub(super) const SOL_CYAN: Color = Color::Rgb(42, 161, 152);
pub(super) const SOL_GREEN: Color = Color::Rgb(133, 153, 0);

// Code-block backgrounds: Solarized Light Base2 tinted with accent hues.
pub(super) const CODE_BG: Color = SOL_BASE2;
pub(super) const DIFF_ADD_BG: Color = Color::Rgb(220, 232, 200);
pub(super) const DIFF_DEL_BG: Color = Color::Rgb(242, 218, 210);
pub(super) const DIFF_META_BG: Color = Color::Rgb(215, 225, 238);

pub(super) fn entry_style(kind: EntryKind) -> (&'static str, &'static str, Style) {
    // Returns (first-line prefix, continuation indent, style).
    // All prefixes are 7 characters wide for consistent alignment.
    match kind {
        EntryKind::User => ("  you> ", "       ", Style::default().fg(SOL_CYAN)),
        EntryKind::Assistant => (" shio> ", "       ", Style::default()),
        EntryKind::Thinking => (
            "  [~~] ",
            "       ",
            Style::default().fg(SOL_BASE01).add_modifier(Modifier::DIM),
        ),
        EntryKind::ToolCall => ("  [**] ", "       ", Style::default().fg(SOL_YELLOW)),
        EntryKind::ToolResult => (
            "  [-›] ",
            "       ",
            Style::default().fg(SOL_GREEN).add_modifier(Modifier::DIM),
        ),
        EntryKind::Info => ("  [--] ", "       ", Style::default().fg(SOL_BASE01)),
        EntryKind::Error => ("  [!!] ", "       ", Style::default().fg(SOL_RED)),
    }
}

/// Picks the background colour for a line inside a code block.
pub(super) fn code_line_bg(line: &str, is_diff: bool) -> Color {
    if is_diff {
        if line.starts_with("---") || line.starts_with("+++") || line.starts_with("@@") {
            DIFF_META_BG
        } else if line.starts_with('-') {
            DIFF_DEL_BG
        } else if line.starts_with('+') {
            DIFF_ADD_BG
        } else {
            CODE_BG
        }
    } else {
        CODE_BG
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_line_bg_non_diff_always_code_bg() {
        assert_eq!(code_line_bg("hello world", false), CODE_BG);
        assert_eq!(code_line_bg("-removed line", false), CODE_BG);
        assert_eq!(code_line_bg("+added line", false), CODE_BG);
        assert_eq!(code_line_bg("--- a/file", false), CODE_BG);
    }

    #[test]
    fn code_line_bg_diff_removed_line() {
        assert_eq!(code_line_bg("-removed", true), DIFF_DEL_BG);
        assert_eq!(code_line_bg("-", true), DIFF_DEL_BG);
    }

    #[test]
    fn code_line_bg_diff_added_line() {
        assert_eq!(code_line_bg("+added", true), DIFF_ADD_BG);
        assert_eq!(code_line_bg("+", true), DIFF_ADD_BG);
    }

    #[test]
    fn code_line_bg_diff_meta_lines() {
        assert_eq!(code_line_bg("--- a/foo.rs", true), DIFF_META_BG);
        assert_eq!(code_line_bg("+++ b/foo.rs", true), DIFF_META_BG);
        assert_eq!(code_line_bg("@@ -1,5 +1,7 @@", true), DIFF_META_BG);
    }

    #[test]
    fn code_line_bg_diff_context_line() {
        assert_eq!(code_line_bg(" context line", true), CODE_BG);
        assert_eq!(code_line_bg("plain", true), CODE_BG);
    }

    #[test]
    fn entry_style_all_prefixes_are_seven_chars_wide() {
        for kind in [
            EntryKind::User,
            EntryKind::Assistant,
            EntryKind::Thinking,
            EntryKind::ToolCall,
            EntryKind::ToolResult,
            EntryKind::Info,
            EntryKind::Error,
        ] {
            let (prefix, indent, _) = entry_style(kind);
            assert_eq!(
                prefix.chars().count(),
                7,
                "prefix for {kind:?} should be 7 chars"
            );
            assert_eq!(
                indent.chars().count(),
                7,
                "indent for {kind:?} should be 7 chars"
            );
        }
    }

    #[test]
    fn entry_style_prefixes_are_distinct() {
        let kinds = [
            EntryKind::User,
            EntryKind::Assistant,
            EntryKind::Thinking,
            EntryKind::ToolCall,
            EntryKind::ToolResult,
            EntryKind::Info,
            EntryKind::Error,
        ];
        let prefixes: Vec<_> = kinds.iter().map(|&k| entry_style(k).0).collect();
        let unique: std::collections::HashSet<_> = prefixes.iter().collect();
        assert_eq!(unique.len(), prefixes.len());
    }
}
