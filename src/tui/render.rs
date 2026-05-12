// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::OnceLock;

use inkjet::{Highlighter, Language, constants::HIGHLIGHT_NAMES, theme::Theme};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use super::context_budget::context_used_pct;
use super::palette::{
    SOL_BASE01, SOL_BASE02, SOL_BASE2, SOL_GREEN, SOL_ORANGE, SOL_RED, SOL_YELLOW, code_line_bg,
    entry_style,
};
use super::{App, AppStatus, ChatEntry, EntryKind};

/// Render a fixed-width 10-character context-usage bar followed by a percentage
/// label, returned as styled `Span`s for embedding in the title bar.
///
/// Example (65% used): `▒▒▒▒▒▒░░░░ 65%`
fn render_context_bar(pct: u32) -> Vec<Span<'static>> {
    const BAR_WIDTH: usize = 10;

    let filled = ((pct as usize) * BAR_WIDTH / 100).min(BAR_WIDTH);
    let empty = BAR_WIDTH - filled;

    let (fill_char, fill_color) = if pct >= 80 {
        ('▓', SOL_RED)
    } else if pct >= 60 {
        ('▒', SOL_YELLOW)
    } else {
        ('▒', SOL_GREEN)
    };

    let bar_bg = Style::default().bg(SOL_BASE02);
    let mut spans = Vec::with_capacity(3);
    if filled > 0 {
        let s: String = std::iter::repeat_n(fill_char, filled).collect();
        spans.push(Span::styled(s, bar_bg.fg(fill_color)));
    }
    if empty > 0 {
        spans.push(Span::styled("░".repeat(empty), bar_bg.fg(SOL_BASE01)));
    }
    spans.push(Span::styled(
        format!(" {pct:>3}%", pct = pct),
        bar_bg.fg(SOL_BASE2),
    ));
    spans
}

