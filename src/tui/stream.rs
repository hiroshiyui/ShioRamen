// SPDX-License-Identifier: GPL-3.0-or-later

use super::{App, EntryKind};

/// Replace LaTeX math notation with Unicode equivalents.
/// Local models occasionally emit LaTeX ($\rightarrow$, $\leq$, etc.) even in
/// plain-text contexts; this makes the output readable in a terminal.
pub(super) fn replace_latex(mut s: String) -> String {
    const SUBS: &[(&str, &str)] = &[
        ("$\\rightarrow$", "→"),
        ("$\\leftarrow$", "←"),
        ("$\\Rightarrow$", "⇒"),
        ("$\\Leftarrow$", "⇐"),
        ("$\\leftrightarrow$", "↔"),
        ("$\\uparrow$", "↑"),
        ("$\\downarrow$", "↓"),
        ("$\\to$", "→"),
        ("$\\gets$", "←"),
        ("$\\times$", "×"),
        ("$\\cdot$", "·"),
        ("$\\neq$", "≠"),
        ("$\\approx$", "≈"),
        ("$\\leq$", "≤"),
        ("$\\geq$", "≥"),
        ("$\\infty$", "∞"),
        ("$\\pm$", "±"),
        ("$\\in$", "∈"),
        ("$\\notin$", "∉"),
        ("$\\subset$", "⊂"),
        ("$\\supset$", "⊃"),
    ];
    for (from, to) in SUBS {
        if s.contains(from) {
            s = s.replace(from, to);
        }
    }
    s
}

/// Route tokens from `raw_buf` to `thinking` or `streaming` by splitting at
/// `<think>` / `</think>` tag boundaries.
///
/// Holds back up to `tag_len - 1` bytes at the tail so a tag split across two
/// consecutive tokens is always caught on the next call.
pub(super) fn consume_raw_buf(
    raw_buf: &mut String,
    in_think: &mut bool,
    streaming: &mut Option<String>,
    thinking: &mut Option<String>,
) {
    const THINK_OPEN: &str = "<think>";
    const THINK_CLOSE: &str = "</think>";
    const HOLD_OPEN: usize = THINK_OPEN.len() - 1;
    const HOLD_CLOSE: usize = THINK_CLOSE.len() - 1;

    loop {
        if *in_think {
            match raw_buf.find(THINK_CLOSE) {
                Some(pos) => {
                    let chunk = raw_buf[..pos].to_string();
                    *raw_buf = raw_buf[pos + THINK_CLOSE.len()..].to_string();
                    *in_think = false;
                    if !chunk.is_empty() {
                        match thinking {
                            Some(t) => t.push_str(&chunk),
                            None => *thinking = Some(chunk),
                        }
                    }
                }
                None => {
                    let hold = HOLD_CLOSE.min(raw_buf.len());
                    let mut safe = raw_buf.len() - hold;
                    while safe > 0 && !raw_buf.is_char_boundary(safe) {
                        safe -= 1;
                    }
                    if safe > 0 {
                        let chunk = raw_buf[..safe].to_string();
                        *raw_buf = raw_buf[safe..].to_string();
                        match thinking {
                            Some(t) => t.push_str(&chunk),
                            None => *thinking = Some(chunk),
                        }
                    }
                    break;
                }
            }
        } else {
            match raw_buf.find(THINK_OPEN) {
                Some(pos) => {
                    let chunk = raw_buf[..pos].to_string();
                    *raw_buf = raw_buf[pos + THINK_OPEN.len()..].to_string();
                    *in_think = true;
                    if !chunk.is_empty() {
                        match streaming {
                            Some(s) => s.push_str(&chunk),
                            None => *streaming = Some(chunk),
                        }
                    }
                }
                None => {
                    let hold = HOLD_OPEN.min(raw_buf.len());
                    let mut safe = raw_buf.len() - hold;
                    while safe > 0 && !raw_buf.is_char_boundary(safe) {
                        safe -= 1;
                    }
                    if safe > 0 {
                        let chunk = raw_buf[..safe].to_string();
                        *raw_buf = raw_buf[safe..].to_string();
                        match streaming {
                            Some(s) => s.push_str(&chunk),
                            None => *streaming = Some(chunk),
                        }
                    }
                    break;
                }
            }
        }
    }
}

