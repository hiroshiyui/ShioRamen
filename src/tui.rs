// SPDX-License-Identifier: GPL-3.0-or-later
use std::collections::HashMap;
use std::io;
use std::sync::OnceLock;

use anyhow::Result;
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
        KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use syntect::{easy::HighlightLines, highlighting::ThemeSet, parsing::SyntaxSet};
use tokio::sync::{mpsc, oneshot};

use crate::chat::ChatSession;
use crate::client::{AgentTurn, LlamaClient, Message, ToolCallItem, ToolDef};
use crate::config::SkillDef;
use crate::context;
use crate::tools::ToolExecutor;

// ── Events from model task → TUI ─────────────────────────────────────────────

enum TuiEvent {
    /// A streaming token (non-agent mode).
    StreamToken(String),
    /// A tool call is starting; the string is a short display label.
    ToolStart(String),
    /// A tool call finished; the string is a one-line result preview.
    ToolDone(String),
    /// The model needs confirmation before a destructive action.
    NeedsConfirm {
        prompt: String,
        reply_tx: oneshot::Sender<bool>,
    },
    /// Non-streaming text response (agent mode final answer).
    AssistantText(String),
    /// The turn completed successfully; contains the updated message history.
    TurnDone(Vec<Message>),
    /// The turn failed.
    TurnError(String),
    /// Plan mode was toggled by the model.
    PlanModeChanged(bool),
}

// ── Chat display ──────────────────────────────────────────────────────────────

struct ChatEntry {
    kind: EntryKind,
    text: String,
}

#[derive(Clone, Copy, Debug)]
enum EntryKind {
    User,
    Assistant,
    ToolCall,
    ToolResult,
    Info,
    Error,
}

// ── Solarized palette ─────────────────────────────────────────────────────────
// https://ethanschoonover.com/solarized/
const SOL_BASE02: Color = Color::Rgb(7, 54, 66); // bg highlights (dark theme)
const SOL_BASE01: Color = Color::Rgb(88, 110, 117); // comments / secondary
const SOL_BASE2: Color = Color::Rgb(238, 232, 213); // bg highlights (light theme)
const SOL_YELLOW: Color = Color::Rgb(181, 137, 0);
const SOL_ORANGE: Color = Color::Rgb(203, 75, 22);
const SOL_RED: Color = Color::Rgb(220, 50, 47);
const SOL_CYAN: Color = Color::Rgb(42, 161, 152);
const SOL_GREEN: Color = Color::Rgb(133, 153, 0);