pub(super) fn render(f: &mut Frame, app: &App) {
    let area = f.area();

    // Wrap the input to the available width up front so the layout, the
    // rendered rows, and the cursor placement all agree on the same row count.
    let prefix_cols: u16 = 2;
    let input_area_cols = area.width.saturating_sub(prefix_cols).max(1);
    let (wrapped_rows, cur_row, cur_col) =
        wrap_input_with_cursor(&app.editor.input, app.editor.cursor, input_area_cols);
    // Show at most 3 wrapped rows; overflow scrolls vertically.
    let visible_input_rows = wrapped_rows.len().clamp(1, 3) as u16;
    let input_height = visible_input_rows + 1; // +1 for the top border

    let chunks = Layout::vertical([
        Constraint::Length(1),            // title bar
        Constraint::Fill(1),              // messages
        Constraint::Length(1),            // status line
        Constraint::Length(input_height), // input
    ])
    .split(area);

    let mode = match (app.executor.is_some(), app.plan_mode) {
        (true, true) => "plan:ON  tools:RO",
        (true, false) => "tools:ON",
        (false, _) => "tools:OFF",
    };
    let left = format!(
        " ShioRamen  [{mode}]  [Tab] complete  [PgUp/Dn] scroll  [F2] select  [Ctrl+C] quit"
    );
    let bar_style = Style::default().bg(SOL_BASE02);
    let title_width = chunks[0].width as usize;
    let title_line = if let Some(pct) = context_used_pct(&app.messages, &app.tools, app.ctx_size) {
        let bar = render_context_bar(pct);
        let pad = title_width
            .saturating_sub(left.len())
            .saturating_sub(bar.iter().map(|s| s.width()).sum::<usize>());
        let mut spans = vec![
            Span::styled(&left, bar_style.fg(SOL_BASE2)),
            Span::styled(" ".repeat(pad), bar_style),
        ];
        spans.extend(bar);
        Line::from(spans)
    } else {
        let pad = title_width.saturating_sub(left.len());
        Line::from(vec![
            Span::styled(&left, bar_style.fg(SOL_BASE2)),
            Span::styled(" ".repeat(pad), bar_style),
        ])
    };
    f.render_widget(Paragraph::new(title_line), chunks[0]);

    let msg_width = chunks[1].width.saturating_sub(1) as usize;
    let streaming_think = if app.stream.show_thinking {
        app.stream.thinking.as_deref()
    } else {
        None
    };
    let all_lines = build_lines(
        &app.entries,
        streaming_think,
        app.stream.streaming.as_deref(),
        msg_width,
    );
    let total = all_lines.len() as u32;
    let visible = chunks[1].height as u32;

    let scroll_y = if app.auto_scroll {
        total.saturating_sub(visible)
    } else {
        app.scroll.min(total.saturating_sub(visible))
    };

    f.render_widget(
        Paragraph::new(all_lines).scroll((scroll_y as u16, 0)),
        chunks[1],
    );

    let (status_text, status_style) = match &app.status {
        AppStatus::Idle => {
            if app.select_mode {
                (
                    "  Select mode — drag to select text, then copy.  [F2] exit select mode"
                        .to_string(),
                    Style::default().fg(SOL_GREEN).add_modifier(Modifier::BOLD),
                )
            } else {
                (String::new(), Style::default())
            }
        }
        AppStatus::Waiting => {
            const FRAMES: &[&str] = &["🤔.", "🤔..", "🤔..."];
            let frame = FRAMES[app.anim_frame as usize % FRAMES.len()];
            (format!("  {frame}"), Style::default().fg(SOL_YELLOW))
        }
        AppStatus::Confirming { prompt, .. } => (
            format!("  Confirm: {prompt}  [y/N]"),
            Style::default().fg(SOL_ORANGE).add_modifier(Modifier::BOLD),
        ),
        AppStatus::ConfirmExit => (
            "  Exit chat? [y/N]".to_string(),
            Style::default().fg(SOL_RED).add_modifier(Modifier::BOLD),
        ),
    };
    f.render_widget(Paragraph::new(status_text).style(status_style), chunks[2]);

    let input_block = Block::default().borders(Borders::TOP);
    let inner = input_block.inner(chunks[3]);
    f.render_widget(input_block, chunks[3]);

    let visible_rows = inner.height as usize;
    let scroll_row: u16 = if visible_rows > 0 && cur_row >= visible_rows {
        (cur_row - visible_rows + 1) as u16
    } else {
        0
    };

    let text_lines: Vec<Line<'_>> = wrapped_rows
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let pfx = if i == 0 { "> " } else { "  " };
            Line::from(vec![Span::raw(pfx), Span::raw(line.clone())])
        })
        .collect();

    f.render_widget(Paragraph::new(text_lines).scroll((scroll_row, 0)), inner);

    if matches!(app.status, AppStatus::Idle) {
        let cx = inner.x + prefix_cols + cur_col;
        let cy = inner.y + (cur_row as u16).saturating_sub(scroll_row);
        f.set_cursor_position((cx, cy));
    }
}

/// Wraps `input` into visual rows that each fit within `width` display columns,
/// breaking on `\n` and on display-width overflow. Returns the wrapped rows
/// together with the cursor's (row, column) inside that wrapped layout.
fn wrap_input_with_cursor(input: &str, cursor: usize, width: u16) -> (Vec<String>, usize, u16) {
    use unicode_width::UnicodeWidthChar;
    let width = width.max(1) as usize;
    let mut rows: Vec<String> = vec![String::new()];
    let mut col: usize = 0;
    let mut cur_pos: Option<(usize, u16)> = None;
    let mut byte_idx = 0usize;

    for ch in input.chars() {
        if cur_pos.is_none() && byte_idx == cursor {
            cur_pos = Some((rows.len() - 1, col as u16));
        }
        if ch == '\n' {
            rows.push(String::new());
            col = 0;
        } else {
            let cw = ch.width().unwrap_or(0);
            if col + cw > width && col > 0 {
                rows.push(String::new());
                col = 0;
            }
            rows.last_mut().unwrap().push(ch);
            col += cw;
        }
        byte_idx += ch.len_utf8();
    }
    if cur_pos.is_none() {
        if col == width {
            rows.push(String::new());
            cur_pos = Some((rows.len() - 1, 0));
        } else {
            cur_pos = Some((rows.len() - 1, col as u16));
        }
    }
    let (cr, cc) = cur_pos.unwrap();
    (rows, cr, cc)
}