pub(super) fn finalize_streaming(app: &mut App) {
    if !app.raw_buf.is_empty() {
        let remaining = std::mem::take(&mut app.raw_buf);
        if app.in_think {
            match &mut app.thinking {
                Some(t) => t.push_str(&remaining),
                None => app.thinking = Some(remaining),
            }
        } else {
            match &mut app.streaming {
                Some(s) => s.push_str(&remaining),
                None => {
                    if !remaining.is_empty() {
                        app.streaming = Some(remaining);
                    }
                }
            }
        }
        app.in_think = false;
    }

    if let Some(text) = app.thinking.take() {
        let text = text.trim_matches('\n').to_string();
        if app.show_thinking && !text.is_empty() {
            app.push_entry(EntryKind::Thinking, &text);
        }
    }

    if let Some(text) = app.streaming.take() {
        let text = replace_latex(text);
        app.push_entry(EntryKind::Assistant, &text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_latex_substitutes_known_symbols() {
        assert_eq!(
            replace_latex("use $\\rightarrow$ here".to_string()),
            "use → here"
        );
        assert_eq!(replace_latex("$\\leq$ 5".to_string()), "≤ 5");
        assert_eq!(replace_latex("$\\neq$ 0".to_string()), "≠ 0");
    }

    #[test]
    fn replace_latex_leaves_unknown_alone() {
        let s = "no latex here".to_string();
        assert_eq!(replace_latex(s.clone()), s);
    }

    #[test]
    fn replace_latex_handles_multiple_in_one_string() {
        let out = replace_latex("$\\leq$ x $\\geq$ y".to_string());
        assert_eq!(out, "≤ x ≥ y");
    }

    fn run_consume(input: &str) -> (Option<String>, Option<String>) {
        let mut raw = input.to_string();
        let mut in_think = false;
        let mut streaming: Option<String> = None;
        let mut thinking: Option<String> = None;
        consume_raw_buf(&mut raw, &mut in_think, &mut streaming, &mut thinking);
        // Flush held-back tail (simulates end-of-turn).
        if !raw.is_empty() {
            if in_think {
                match &mut thinking {
                    Some(t) => t.push_str(&raw),
                    None => thinking = Some(raw.clone()),
                }
            } else {
                match &mut streaming {
                    Some(s) => s.push_str(&raw),
                    None => streaming = Some(raw.clone()),
                }
            }
        }
        (streaming, thinking)
    }

    #[test]
    fn consume_raw_buf_no_think_tag_goes_to_streaming() {
        let (streaming, thinking) = run_consume("hello world");
        assert_eq!(streaming.as_deref(), Some("hello world"));
        assert!(thinking.is_none());
    }

    #[test]
    fn consume_raw_buf_think_block_routed_to_thinking() {
        let (streaming, thinking) = run_consume("<think>reason</think>answer");
        assert_eq!(thinking.as_deref(), Some("reason"));
        assert_eq!(streaming.as_deref(), Some("answer"));
    }

    #[test]
    fn consume_raw_buf_think_only_no_response() {
        let (streaming, thinking) = run_consume("<think>just thinking</think>");
        assert_eq!(thinking.as_deref(), Some("just thinking"));
        assert!(streaming.is_none() || streaming.as_deref() == Some(""));
    }

    #[test]
    fn consume_raw_buf_text_before_think_goes_to_streaming() {
        let (streaming, thinking) = run_consume("prefix<think>thought</think>suffix");
        assert_eq!(thinking.as_deref(), Some("thought"));
        assert!(streaming.as_deref().unwrap_or("").contains("prefix"));
        assert!(streaming.as_deref().unwrap_or("").contains("suffix"));
    }

    #[test]
    fn consume_raw_buf_multibyte_emoji_does_not_panic() {
        let input = "# \u{1F3E0}";
        let (streaming, thinking) = run_consume(input);
        assert!(thinking.is_none());
        let s = streaming.unwrap_or_default();
        assert!(s.contains("# \u{1F3E0}") || s.is_empty());
    }
}