fn entry_style(kind: EntryKind) -> (&'static str, &'static str, Style) {
    // Returns (first-line prefix, continuation indent, style)
    // All prefixes are 7 characters wide for consistent alignment.
    match kind {
        EntryKind::User => ("  you> ", "       ", Style::default().fg(SOL_CYAN)),
        EntryKind::Assistant => (" shio> ", "       ", Style::default()),
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

// ── App state ─────────────────────────────────────────────────────────────────

struct App {
    // Chat session state
    messages: Vec<Message>,
    tools: Vec<ToolDef>,
    executor: Option<ToolExecutor>,
    client: LlamaClient,
    temperature: f32,

    // Display
    entries: Vec<ChatEntry>,
    streaming: Option<String>, // assistant response being built token-by-token
    scroll: u16,
    auto_scroll: bool,

    // Input
    input: String,
    cursor: usize,
    history: Vec<String>,
    hist_idx: Option<usize>,
    saved: String, // input saved when browsing history

    // Tab completion state
    comp_candidates: Vec<String>,
    comp_idx: usize,

    // Async communication from model task
    event_tx: mpsc::UnboundedSender<TuiEvent>,
    event_rx: mpsc::UnboundedReceiver<TuiEvent>,

    /// Custom skills loaded from shio.toml, keyed by name.
    skills: HashMap<String, SkillDef>,

    status: AppStatus,
    anim_frame: u8,    // cycles 0-2 while Waiting, drives the thinking animation
    select_mode: bool, // when true, mouse capture is disabled so the terminal can select text
    plan_mode: bool,   // true when the model has entered plan/read-only mode
    quit: bool,

    // Handle to the currently running model task; aborted on quit.
    model_task: Option<tokio::task::JoinHandle<()>>,

    /// Context window size in tokens (0 = unknown).  Used to trim history
    /// before dispatch so we never send more than ~80 % of the context.
    ctx_size: u32,
}

enum AppStatus {
    Idle,
    Waiting,
    Confirming {
        prompt: String,
        reply_tx: oneshot::Sender<bool>,
    },
    ConfirmExit,
}

impl App {
    fn push_entry(&mut self, kind: EntryKind, text: &str) {
        self.entries.push(ChatEntry {
            kind,
            text: text.to_string(),
        });
        self.auto_scroll = true;
    }
    fn push_info(&mut self, text: &str) {
        self.push_entry(EntryKind::Info, text);
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub async fn run(session: ChatSession) -> Result<()> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(out);
    let mut term = Terminal::new(backend)?;

    let result = run_loop(&mut term, session).await;

    // Always restore terminal, even on error.
    disable_raw_mode()?;
    execute!(
        term.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    term.show_cursor()?;
    result
}

async fn run_loop(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    session: ChatSession,
) -> Result<()> {
    let (event_tx, event_rx) = mpsc::unbounded_channel::<TuiEvent>();

    let has_tools = session.executor.is_some();
    let has_skills = !session.skills.is_empty();
    let mut app = App {
        messages: session.messages,
        tools: session.tools,
        executor: session.executor,
        client: session.client,
        temperature: session.temperature,
        entries: Vec::new(),
        streaming: None,
        scroll: 0,
        auto_scroll: true,
        input: String::new(),
        cursor: 0,
        history: Vec::new(),
        hist_idx: None,
        saved: String::new(),
        comp_candidates: Vec::new(),
        comp_idx: 0,
        event_tx,
        event_rx,
        skills: session.skills,
        status: AppStatus::Idle,
        anim_frame: 0,
        select_mode: false,
        plan_mode: false,
        quit: false,
        model_task: None,
        ctx_size: session.ctx_size,
    };

    let welcome = match (has_tools, has_skills) {
        (true, true) => {
            "ShioRamen ready — tool use ON.  /clear /stats /include <path> /tools /skills /exit   PgUp/Dn to scroll"
        }
        (true, false) => {
            "ShioRamen ready — tool use ON.  /clear /stats /include <path> /tools /exit   PgUp/Dn to scroll"
        }
        (false, true) => {
            "ShioRamen ready.  /clear /stats /include <path> /skills /exit   PgUp/Dn to scroll"
        }
        (false, false) => {
            "ShioRamen ready.  /clear /stats /include <path> /exit   PgUp/Dn to scroll"
        }
    };
    app.push_info(welcome);

    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(400));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    term.draw(|f| render(f, &app))?;

    while !app.quit {
        tokio::select! {
            maybe_ev = events.next() => match maybe_ev {
                Some(Ok(Event::Key(key))) => {
                    if handle_key(&mut app, key).await { app.quit = true; }
                }
                Some(Ok(Event::Mouse(mouse))) => {
                    match mouse.kind {
                        MouseEventKind::ScrollUp   => view_scroll(&mut app, -3),
                        MouseEventKind::ScrollDown => view_scroll(&mut app,  3),
                        _ => {}
                    }
                }
                Some(Ok(Event::Resize(_, _))) => {}   // just re-render
                Some(Err(e)) => return Err(e.into()),
                None => { app.quit = true; }
                _ => {}
            },
            maybe_model = app.event_rx.recv() => {
                if let Some(ev) = maybe_model {
                    handle_model_event(&mut app, ev);
                }
            },
            _ = ticker.tick() => {
                if matches!(app.status, AppStatus::Waiting) {
                    app.anim_frame = app.anim_frame.wrapping_add(1);
                }
            }
        }

        term.draw(|f| render(f, &app))?;
    }

    if let Some(h) = app.model_task.take() {
        h.abort();
    }

    Ok(())
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn render(f: &mut Frame, app: &App) {
    let area = f.area();

    // Expand input area height for multi-line input (cap at 10 visible lines).
    let input_line_count = (app.input.chars().filter(|&c| c == '\n').count() + 1).min(10) as u16;
    let input_height = input_line_count + 1; // +1 for the top border

    let chunks = Layout::vertical([
        Constraint::Length(1),            // title bar
        Constraint::Fill(1),              // messages
        Constraint::Length(1),            // status line
        Constraint::Length(input_height), // input
    ])
    .split(area);

    // ── Title bar ──────────────────────────────────────────────────────────────
    let mode = match (app.executor.is_some(), app.plan_mode) {
        (true, true) => "plan:ON  tools:RO",
        (true, false) => "tools:ON",
        (false, _) => "tools:OFF",
    };
    let title_str = format!(
        " ShioRamen  [{mode}]  [Tab] complete  [PgUp/Dn] scroll  [F2] select  [Ctrl+C] quit"
    );
    f.render_widget(
        Paragraph::new(title_str).style(Style::default().bg(SOL_BASE02).fg(SOL_BASE2)),
        chunks[0],
    );

    // ── Messages area ──────────────────────────────────────────────────────────
    let msg_width = chunks[1].width.saturating_sub(1) as usize;
    let all_lines = build_lines(&app.entries, app.streaming.as_deref(), msg_width);
    let total = all_lines.len() as u16;
    let visible = chunks[1].height;

    let scroll_y = if app.auto_scroll {
        total.saturating_sub(visible)
    } else {
        app.scroll.min(total.saturating_sub(visible))
    };

    f.render_widget(Paragraph::new(all_lines).scroll((scroll_y, 0)), chunks[1]);

    // ── Status line ────────────────────────────────────────────────────────────
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

    // ── Input area ─────────────────────────────────────────────────────────────
    let input_block = Block::default().borders(Borders::TOP);
    let inner = input_block.inner(chunks[3]);
    f.render_widget(input_block, chunks[3]);

    use unicode_width::UnicodeWidthStr;
    let prefix = "> ";
    let cont_prefix = "  ";
    let prefix_cols = prefix.width() as u16; // 2

    // Determine cursor line and its display column.
    let (cursor_line, _) = cursor_line_col(&app.input, app.cursor);
    let cursor_col_str = app.input[..app.cursor]
        .rsplit_once('\n')
        .map(|(_, after)| after)
        .unwrap_or(&app.input[..app.cursor]);
    let cursor_col = cursor_col_str.width() as u16;

    let input_area_cols = inner.width.saturating_sub(prefix_cols);

    // Horizontal scroll only for single-line input to avoid distorting other
    // lines in multi-line mode.
    let input_lines: Vec<&str> = if app.input.is_empty() {
        vec![""]
    } else {
        app.input.split('\n').collect()
    };
    let scroll_x: u16 = if input_lines.len() == 1 && cursor_col >= input_area_cols {
        cursor_col - input_area_cols + 1
    } else {
        0
    };

    // Vertical scroll to keep cursor line visible when input is capped at 10 rows.
    let visible_rows = inner.height as usize;
    let scroll_row: u16 = if cursor_line >= visible_rows {
        (cursor_line - visible_rows + 1) as u16
    } else {
        0
    };

    // Build one ratatui Line per input row with the appropriate prefix.
    let text_lines: Vec<Line<'_>> = input_lines
        .iter()
        .enumerate()
        .map(|(i, &line)| {
            let pfx = if i == 0 { prefix } else { cont_prefix };
            Line::from(vec![Span::raw(pfx), Span::raw(line)])
        })
        .collect();

    f.render_widget(
        Paragraph::new(text_lines).scroll((scroll_row, scroll_x)),
        inner,
    );

    // Draw cursor only while input is active.
    if matches!(app.status, AppStatus::Idle) {
        let cx = inner.x + prefix_cols + cursor_col.saturating_sub(scroll_x);
        let cy = inner.y + (cursor_line as u16).saturating_sub(scroll_row);
        f.set_cursor_position((cx, cy));
    }
}

fn build_lines(entries: &[ChatEntry], streaming: Option<&str>, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();

    for entry in entries {
        push_entry_lines(&mut out, entry, width);
        out.push(Line::raw(""));
    }

    // In-progress streaming response shown at the bottom.
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

// ── Syntax highlighting ───────────────────────────────────────────────────────

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme_set() -> &'static ThemeSet {
    THEME_SET.get_or_init(ThemeSet::load_defaults)
}

/// ── Code-block backgrounds — Solarized Light: Base2 tinted with accent hues. ─
const CODE_BG: Color = SOL_BASE2; // Base2  — standard code bg
const DIFF_ADD_BG: Color = Color::Rgb(220, 232, 200); // Base2 + green tint
const DIFF_DEL_BG: Color = Color::Rgb(242, 218, 210); // Base2 + red tint
const DIFF_META_BG: Color = Color::Rgb(215, 225, 238); // Base2 + blue tint

fn push_entry_lines(out: &mut Vec<Line<'static>>, entry: &ChatEntry, width: usize) {
    use unicode_width::UnicodeWidthStr;

    let (first_prefix, cont_prefix, style) = entry_style(entry.kind);
    let pfx_width = first_prefix.len(); // all prefixes are exactly 7 ASCII cols
    let text_width = width.saturating_sub(pfx_width).max(10);

    let ss = syntax_set();
    let theme = &theme_set().themes["Solarized (light)"];

    let mut first_out = true;
    let mut in_code = false;
    let mut is_diff = false;
    let mut prose_buf: Vec<&str> = Vec::new();
    let mut highlighter: Option<HighlightLines<'_>> = None;

    for raw_line in entry.text.split('\n') {
        if !in_code {
            if fence_prefix_len(raw_line) > 0 {
                // Flush accumulated prose through textwrap first.
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

                // Set up the syntax highlighter for this language.
                let syntax = ss
                    .find_syntax_by_token(lang)
                    .unwrap_or_else(|| ss.find_syntax_plain_text());
                highlighter = Some(HighlightLines::new(syntax, theme));

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
            // Closing fence.
            in_code = false;
            is_diff = false;
            highlighter = None;
            let pfx = if first_out { first_prefix } else { cont_prefix };
            first_out = false;
            out.push(Line::from(vec![
                Span::styled(pfx.to_string(), style),
                Span::styled("```".to_string(), Style::default().fg(SOL_BASE01)),
            ]));
        } else {
            // Code line — syntax-highlighted foreground, Morandi background.
            let pfx = if first_out { first_prefix } else { cont_prefix };
            first_out = false;
            let bg = code_line_bg(raw_line, is_diff);

            // highlight_line expects a trailing newline when using the
            // `load_defaults_newlines` syntax set.
            let line_nl = format!("{raw_line}\n");
            let spans = match highlighter
                .as_mut()
                .and_then(|h| h.highlight_line(&line_nl, ss).ok())
            {
                Some(ranges) => {
                    let mut spans: Vec<Span<'static>> = vec![Span::styled(pfx.to_string(), style)];
                    let mut col = 0usize;
                    for (syn_style, text) in &ranges {
                        // Strip the trailing newline syntect appended.
                        let text = text.trim_end_matches('\n');
                        if text.is_empty() {
                            continue;
                        }
                        let fg = Color::Rgb(
                            syn_style.foreground.r,
                            syn_style.foreground.g,
                            syn_style.foreground.b,
                        );
                        col += text.width();
                        spans.push(Span::styled(
                            text.to_string(),
                            Style::default().fg(fg).bg(bg),
                        ));
                    }
                    // Trailing pad so the background fills the whole column.
                    if text_width > col {
                        spans.push(Span::styled(
                            " ".repeat(text_width - col),
                            Style::default().bg(bg),
                        ));
                    }
                    spans
                }
                None => {
                    // Fallback: plain text with background colour.
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

    // Flush any remaining prose after the last code block (or when there were no
    // code blocks at all — the common case).
    emit_prose(
        &mut prose_buf,
        out,
        &mut first_out,
        first_prefix,
        cont_prefix,
        style,
        text_width,
    );

    // Guard: if entry text was empty, emit the prefix alone.
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

/// Picks the background colour for a line inside a code block.
fn code_line_bg(line: &str, is_diff: bool) -> Color {
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

// ── Key handling ──────────────────────────────────────────────────────────────

/// Returns `true` if the application should quit.
async fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    use KeyCode::*;
    use KeyModifiers as Mods;

    // In ConfirmExit state: y/Enter quits, anything else cancels.
    if matches!(app.status, AppStatus::ConfirmExit) {
        match key.code {
            Char('y') | Char('Y') | Enter => return true,
            _ => {
                app.status = AppStatus::Idle;
            }
        }
        return false;
    }

    // In Confirming state: only y/n/Escape accepted.
    if let AppStatus::Confirming { .. } = &app.status {
        match key.code {
            Char('y') | Char('Y') => send_confirm(app, true),
            Char('n') | Char('N') | Esc | Enter => send_confirm(app, false),
            _ => {}
        }
        return false;
    }

    // While waiting for model: Esc aborts the turn; Ctrl+C quits.
    if matches!(app.status, AppStatus::Waiting) {
        match key.code {
            Esc => {
                if let Some(h) = app.model_task.take() {
                    h.abort();
                }
                finalize_streaming(app);
                app.status = AppStatus::Idle;
                app.plan_mode = false;
                app.push_info("Interrupted.");
            }
            Char('c') if key.modifiers.contains(Mods::CONTROL) => return true,
            _ => {}
        }
        return false;
    }

    // ── Normal editing ────────────────────────────────────────────────────────
    match (key.code, key.modifiers) {
        // Ctrl+C / Ctrl+D → ask for confirmation before quitting.
        (Char('c'), m) | (Char('d'), m) if m.contains(Mods::CONTROL) => {
            app.status = AppStatus::ConfirmExit;
        }

        // Newline (Alt+Enter inserts a literal newline; plain Enter submits)
        (Enter, m) if m.contains(Mods::ALT) => {
            app.input.insert(app.cursor, '\n');
            app.cursor += 1;
            app.comp_candidates.clear();
        }
        // Submit
        (Enter, _) => submit(app).await,

        // Delete
        (Backspace, _) => {
            if app.cursor > 0 {
                let new = char_start_before(&app.input, app.cursor);
                app.input.drain(new..app.cursor);
                app.cursor = new;
                app.comp_candidates.clear();
            }
        }
        (Delete, _) => {
            if app.cursor < app.input.len() {
                let next = char_end_at(&app.input, app.cursor);
                app.input.drain(app.cursor..next);
                app.comp_candidates.clear();
            }
        }

        // Cursor movement
        (Left, m) if m.contains(Mods::CONTROL) => {
            app.cursor = prev_word(&app.input, app.cursor);
        }
        (Right, m) if m.contains(Mods::CONTROL) => {
            app.cursor = next_word(&app.input, app.cursor);
        }
        (Left, _) => {
            app.cursor = char_start_before(&app.input, app.cursor);
        }
        (Right, _) => {
            app.cursor = char_end_at(&app.input, app.cursor);
        }
        (Home, _) => {
            let (line, _) = cursor_line_col(&app.input, app.cursor);
            app.cursor = line_starts(&app.input)[line];
        }
        (End, _) => {
            let (line, _) = cursor_line_col(&app.input, app.cursor);
            let starts = line_starts(&app.input);
            app.cursor = if line + 1 < starts.len() {
                starts[line + 1] - 1 // position of the '\n', not past it
            } else {
                app.input.len()
            };
        }

        // Up: move cursor up one line within multi-line input, or go to history.
        (Up, _) => {
            let (line, col) = cursor_line_col(&app.input, app.cursor);
            if line == 0 {
                hist_prev(app);
            } else {
                let starts = line_starts(&app.input);
                let prev_start = starts[line - 1];
                let prev_len = starts[line] - 1 - prev_start; // bytes before the '\n'
                app.cursor = prev_start + col.min(prev_len);
            }
        }
        // Down: move cursor down one line within multi-line input, or go to history.
        (Down, _) => {
            let (line, col) = cursor_line_col(&app.input, app.cursor);
            let starts = line_starts(&app.input);
            if line + 1 >= starts.len() {
                hist_next(app);
            } else {
                let next_start = starts[line + 1];
                let next_len = if line + 2 < starts.len() {
                    starts[line + 2] - 1 - next_start
                } else {
                    app.input.len() - next_start
                };
                app.cursor = next_start + col.min(next_len);
            }
        }

        // Scroll
        (PageUp, _) => view_scroll(app, -10),
        (PageDown, _) => view_scroll(app, 10),

        // Tab completion
        (Tab, _) => do_complete(app),

        // Select mode toggle
        (F(2), _) => toggle_select_mode(app),

        // Bash-style line editing
        (Char('a'), m) if m.contains(Mods::CONTROL) => {
            let (line, _) = cursor_line_col(&app.input, app.cursor);
            app.cursor = line_starts(&app.input)[line];
        }
        (Char('e'), m) if m.contains(Mods::CONTROL) => {
            let (line, _) = cursor_line_col(&app.input, app.cursor);
            let starts = line_starts(&app.input);
            app.cursor = if line + 1 < starts.len() {
                starts[line + 1] - 1
            } else {
                app.input.len()
            };
        }
        (Char('u'), m) if m.contains(Mods::CONTROL) => {
            app.input.clear();
            app.cursor = 0;
            app.comp_candidates.clear();
        }
        (Char('w'), m) if m.contains(Mods::CONTROL) => {
            let new_cursor = prev_word(&app.input, app.cursor);
            app.input.drain(new_cursor..app.cursor);
            app.cursor = new_cursor;
            app.comp_candidates.clear();
        }

        // Regular character
        (Char(c), _) => {
            app.input.insert(app.cursor, c);
            app.cursor += c.len_utf8();
            app.comp_candidates.clear();
        }

        _ => {}
    }

    false
}

fn toggle_select_mode(app: &mut App) {
    use std::io::stdout;
    let want_select = !app.select_mode;
    let result = if want_select {
        execute!(stdout(), DisableMouseCapture)
    } else {
        execute!(stdout(), EnableMouseCapture)
    };
    match result {
        Ok(()) => app.select_mode = want_select,
        Err(e) => app.push_entry(
            EntryKind::Error,
            &format!("Failed to toggle select mode: {e}"),
        ),
    }
}

fn send_confirm(app: &mut App, yes: bool) {
    let old = std::mem::replace(&mut app.status, AppStatus::Waiting);
    if let AppStatus::Confirming { reply_tx, .. } = old {
        let _ = reply_tx.send(yes);
    }
}

fn view_scroll(app: &mut App, delta: i16) {
    app.auto_scroll = false;
    app.scroll = (app.scroll as i32 + delta as i32).max(0) as u16;
}

fn hist_prev(app: &mut App) {
    if app.history.is_empty() {
        return;
    }
    match app.hist_idx {
        None => {
            app.saved = app.input.clone();
            app.hist_idx = Some(app.history.len() - 1);
        }
        Some(0) => return,
        Some(ref mut i) => {
            *i -= 1;
        }
    }
    let idx = app.hist_idx.expect("hist_idx set in match above");
    app.input = app.history[idx].clone();
    app.cursor = app.input.len();
}

fn hist_next(app: &mut App) {
    match app.hist_idx {
        None => (),
        Some(idx) if idx + 1 >= app.history.len() => {
            app.hist_idx = None;
            app.input = app.saved.clone();
            app.cursor = app.input.len();
        }
        Some(ref mut i) => {
            *i += 1;
            let idx = *i;
            app.input = app.history[idx].clone();
            app.cursor = app.input.len();
        }
    }
}

fn do_complete(app: &mut App) {
    const SLASH_CMDS: &[&str] = &[
        "/exit",
        "/quit",
        "/reset",
        "/clear",
        "/stats",
        "/include ",
        "/tools",
        "/skills",
    ];

    let typed = app.input[..app.cursor].to_string();

    if app.comp_candidates.is_empty() {
        // Build candidate list for the current input.
        let candidates: Vec<String> = if let Some(path_part) = typed.strip_prefix("/include ") {
            let (dir, prefix) = split_path(path_part);
            list_path_completions(&dir, &prefix)
                .into_iter()
                .map(|c| format!("/include {c}"))
                .collect()
        } else if typed.starts_with('/') {
            let mut all: Vec<String> = SLASH_CMDS
                .iter()
                .filter(|&&c| c.starts_with(typed.as_str()))
                .map(|&c| c.to_string())
                .collect();
            // Add dynamic skill names (e.g. "/commit", "/review").
            for name in app.skills.keys() {
                let slash_name = format!("/{name}");
                if slash_name.starts_with(typed.as_str()) {
                    all.push(slash_name);
                }
            }
            all.sort();
            all.dedup();
            all
        } else {
            return;
        };

        if candidates.is_empty() {
            return;
        }
        app.comp_candidates = candidates;
        app.comp_idx = 0;
    } else {
        // Cycle through the existing candidates.
        app.comp_idx = (app.comp_idx + 1) % app.comp_candidates.len();
    }

    let c = app.comp_candidates[app.comp_idx].clone();
    app.input = c;
    app.cursor = app.input.len();
}

fn split_path(path: &str) -> (String, String) {
    match path.rfind('/') {
        Some(p) => (path[..=p].to_string(), path[p + 1..].to_string()),
        None => (String::new(), path.to_string()),
    }
}

fn list_path_completions(dir: &str, prefix: &str) -> Vec<String> {
    let read_path = if dir.is_empty() { "." } else { dir };
    let Ok(entries) = std::fs::read_dir(read_path) else {
        return vec![];
    };
    let mut results: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(prefix) {
                let trail = if e.path().is_dir() { "/" } else { "" };
                Some(format!("{dir}{name}{trail}"))
            } else {
                None
            }
        })
        .collect();
    results.sort();
    results
}

/// Returns the byte offset of the start of each line (split by `\n`).
fn line_starts(s: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in s.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// Returns `(line_index, byte_column_within_line)` for the given cursor byte offset.
fn cursor_line_col(s: &str, cursor: usize) -> (usize, usize) {
    let starts = line_starts(s);
    let line = starts.partition_point(|&st| st <= cursor).saturating_sub(1);
    (line, cursor - starts[line])
}

/// Return the byte index of the start of the Unicode codepoint that ends at `pos`.
/// Safe to use as a cursor position or slice boundary.
fn char_start_before(s: &str, pos: usize) -> usize {
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
fn char_end_at(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    let mut i = pos + 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn prev_word(s: &str, pos: usize) -> usize {
    let bytes = s.as_bytes();
    let mut i = pos;
    while i > 0 && bytes[i - 1] == b' ' {
        i -= 1;
    }
    while i > 0 && bytes[i - 1] != b' ' {
        i -= 1;
    }
    i
}

fn next_word(s: &str, pos: usize) -> usize {
    let bytes = s.as_bytes();
    let mut i = pos;
    while i < bytes.len() && bytes[i] != b' ' {
        i += 1;
    }
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    i
}

// ── Input submission ──────────────────────────────────────────────────────────

async fn submit(app: &mut App) {
    let input = app.input.trim().to_string();
    if input.is_empty() {
        return;
    }

    // Save to input history (deduplicate consecutive identical entries).
    if app.history.last() != Some(&input) {
        app.history.push(input.clone());
    }
    app.hist_idx = None;
    app.input.clear();
    app.cursor = 0;
    app.comp_candidates.clear();
    app.auto_scroll = true;

    match input.as_str() {
        "/exit" | "/quit" => {
            app.status = AppStatus::ConfirmExit;
            return;
        }
        "/reset" | "/clear" => {
            app.messages.truncate(1);
            app.entries.clear();
            app.streaming = None;
            app.push_info("History cleared.");
            return;
        }
        "/stats" => {
            match app.client.slots().await {
                Ok(slots) => {
                    let lines: Vec<String> = slots
                        .iter()
                        .map(|s| {
                            let state = if s.is_processing { "busy" } else { "idle" };
                            let pct = if s.n_ctx > 0 {
                                s.n_past * 100 / s.n_ctx
                            } else {
                                0
                            };
                            format!(
                                "slot {}: {state}  {}/{} tokens used ({}%)",
                                s.id, s.n_past, s.n_ctx, pct
                            )
                        })
                        .collect();
                    app.push_info(&lines.join("\n"));
                }
                Err(e) => app.push_entry(EntryKind::Error, &format!("stats: {e}")),
            }
            return;
        }
        "/tools" => {
            if app.tools.is_empty() {
                app.push_info("No tools available.");
            } else {
                let list = app
                    .tools
                    .iter()
                    .map(|t| format!("{} — {}", t.function.name, t.function.description))
                    .collect::<Vec<_>>()
                    .join("\n");
                app.push_info(&list);
            }
            return;
        }
        "/skills" => {
            if app.skills.is_empty() {
                app.push_info(
                    "No custom skills defined. Add [skills.<name>] sections to shio.toml.",
                );
            } else {
                let mut names: Vec<&String> = app.skills.keys().collect();
                names.sort();
                let list = names
                    .iter()
                    .map(|name| format!("/{} — {}", name, app.skills[*name].description))
                    .collect::<Vec<_>>()
                    .join("\n");
                app.push_info(&list);
            }
            return;
        }
        _ if input.starts_with("/include ") => {
            let path_str = input["/include ".len()..].trim();
            cmd_include(app, path_str);
            return;
        }
        _ => {}
    }

    // Custom skill dispatch (built-in commands take precedence above).
    if let Some(expanded) = try_expand_skill(app, &input) {
        app.push_entry(EntryKind::User, &input);
        app.messages.push(Message::user(&expanded));
        app.status = AppStatus::Waiting;
        app.anim_frame = 0;
        dispatch_turn(app);
        return;
    }

    // Regular user message.
    app.push_entry(EntryKind::User, &input);
    app.messages.push(Message::user(&input));
    app.status = AppStatus::Waiting;
    app.anim_frame = 0;
    dispatch_turn(app);
}

fn cmd_include(app: &mut App, path_str: &str) {
    let path = std::path::Path::new(path_str);
    match context::collect(path) {
        Err(e) => {
            app.push_entry(EntryKind::Error, &format!("include: {e}"));
        }
        Ok(ref files) if files.is_empty() => {
            app.push_info(&format!("No source files found in {path_str}"));
        }
        Ok(files) => {
            let count = files.len();
            let total_bytes: usize = files.iter().map(|(_, c)| c.len()).sum();
            let content = context::format_as_blocks(&files);
            app.messages.push(Message::user(&content));
            app.messages.push(Message::assistant(format!(
                "Understood. I've loaded {count} file(s) and am ready for your questions.",
            )));
            app.push_info(&format!("Included {count} file(s) from {path_str}"));
            if total_bytes > 512 * 1024 {
                app.push_entry(
                    EntryKind::Info,
                    &format!(
                        "Warning: {:.0} KB of context injected.",
                        total_bytes as f64 / 1024.0
                    ),
                );
            }
        }
    }
}

/// Expand a skill prompt template with the given args string.
/// `{args}` is replaced by `args`; if the placeholder is absent and `args` is
/// non-empty, they are appended after the prompt. The result is trimmed.
fn expand_skill_prompt(prompt: &str, args: &str) -> String {
    if prompt.contains("{args}") {
        prompt.replace("{args}", args).trim().to_string()
    } else if !args.is_empty() {
        format!("{} {}", prompt.trim_end(), args)
    } else {
        prompt.to_string()
    }
}

/// If `input` is a skill invocation (`/<name> [args]`), return the expanded
/// prompt. Returns `None` for unknown skill names or non-slash input.
/// Built-in commands are checked by the caller before this is reached.
fn try_expand_skill(app: &App, input: &str) -> Option<String> {
    let rest = input.strip_prefix('/')?;
    let (skill_name, raw_args) = match rest.find(char::is_whitespace) {
        Some(pos) => (&rest[..pos], rest[pos + 1..].trim()),
        None => (rest, ""),
    };
    let skill = app.skills.get(skill_name)?;
    Some(expand_skill_prompt(&skill.prompt, raw_args))
}

/// Drop the oldest non-system messages from `msgs` until the total content
/// length in bytes fits within `budget`.  The system prompt (index 0) is
/// always kept.  Returns the number of messages removed.
fn trim_to_budget(msgs: &mut Vec<Message>, budget: usize) -> usize {
    let mut dropped = 0;
    loop {
        let total: usize = msgs
            .iter()
            .map(|m| m.content.as_deref().map_or(0, str::len))
            .sum();
        if total <= budget || msgs.len() <= 1 {
            break;
        }
        msgs.remove(1);
        dropped += 1;
    }
    dropped
}

fn dispatch_turn(app: &mut App) {
    // Trim conversation history if approaching the context window limit.
    // Budget: 80 % of ctx_size tokens, estimated at 4 bytes per token.
    if app.ctx_size > 0 {
        let budget = app.ctx_size as usize * 4 * 80 / 100;
        let dropped = trim_to_budget(&mut app.messages, budget);
        if dropped > 0 {
            app.push_info(&format!(
                "Context limit approaching — dropped {dropped} old message(s) from history."
            ));
        }
    }

    let client = app.client.clone();
    let msgs = app.messages.clone();
    let temp = app.temperature;
    let tools = app.tools.clone();
    let executor = app.executor.clone();
    let tx = app.event_tx.clone();

    app.model_task = Some(tokio::spawn(async move {
        run_model_task(client, msgs, temp, tools, executor, tx).await;
    }));
}

// ── Model event handling ──────────────────────────────────────────────────────

fn handle_model_event(app: &mut App, ev: TuiEvent) {
    match ev {
        TuiEvent::StreamToken(token) => {
            match &mut app.streaming {
                Some(s) => s.push_str(&token),
                None => app.streaming = Some(token),
            }
            app.auto_scroll = true;
        }
        TuiEvent::ToolStart(label) => {
            finalize_streaming(app);
            app.push_entry(EntryKind::ToolCall, &label);
        }
        TuiEvent::ToolDone(preview) => {
            app.push_entry(EntryKind::ToolResult, &preview);
        }
        TuiEvent::NeedsConfirm { prompt, reply_tx } => {
            finalize_streaming(app);
            app.status = AppStatus::Confirming { prompt, reply_tx };
        }
        TuiEvent::AssistantText(text) => {
            finalize_streaming(app);
            app.push_entry(EntryKind::Assistant, &replace_latex(text));
            app.auto_scroll = true;
        }
        TuiEvent::TurnDone(new_msgs) => {
            finalize_streaming(app);
            app.messages = new_msgs;
            app.status = AppStatus::Idle;
            app.auto_scroll = true;
            // Plan mode is local to each agent turn; reset the display so the
            // next turn starts with the correct title bar.
            app.plan_mode = false;
        }
        TuiEvent::TurnError(err) => {
            app.streaming = None;
            // Roll back the user message we pushed before dispatch.
            app.messages.pop();
            // Remove the matching User display entry and everything after it.
            if let Some(idx) = app
                .entries
                .iter()
                .rposition(|e| matches!(e.kind, EntryKind::User))
            {
                app.entries.truncate(idx);
            }
            app.push_entry(EntryKind::Error, &err);
            app.status = AppStatus::Idle;
        }
        TuiEvent::PlanModeChanged(on) => {
            app.plan_mode = on;
            let msg = if on {
                "Plan mode ON — tools restricted to read-only."
            } else {
                "Plan mode OFF — all tools available."
            };
            app.push_info(msg);
        }
    }
}

/// Replace LaTeX math notation with Unicode equivalents.
/// Local models occasionally emit LaTeX ($\rightarrow$, $\leq$, etc.) even in
/// plain-text contexts; this makes the output readable in a terminal.
fn replace_latex(mut s: String) -> String {
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

fn finalize_streaming(app: &mut App) {
    if let Some(text) = app.streaming.take() {
        app.entries.push(ChatEntry {
            kind: EntryKind::Assistant,
            text: replace_latex(text),
        });
    }
}

// ── Model task ────────────────────────────────────────────────────────────────

const MAX_AGENT_ITERATIONS: usize = 20;

/// Tools allowed when plan mode is active (read-only subset).
const PLAN_MODE_ALLOWED: &[&str] = &[
    "read_file",
    "read_file_range",
    "read_many_files",
    "search_files",
    "grep_files",
    "list_directory",
    "get_working_directory",
    "fetch_url",
    "web_search",
    "lsp",
    "exit_plan_mode",
];

async fn run_model_task(
    client: LlamaClient,
    mut msgs: Vec<Message>,
    temp: f32,
    tools: Vec<ToolDef>,
    executor: Option<ToolExecutor>,
    tx: mpsc::UnboundedSender<TuiEvent>,
) {
    let result = if let Some(exec) = &executor {
        run_agent_loop(&client, &mut msgs, temp, &tools, exec, &tx).await
    } else {
        run_stream_turn(&client, &mut msgs, temp, &tx).await
    };

    let ev = match result {
        Ok(()) => TuiEvent::TurnDone(msgs),
        Err(e) => TuiEvent::TurnError(e.to_string()),
    };
    let _ = tx.send(ev);
}

async fn run_stream_turn(
    client: &LlamaClient,
    msgs: &mut Vec<Message>,
    temp: f32,
    tx: &mpsc::UnboundedSender<TuiEvent>,
) -> Result<()> {
    let tx_clone = tx.clone();
    let full_text = client
        .chat_stream_cb(msgs, temp, move |token| {
            let _ = tx_clone.send(TuiEvent::StreamToken(token.to_string()));
        })
        .await?;
    msgs.push(Message::assistant(&full_text));
    Ok(())
}

/// Cap a tool result at `limit` characters.
/// The truncation message instructs the model to use read_file_range
/// rather than leaving it confused about partial content.
fn cap_tool_result(result: String, limit: usize) -> String {
    if result.len() <= limit {
        return result;
    }
    // Truncate at a char boundary.
    let cut = result
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i < limit)
        .last()
        .unwrap_or(limit);
    format!(
        "{}\n[Output truncated at {cut} chars. \
         Use read_file_range with explicit line numbers to read specific sections.]",
        &result[..cut]
    )
}

async fn run_agent_loop(
    client: &LlamaClient,
    msgs: &mut Vec<Message>,
    temp: f32,
    tools: &[ToolDef],
    executor: &ToolExecutor,
    tx: &mpsc::UnboundedSender<TuiEvent>,
) -> Result<()> {
    let mut planning_mode = false;
    // Built lazily on first enter_plan_mode; reused for all subsequent iterations.
    let mut plan_mode_tools: Option<Vec<ToolDef>> = None;

    for _ in 0..MAX_AGENT_ITERATIONS {
        // In plan mode, filter the tool list to read-only operations only.
        // `plan_mode_tools` is computed at most once per session.
        let tools_for_call: &[ToolDef] = if planning_mode {
            plan_mode_tools.get_or_insert_with(|| {
                tools
                    .iter()
                    .filter(|t| PLAN_MODE_ALLOWED.contains(&t.function.name))
                    .cloned()
                    .collect()
            })
        } else {
            tools
        };

        // Some local models (e.g. Gemma4 with peg-gemma4 template) emit EOS
        // immediately when the last message has role "tool".  Append a temporary
        // user nudge to the outgoing slice so the model understands it should
        // produce a response.  We do NOT push it into `msgs` so it never
        // appears in the persistent conversation history.
        let nudged: Vec<Message>;
        let msgs_to_send: &[Message] = if msgs.last().map(|m| m.role.as_str()) == Some("tool") {
            nudged = {
                let mut v = msgs.to_vec();
                v.push(Message::user(
                    "Tool result received. Continue the task; call more tools if needed.",
                ));
                v
            };
            &nudged
        } else {
            msgs
        };

        // Some local models occasionally return an empty response (no content,
        // no tool calls).  Retry a few times before surfacing the error.
        let turn = {
            const MAX_EMPTY_RETRIES: usize = 3;
            let mut last_err = anyhow::anyhow!("unreachable");
            let mut turn_opt = None;
            for attempt in 0..MAX_EMPTY_RETRIES {
                match client.chat_agent(msgs_to_send, temp, tools_for_call).await {
                    Ok(t) => {
                        turn_opt = Some(t);
                        break;
                    }
                    Err(e) if e.to_string().contains("no content and no tool calls") => {
                        last_err = e;
                        if attempt + 1 < MAX_EMPTY_RETRIES {
                            let _ = tx.send(TuiEvent::ToolStart(format!(
                                "empty response, retrying ({}/{MAX_EMPTY_RETRIES})…",
                                attempt + 1
                            )));
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
            match turn_opt {
                Some(t) => t,
                None => return Err(last_err),
            }
        };
        match turn {
            AgentTurn::Text(text) => {
                let _ = tx.send(TuiEvent::AssistantText(text.clone()));
                msgs.push(Message::assistant(&text));
                return Ok(());
            }
            AgentTurn::ToolCalls(calls) => {
                msgs.push(Message::assistant_tool_calls(calls.clone()));

                for call in &calls {
                    // Display the call.
                    let label = fmt_call(call);
                    let _ = tx.send(TuiEvent::ToolStart(label));

                    // Handle plan mode control calls without going through the executor.
                    if call.function.name == "enter_plan_mode" {
                        planning_mode = true;
                        let _ = tx.send(TuiEvent::PlanModeChanged(true));
                        let _ = tx.send(TuiEvent::ToolDone("Plan mode activated.".to_string()));
                        msgs.push(Message::tool_result(
                            &call.id,
                            "Plan mode activated. You can now read files and explore the \
                             codebase. Write tools are disabled until you call exit_plan_mode.",
                        ));
                        continue;
                    }
                    if call.function.name == "exit_plan_mode" {
                        planning_mode = false;
                        let _ = tx.send(TuiEvent::PlanModeChanged(false));
                        let _ = tx.send(TuiEvent::ToolDone("Plan mode deactivated.".to_string()));
                        msgs.push(Message::tool_result(
                            &call.id,
                            "Plan mode deactivated. All tools are now available.",
                        ));
                        continue;
                    }

                    // In plan mode, block any write tools the model might try to call.
                    if planning_mode && !PLAN_MODE_ALLOWED.contains(&call.function.name.as_str()) {
                        let _ = tx.send(TuiEvent::ToolDone(
                            "Blocked — write tools disabled in plan mode.".to_string(),
                        ));
                        msgs.push(Message::tool_result(
                            &call.id,
                            "Error: write tools are disabled in plan mode. \
                             Call exit_plan_mode first.",
                        ));
                        continue;
                    }

                    // Confirm if needed.
                    if needs_confirm(call, executor) {
                        let prompt = fmt_confirm_prompt(call);
                        let (reply_tx, reply_rx) = oneshot::channel::<bool>();
                        let _ = tx.send(TuiEvent::NeedsConfirm { prompt, reply_tx });
                        let allowed = reply_rx.await.unwrap_or(false);
                        if !allowed {
                            // User denied — abort the entire turn so the model
                            // cannot loop back and retry the same operation.
                            let _ = tx.send(TuiEvent::ToolDone("Denied by user.".to_string()));
                            let _ = tx.send(TuiEvent::TurnDone(msgs.clone()));
                            return Ok(());
                        }
                    }

                    // Execute the tool (use spawn_blocking to avoid blocking the async runtime).
                    let result = {
                        let exec = ToolExecutor {
                            confirm_writes: false,
                            confirm_shell: false,
                            lsp: executor.lsp.clone(),
                            max_tool_result_chars: executor.max_tool_result_chars,
                        };
                        let call2 = call.clone();
                        tokio::task::spawn_blocking(move || exec.execute_quiet(&call2))
                            .await
                            .unwrap_or_else(|e| format!("internal error: {e}"))
                    };

                    // Show a one-line preview of the result.
                    let preview = result
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(120)
                        .collect::<String>();
                    let _ = tx.send(TuiEvent::ToolDone(preview));

                    msgs.push(Message::tool_result(
                        &call.id,
                        cap_tool_result(result, executor.max_tool_result_chars),
                    ));
                }
            }
        }
    }

    anyhow::bail!("agent exceeded {MAX_AGENT_ITERATIONS} iterations without a text response")
}

fn needs_confirm(call: &ToolCallItem, exec: &ToolExecutor) -> bool {
    match call.function.name.as_str() {
        "write_file" | "patch_file" | "delete_file" | "move_file" => exec.confirm_writes,
        "run_shell" => exec.confirm_shell,
        _ => false,
    }
}

fn fmt_confirm_prompt(call: &ToolCallItem) -> String {
    let args: serde_json::Value =
        serde_json::from_str(&call.function.arguments).unwrap_or_default();
    match call.function.name.as_str() {
        "write_file" => format!("Write to {}?", args["path"].as_str().unwrap_or("?")),
        "patch_file" => format!("Patch {}?", args["path"].as_str().unwrap_or("?")),
        "delete_file" => format!("Delete {}?", args["path"].as_str().unwrap_or("?")),
        "move_file" => format!(
            "Move {} → {}?",
            args["src"].as_str().unwrap_or("?"),
            args["dst"].as_str().unwrap_or("?"),
        ),
        "run_shell" => format!("Run: {}?", args["command"].as_str().unwrap_or("?")),
        name => format!("Execute {name}?"),
    }
}

fn fmt_call(call: &ToolCallItem) -> String {
    let args: serde_json::Value =
        serde_json::from_str(&call.function.arguments).unwrap_or_default();
    let name = &call.function.name;
    if let Some(map) = args.as_object() {
        let parts: Vec<String> = map
            .iter()
            .take(2)
            .filter_map(|(k, v)| {
                v.as_str().map(|s| {
                    let s: String = s.chars().take(60).collect();
                    format!("{k}=\"{s}\"")
                })
            })
            .collect();
        format!("{name}({})", parts.join(", "))
    } else {
        name.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ToolCallFunction;
    use crate::tools::DEFAULT_MAX_TOOL_RESULT_CHARS;

    // ── char_start_before ─────────────────────────────────────────────────────

    #[test]
    fn char_start_before_ascii() {
        let s = "hello";
        assert_eq!(char_start_before(s, 3), 2);
        assert_eq!(char_start_before(s, 1), 0);
        assert_eq!(char_start_before(s, 0), 0);
    }

    #[test]
    fn char_start_before_multibyte() {
        // "é" is 2 bytes (0xC3 0xA9); cursor at byte 2 (past é) should land at 0
        let s = "é";
        assert_eq!(s.len(), 2);
        assert_eq!(char_start_before(s, 2), 0);
    }

    // ── char_end_at ───────────────────────────────────────────────────────────

    #[test]
    fn char_end_at_ascii() {
        let s = "hello";
        assert_eq!(char_end_at(s, 0), 1);
        assert_eq!(char_end_at(s, 4), 5);
        assert_eq!(char_end_at(s, 5), 5); // at end
    }

    #[test]
    fn char_end_at_multibyte() {
        let s = "é!"; // é = 2 bytes, ! = 1 byte
        assert_eq!(char_end_at(s, 0), 2); // advances past 2-byte char
        assert_eq!(char_end_at(s, 2), 3); // advances past !
    }

    // ── prev_word ─────────────────────────────────────────────────────────────

    #[test]
    fn prev_word_from_end_of_word() {
        let s = "hello world";
        assert_eq!(prev_word(s, 11), 6); // from end → start of "world"
    }

    #[test]
    fn prev_word_skips_trailing_spaces() {
        let s = "hello   ";
        assert_eq!(prev_word(s, 8), 0); // skips spaces then word
    }

    #[test]
    fn prev_word_at_start() {
        assert_eq!(prev_word("hello", 0), 0);
    }

    // ── next_word ─────────────────────────────────────────────────────────────

    #[test]
    fn next_word_from_start_of_word() {
        let s = "hello world";
        assert_eq!(next_word(s, 0), 6); // skips "hello" then space → 6
    }

    #[test]
    fn next_word_at_end() {
        let s = "hello";
        assert_eq!(next_word(s, 5), 5);
    }

    // ── split_path ────────────────────────────────────────────────────────────

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

    // ── expand_skill_prompt ───────────────────────────────────────────────────

    #[test]
    fn expand_skill_prompt_replaces_args_placeholder() {
        let out = expand_skill_prompt("Review: {args}", "src/main.rs");
        assert_eq!(out, "Review: src/main.rs");
    }

    #[test]
    fn expand_skill_prompt_appends_args_when_no_placeholder() {
        let out = expand_skill_prompt("Write a commit message.", "for auth module");
        assert_eq!(out, "Write a commit message. for auth module");
    }

    #[test]
    fn expand_skill_prompt_no_args_no_placeholder() {
        let out = expand_skill_prompt("Write a commit message.", "");
        assert_eq!(out, "Write a commit message.");
    }

    #[test]
    fn expand_skill_prompt_trims_result_when_args_empty() {
        // {args} replaced by "" leaves trailing whitespace; trim cleans it.
        let out = expand_skill_prompt("Review: {args}", "");
        assert_eq!(out, "Review:");
    }

    #[test]
    fn expand_skill_prompt_multiple_placeholders() {
        let out = expand_skill_prompt("Do {args} then do {args}", "foo");
        assert_eq!(out, "Do foo then do foo");
    }

    // ── line_starts ───────────────────────────────────────────────────────────

    #[test]
    fn line_starts_empty_string() {
        assert_eq!(line_starts(""), vec![0]);
    }

    #[test]
    fn line_starts_single_line() {
        assert_eq!(line_starts("hello"), vec![0]);
    }

    #[test]
    fn line_starts_two_lines() {
        assert_eq!(line_starts("hello\nworld"), vec![0, 6]);
    }

    #[test]
    fn line_starts_three_lines() {
        assert_eq!(line_starts("a\nb\nc"), vec![0, 2, 4]);
    }

    #[test]
    fn line_starts_trailing_newline() {
        // "hi\n" has a final empty line starting at byte 3
        assert_eq!(line_starts("hi\n"), vec![0, 3]);
    }

    // ── cursor_line_col ───────────────────────────────────────────────────────

    #[test]
    fn cursor_line_col_single_line() {
        assert_eq!(cursor_line_col("hello", 0), (0, 0));
        assert_eq!(cursor_line_col("hello", 3), (0, 3));
        assert_eq!(cursor_line_col("hello", 5), (0, 5));
    }

    #[test]
    fn cursor_line_col_multiline_first_line() {
        let s = "hello\nworld";
        assert_eq!(cursor_line_col(s, 0), (0, 0));
        assert_eq!(cursor_line_col(s, 4), (0, 4));
        // byte 5 is the '\n' itself — still on line 0
        assert_eq!(cursor_line_col(s, 5), (0, 5));
    }

    #[test]
    fn cursor_line_col_multiline_second_line() {
        let s = "hello\nworld";
        // byte 6 is the start of "world"
        assert_eq!(cursor_line_col(s, 6), (1, 0));
        assert_eq!(cursor_line_col(s, 9), (1, 3));
        assert_eq!(cursor_line_col(s, 11), (1, 5));
    }

    #[test]
    fn cursor_line_col_three_lines() {
        let s = "a\nb\nc";
        assert_eq!(cursor_line_col(s, 0), (0, 0)); // 'a'
        assert_eq!(cursor_line_col(s, 2), (1, 0)); // 'b'
        assert_eq!(cursor_line_col(s, 4), (2, 0)); // 'c'
        assert_eq!(cursor_line_col(s, 5), (2, 1)); // end of 'c'
    }

    // ── fence_prefix_len ──────────────────────────────────────────────────────

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
        assert_eq!(fence_prefix_len("``"), 0); // only 2 backticks
        assert_eq!(fence_prefix_len("hello"), 0);
        assert_eq!(fence_prefix_len("``~"), 0); // mixed chars
        assert_eq!(fence_prefix_len("`~`"), 0);
    }

    // ── code_line_bg ──────────────────────────────────────────────────────────

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
        // Lines without a leading +/- are unchanged context — same bg as regular code.
        assert_eq!(code_line_bg(" context line", true), CODE_BG);
        assert_eq!(code_line_bg("plain", true), CODE_BG);
    }

    // ── fmt_confirm_prompt ────────────────────────────────────────────────────

    fn make_call(name: &str, args: &str) -> ToolCallItem {
        ToolCallItem {
            id: "test-id".to_string(),
            kind: "function".to_string(),
            function: crate::client::ToolCallFunction {
                name: name.to_string(),
                arguments: args.to_string(),
            },
        }
    }

    #[test]
    fn fmt_confirm_prompt_write_file() {
        let call = make_call("write_file", r#"{"path":"src/main.rs"}"#);
        assert_eq!(fmt_confirm_prompt(&call), "Write to src/main.rs?");
    }

    #[test]
    fn fmt_confirm_prompt_patch_file() {
        let call = make_call("patch_file", r#"{"path":"README.md"}"#);
        assert_eq!(fmt_confirm_prompt(&call), "Patch README.md?");
    }

    #[test]
    fn fmt_confirm_prompt_delete_file() {
        let call = make_call("delete_file", r#"{"path":"old.txt"}"#);
        assert_eq!(fmt_confirm_prompt(&call), "Delete old.txt?");
    }

    #[test]
    fn fmt_confirm_prompt_move_file() {
        let call = make_call("move_file", r#"{"src":"a.txt","dst":"b.txt"}"#);
        assert_eq!(fmt_confirm_prompt(&call), "Move a.txt → b.txt?");
    }

    #[test]
    fn fmt_confirm_prompt_run_shell() {
        let call = make_call("run_shell", r#"{"command":"rm -rf /tmp/test"}"#);
        assert_eq!(fmt_confirm_prompt(&call), "Run: rm -rf /tmp/test?");
    }

    #[test]
    fn fmt_confirm_prompt_unknown_tool_falls_back_to_name() {
        let call = make_call("my_custom_tool", "{}");
        assert_eq!(fmt_confirm_prompt(&call), "Execute my_custom_tool?");
    }

    #[test]
    fn fmt_confirm_prompt_missing_path_field_shows_question_mark() {
        let call = make_call("write_file", "{}");
        assert_eq!(fmt_confirm_prompt(&call), "Write to ??");
    }

    // ── fmt_call ──────────────────────────────────────────────────────────────

    #[test]
    fn fmt_call_shows_function_name_and_first_two_string_args() {
        let call = make_call("read_file", r#"{"path":"src/lib.rs"}"#);
        assert!(fmt_call(&call).starts_with("read_file("));
        assert!(fmt_call(&call).contains(r#"path="src/lib.rs""#));
    }

    #[test]
    fn fmt_call_truncates_long_arg_values_at_60_chars() {
        let long = "x".repeat(80);
        let args = format!(r#"{{"path":"{long}"}}"#);
        let out = fmt_call(&make_call("write_file", &args));
        // The displayed path value must be capped at 60 chars.
        let value_part = out.split('"').nth(3).unwrap_or("");
        assert!(value_part.len() <= 60, "value not truncated: {value_part}");
    }

    #[test]
    fn fmt_call_no_string_args_shows_just_name() {
        // Arguments has only non-string (numeric) values — filtered out.
        let call = make_call("set_timeout", r#"{"ms":500}"#);
        assert_eq!(fmt_call(&call), "set_timeout()");
    }

    #[test]
    fn fmt_call_empty_args_shows_just_name() {
        let call = make_call("list_tools", "{}");
        assert_eq!(fmt_call(&call), "list_tools()");
    }

    #[test]
    fn fmt_call_invalid_json_falls_back_to_name() {
        let call = make_call("broken_tool", "not json");
        assert_eq!(fmt_call(&call), "broken_tool");
    }

    // ── replace_latex ─────────────────────────────────────────────────────────

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

    // ── trim_to_budget ────────────────────────────────────────────────────────

    fn sys() -> Message {
        Message::system("system prompt")
    }
    fn usr(s: &str) -> Message {
        Message::user(s)
    }
    fn ast(s: &str) -> Message {
        Message::assistant(s)
    }

    #[test]
    fn trim_to_budget_no_op_when_within_budget() {
        let mut msgs = vec![sys(), usr("hello"), ast("hi")];
        let dropped = trim_to_budget(&mut msgs, 1000);
        assert_eq!(dropped, 0);
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn trim_to_budget_drops_oldest_non_system_first() {
        // "hello" (5 bytes) + "world" (5 bytes) = 10 total content bytes.
        // Budget of 6 forces one drop; the system prompt content "system prompt"
        // (13 bytes) alone already exceeds 6, but msgs.len() stops at 1.
        let mut msgs = vec![sys(), usr("hello"), ast("world")];
        // Budget that fits "world" but not "hello" + "world".
        let dropped = trim_to_budget(&mut msgs, 5);
        // Should drop usr("hello") first (index 1).
        assert!(dropped >= 1);
        assert!(!msgs.iter().any(|m| m.content.as_deref() == Some("hello")));
    }

    #[test]
    fn trim_to_budget_always_keeps_system_prompt() {
        let mut msgs = vec![sys()];
        let dropped = trim_to_budget(&mut msgs, 0);
        assert_eq!(dropped, 0);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "system");
    }

    #[test]
    fn trim_to_budget_drops_multiple_messages() {
        let mut msgs = vec![sys(), usr("aaaa"), ast("bbbb"), usr("cccc"), ast("dddd")];
        // Budget of 4 bytes: only one message's content fits; drop until we
        // can't drop any more (len == 1) since system prompt alone is 13 bytes.
        let dropped = trim_to_budget(&mut msgs, 4);
        assert_eq!(dropped, 4);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn trim_to_budget_returns_correct_count() {
        let mut msgs = vec![sys(), usr("aa"), ast("bb"), usr("cc")];
        // Total = 13 (sys) + 2 + 2 + 2 = 19 bytes.
        // Budget = 15: drop usr("aa") → 17, drop ast("bb") → 15 ≤ 15, stop.
        let dropped = trim_to_budget(&mut msgs, 15);
        assert_eq!(dropped, 2);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn trim_to_budget_skips_messages_with_no_content() {
        // tool_call messages have content = None; their byte cost is 0.
        let no_content = Message {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![]),
            tool_call_id: None,
        };
        let mut msgs = vec![sys(), no_content, usr("hello")];
        let dropped = trim_to_budget(&mut msgs, 1000);
        assert_eq!(dropped, 0);
    }

    // ── cap_tool_result ───────────────────────────────────────────────────────

    #[test]
    fn cap_tool_result_passthrough_when_short() {
        let s = "hello".to_string();
        assert_eq!(cap_tool_result(s.clone(), DEFAULT_MAX_TOOL_RESULT_CHARS), s);
    }

    #[test]
    fn cap_tool_result_truncates_long_result() {
        let limit = DEFAULT_MAX_TOOL_RESULT_CHARS;
        let long = "a".repeat(limit + 100);
        let out = cap_tool_result(long, limit);
        assert!(out.len() < limit + 200);
        assert!(out.contains("[Output truncated"));
        assert!(out.contains("read_file_range"));
    }

    #[test]
    fn cap_tool_result_at_exact_limit_is_not_truncated() {
        let limit = DEFAULT_MAX_TOOL_RESULT_CHARS;
        let exact = "b".repeat(limit);
        let out = cap_tool_result(exact.clone(), limit);
        assert_eq!(out, exact);
    }

    #[test]
    fn cap_tool_result_handles_multibyte_chars() {
        // Each '→' is 3 bytes; fill to just above the byte limit.
        let limit = DEFAULT_MAX_TOOL_RESULT_CHARS;
        let arrow = "→".repeat(limit / 3 + 10);
        let out = cap_tool_result(arrow, limit);
        // Must not panic and must be valid UTF-8.
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        assert!(out.contains("[Output truncated"));
    }

    #[test]
    fn cap_tool_result_respects_custom_limit() {
        // A larger limit should pass through content that would be truncated at
        // the default limit.
        let large_limit = DEFAULT_MAX_TOOL_RESULT_CHARS * 4; // e.g. ctx=32K scenario
        let content = "x".repeat(DEFAULT_MAX_TOOL_RESULT_CHARS + 1000);
        let out = cap_tool_result(content.clone(), large_limit);
        assert_eq!(
            out, content,
            "content within large_limit should not be truncated"
        );
    }

    // ── needs_confirm ─────────────────────────────────────────────────────────

    fn make_call_for_confirm(name: &str) -> ToolCallItem {
        ToolCallItem {
            id: "x".into(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: name.into(),
                arguments: "{}".into(),
            },
        }
    }

    fn exec(confirm_writes: bool, confirm_shell: bool) -> ToolExecutor {
        ToolExecutor {
            confirm_writes,
            confirm_shell,
            ..Default::default()
        }
    }

    #[test]
    fn needs_confirm_write_file_follows_confirm_writes() {
        assert!(needs_confirm(
            &make_call_for_confirm("write_file"),
            &exec(true, false)
        ));
        assert!(!needs_confirm(
            &make_call_for_confirm("write_file"),
            &exec(false, false)
        ));
    }

    #[test]
    fn needs_confirm_patch_and_delete_and_move_follow_confirm_writes() {
        for name in &["patch_file", "delete_file", "move_file"] {
            assert!(needs_confirm(
                &make_call_for_confirm(name),
                &exec(true, false)
            ));
            assert!(!needs_confirm(
                &make_call_for_confirm(name),
                &exec(false, false)
            ));
        }
    }

    #[test]
    fn needs_confirm_run_shell_follows_confirm_shell() {
        assert!(needs_confirm(
            &make_call_for_confirm("run_shell"),
            &exec(false, true)
        ));
        assert!(!needs_confirm(
            &make_call_for_confirm("run_shell"),
            &exec(false, false)
        ));
    }

    #[test]
    fn needs_confirm_read_file_never_requires_confirmation() {
        assert!(!needs_confirm(
            &make_call_for_confirm("read_file"),
            &exec(true, true)
        ));
    }

    #[test]
    fn needs_confirm_unknown_tool_never_requires_confirmation() {
        assert!(!needs_confirm(
            &make_call_for_confirm("web_search"),
            &exec(true, true)
        ));
    }

    // ── entry_style ───────────────────────────────────────────────────────────

    #[test]
    fn entry_style_all_prefixes_are_seven_chars_wide() {
        for kind in [
            EntryKind::User,
            EntryKind::Assistant,
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
            EntryKind::ToolCall,
            EntryKind::ToolResult,
            EntryKind::Info,
            EntryKind::Error,
        ];
        let prefixes: Vec<_> = kinds.iter().map(|&k| entry_style(k).0).collect();
        // All prefixes must be unique.
        let unique: std::collections::HashSet<_> = prefixes.iter().collect();
        assert_eq!(unique.len(), prefixes.len());
    }
}