fn build_lines(
    entries: &[ChatEntry],
    streaming_think: Option<&str>,
    streaming: Option<&str>,
    width: usize,
) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();

    for entry in entries {
        push_entry_lines(&mut out, entry, width);
        out.push(Line::raw(""));
    }

    if let Some(text) = streaming_think {
        push_entry_lines(
            &mut out,
            &ChatEntry {
                kind: EntryKind::Thinking,
                text: text.to_string(),
            },
            width,
        );
        out.push(Line::raw(""));
    }

    if let Some(text) = streaming {
        push_entry_lines(
            &mut out,
            &ChatEntry {
                kind: EntryKind::Assistant,
                text: text.to_string(),
            },
            width,
        );
        out.push(Line::raw(""));
    }

    out
}

static INKJET_THEME: OnceLock<Theme> = OnceLock::new();

fn inkjet_theme() -> &'static Theme {
    INKJET_THEME.get_or_init(|| {
        Theme::from_helix(inkjet::theme::vendored::SOLARIZED_LIGHT)
            .expect("vendored Solarized Light theme must parse")
    })
}

/// Highlight a single line of source code using inkjet.
/// Returns `Some(vec of (fg_color, text))` on success, `None` if the language
/// is unknown or highlighting fails.
fn highlight_line_inkjet(
    line: &str,
    lang: Option<Language>,
    theme: &Theme,
) -> Option<Vec<(Color, String)>> {
    use inkjet::tree_sitter_highlight::HighlightEvent;

    let lang = lang?;
    let mut hl = Highlighter::new();
    let source = line.to_string();
    let events = hl.highlight_raw(lang, &source).ok()?;

    let default_fg = Color::Rgb(theme.fg.r, theme.fg.g, theme.fg.b);
    let mut result: Vec<(Color, String)> = Vec::new();
    let mut current_fg = default_fg;

    for event in events {
        let Ok(event) = event else { continue };
        match event {
            HighlightEvent::HighlightStart(highlight) => {
                let name = HIGHLIGHT_NAMES.get(highlight.0).copied().unwrap_or("");
                current_fg = theme
                    .get_style(name)
                    .and_then(|s| s.fg)
                    .map(|c| Color::Rgb(c.r, c.g, c.b))
                    .unwrap_or(default_fg);
            }
            HighlightEvent::HighlightEnd => {
                current_fg = default_fg;
            }
            HighlightEvent::Source { start, end } => {
                let text = &line[start..end];
                if !text.is_empty() {
                    result.push((current_fg, text.to_string()));
                }
            }
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn push_entry_lines(out: &mut Vec<Line<'static>>, entry: &ChatEntry, width: usize) {
    use unicode_width::UnicodeWidthStr;

    let (first_prefix, cont_prefix, style) = entry_style(entry.kind);
    let pfx_width = first_prefix.len();
    let text_width = width.saturating_sub(pfx_width).max(10);

    let theme = inkjet_theme();

    let mut first_out = true;
    let mut in_code = false;
    let mut is_diff = false;
    let mut prose_buf: Vec<&str> = Vec::new();
    let mut code_lang: Option<Language> = None;

    for raw_line in entry.text.split('\n') {
        if !in_code {
            if fence_prefix_len(raw_line) > 0 {
                emit_prose(
                    &mut prose_buf,
                    out,
                    &mut first_out,
                    first_prefix,
                    cont_prefix,
                    style,
                    text_width,
                );

                let lang = raw_line[3..].trim();
                is_diff = lang.eq_ignore_ascii_case("diff");
                in_code = true;
                code_lang = Language::from_token(lang);

                let pfx = if first_out { first_prefix } else { cont_prefix };
                first_out = false;
                let label = if lang.is_empty() {
                    "```".to_string()
                } else {
                    format!("```{lang}")
                };
                out.push(Line::from(vec![
                    Span::styled(pfx.to_string(), style),
                    Span::styled(label, Style::default().fg(SOL_BASE01)),
                ]));
            } else {
                prose_buf.push(raw_line);
            }
        } else if fence_prefix_len(raw_line) > 0 {
            in_code = false;
            is_diff = false;
            code_lang = None;
            let pfx = if first_out { first_prefix } else { cont_prefix };
            first_out = false;
            out.push(Line::from(vec![
                Span::styled(pfx.to_string(), style),
                Span::styled("```".to_string(), Style::default().fg(SOL_BASE01)),
            ]));
        } else {
            let pfx = if first_out { first_prefix } else { cont_prefix };
            first_out = false;
            let bg = code_line_bg(raw_line, is_diff);

            let spans = match highlight_line_inkjet(raw_line, code_lang, theme) {
                Some(ranges) => {
                    let mut spans: Vec<Span<'static>> = vec![Span::styled(pfx.to_string(), style)];
                    let mut col = 0usize;
                    for (fg, text) in &ranges {
                        col += text.width();
                        spans.push(Span::styled(
                            text.to_string(),
                            Style::default().fg(*fg).bg(bg),
                        ));
                    }
                    if text_width > col {
                        spans.push(Span::styled(
                            " ".repeat(text_width - col),
                            Style::default().bg(bg),
                        ));
                    }
                    spans
                }
                None => {
                    let display_w = raw_line.width();
                    let padded = format!(
                        "{}{}",
                        raw_line,
                        " ".repeat(text_width.saturating_sub(display_w))
                    );
                    vec![
                        Span::styled(pfx.to_string(), style),
                        Span::styled(padded, Style::default().bg(bg)),
                    ]
                }
            };
            out.push(Line::from(spans));
        }
    }

    emit_prose(
        &mut prose_buf,
        out,
        &mut first_out,
        first_prefix,
        cont_prefix,
        style,
        text_width,
    );

    if first_out {
        out.push(Line::from(vec![
            Span::styled(first_prefix.to_string(), style),
            Span::raw(""),
        ]));
    }
}

/// Returns 3 if `line` begins with ``` or ~~~, otherwise 0.
fn fence_prefix_len(line: &str) -> usize {
    let b = line.as_bytes();
    if b.len() >= 3 && (b[0] == b'`' || b[0] == b'~') && b[1] == b[0] && b[2] == b[0] {
        3
    } else {
        0
    }
}

/// Wraps accumulated prose lines through textwrap and appends them to `out`.
fn emit_prose(
    prose_buf: &mut Vec<&str>,
    out: &mut Vec<Line<'static>>,
    first_out: &mut bool,
    first_prefix: &'static str,
    cont_prefix: &'static str,
    style: Style,
    text_width: usize,
) {
    if prose_buf.is_empty() {
        return;
    }
    let joined = prose_buf.join("\n");
    prose_buf.clear();
    for seg in textwrap::wrap(&joined, text_width).iter() {
        let pfx = if *first_out {
            first_prefix
        } else {
            cont_prefix
        };
        *first_out = false;
        out.push(Line::from(vec![
            Span::styled(pfx.to_string(), style),
            Span::raw(seg.to_string()),
        ]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fence_prefix_len_backticks() {
        assert_eq!(fence_prefix_len("```"), 3);
        assert_eq!(fence_prefix_len("```python"), 3);
        assert_eq!(fence_prefix_len("```rust some text"), 3);
    }

    #[test]
    fn fence_prefix_len_tildes() {
        assert_eq!(fence_prefix_len("~~~"), 3);
        assert_eq!(fence_prefix_len("~~~js"), 3);
    }

    #[test]
    fn fence_prefix_len_not_a_fence() {
        assert_eq!(fence_prefix_len(""), 0);
        assert_eq!(fence_prefix_len("``"), 0);
        assert_eq!(fence_prefix_len("hello"), 0);
        assert_eq!(fence_prefix_len("``~"), 0);
        assert_eq!(fence_prefix_len("`~`"), 0);
    }
}
