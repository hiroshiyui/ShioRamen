// SPDX-License-Identifier: GPL-3.0-or-later
use std::io;

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
use tokio::sync::{mpsc, oneshot};

use crate::chat::ChatSession;
use crate::client::{AgentTurn, LlamaClient, Message, ToolCallItem, ToolDef};
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
}

// ── Chat display ──────────────────────────────────────────────────────────────

struct ChatEntry {
    kind: EntryKind,
    text: String,
}

#[derive(Clone, Copy)]
enum EntryKind {
    User,
    Assistant,
    ToolCall,
    ToolResult,
    Info,
    Error,
}

fn entry_style(kind: EntryKind) -> (&'static str, &'static str, Style) {
    // Returns (first-line prefix, continuation indent, style)
    // All prefixes are 7 characters wide for consistent alignment.
    match kind {
        EntryKind::User => ("  you> ", "       ", Style::default().fg(Color::Cyan)),
        EntryKind::Assistant => (" shio> ", "       ", Style::default()),
        EntryKind::ToolCall => ("  [**] ", "       ", Style::default().fg(Color::Yellow)),
        EntryKind::ToolResult => (
            "  [-›] ",
            "       ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::DIM),
        ),
        EntryKind::Info => ("  [--] ", "       ", Style::default().fg(Color::DarkGray)),
        EntryKind::Error => ("  [!!] ", "       ", Style::default().fg(Color::Red)),
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

    status: AppStatus,
    anim_frame: u8,    // cycles 0-2 while Waiting, drives the thinking animation
    select_mode: bool, // when true, mouse capture is disabled so the terminal can select text
    quit: bool,
}

enum AppStatus {
    Idle,
    Waiting,
    Confirming {
        prompt: String,
        reply_tx: oneshot::Sender<bool>,
    },
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
        status: AppStatus::Idle,
        anim_frame: 0,
        select_mode: false,
        quit: false,
    };

    let welcome = if has_tools {
        "ShioRamen ready — tool use ON.  /reset /include <path> /tools /exit   PgUp/Dn to scroll"
    } else {
        "ShioRamen ready.  /reset /include <path> /exit   PgUp/Dn to scroll"
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

    Ok(())
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn render(f: &mut Frame, app: &App) {
    let area = f.area();

    let chunks = Layout::vertical([
        Constraint::Length(1), // title bar
        Constraint::Fill(1),   // messages
        Constraint::Length(1), // status line
        Constraint::Length(2), // input (top-border + 1 line)
    ])
    .split(area);

    // ── Title bar ──────────────────────────────────────────────────────────────
    let mode = if app.executor.is_some() {
        "tools:ON"
    } else {
        "tools:OFF"
    };
    let title_str = format!(
        " ShioRamen  [{mode}]  [Tab] complete  [PgUp/Dn] scroll  [F2] select  [Ctrl+C] quit"
    );
    f.render_widget(
        Paragraph::new(title_str).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
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
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                (String::new(), Style::default())
            }
        }
        AppStatus::Waiting => {
            const FRAMES: &[&str] = &["🤔.", "🤔..", "🤔..."];
            let frame = FRAMES[app.anim_frame as usize % FRAMES.len()];
            (format!("  {frame}"), Style::default().fg(Color::Yellow))
        }
        AppStatus::Confirming { prompt, .. } => (
            format!("  Confirm: {prompt}  [y/N]"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    };
    f.render_widget(Paragraph::new(status_text).style(status_style), chunks[2]);

    // ── Input area ─────────────────────────────────────────────────────────────
    let input_block = Block::default().borders(Borders::TOP);
    let inner = input_block.inner(chunks[3]);
    f.render_widget(input_block, chunks[3]);

    let prefix = "> ";
    f.render_widget(Paragraph::new(format!("{prefix}{}", app.input)), inner);

    // Draw cursor only while input is active.
    // Use display-column width (not byte offset) so CJK and other wide
    // characters — which are 2 terminal columns but 2–4 bytes — are handled
    // correctly.
    if matches!(app.status, AppStatus::Idle) {
        use unicode_width::UnicodeWidthStr;
        let display_col = app.input[..app.cursor].width() as u16;
        let cx = inner.x + prefix.len() as u16 + display_col;
        let cy = inner.y;
        if cx < inner.x + inner.width {
            f.set_cursor_position((cx, cy));
        }
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

fn push_entry_lines(out: &mut Vec<Line<'static>>, entry: &ChatEntry, width: usize) {
    let (first_prefix, cont_prefix, style) = entry_style(entry.kind);
    let pfx_width = first_prefix.len(); // all prefixes are exactly 7 bytes / 7 ASCII cols
    let text_width = width.saturating_sub(pfx_width).max(10);

    let wrapped = textwrap::wrap(&entry.text, text_width);
    for (i, segment) in wrapped.iter().enumerate() {
        let pfx: &str = if i == 0 { first_prefix } else { cont_prefix };
        out.push(Line::from(vec![
            Span::styled(pfx.to_string(), style),
            Span::raw(segment.to_string()),
        ]));
    }
}

// ── Key handling ──────────────────────────────────────────────────────────────

/// Returns `true` if the application should quit.
async fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    use KeyCode::*;
    use KeyModifiers as Mods;

    // In Confirming state: only y/n/Escape accepted.
    if let AppStatus::Confirming { .. } = &app.status {
        match key.code {
            Char('y') | Char('Y') => send_confirm(app, true),
            Char('n') | Char('N') | Esc | Enter => send_confirm(app, false),
            _ => {}
        }
        return false;
    }

    // While waiting for model: only Ctrl+C quits.
    if matches!(app.status, AppStatus::Waiting) {
        if key.code == Char('c') && key.modifiers.contains(Mods::CONTROL) {
            return true;
        }
        return false;
    }

    // ── Normal editing ────────────────────────────────────────────────────────
    match (key.code, key.modifiers) {
        // Quit
        (Char('c'), m) | (Char('d'), m) if m.contains(Mods::CONTROL) => return true,

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
            app.cursor = 0;
        }
        (End, _) => {
            app.cursor = app.input.len();
        }

        // Input history
        (Up, _) => hist_prev(app),
        (Down, _) => hist_next(app),

        // Scroll
        (PageUp, _) => view_scroll(app, -10),
        (PageDown, _) => view_scroll(app, 10),

        // Tab completion
        (Tab, _) => do_complete(app),

        // Select mode toggle
        (F(2), _) => toggle_select_mode(app),

        // Bash-style line editing
        (Char('a'), m) if m.contains(Mods::CONTROL) => {
            app.cursor = 0;
        }
        (Char('e'), m) if m.contains(Mods::CONTROL) => {
            app.cursor = app.input.len();
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
    let idx = app.hist_idx.unwrap();
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
    const SLASH_CMDS: &[&str] = &["/exit", "/quit", "/reset", "/include ", "/tools"];

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
            SLASH_CMDS
                .iter()
                .filter(|&&c| c.starts_with(typed.as_str()))
                .map(|&c| c.to_string())
                .collect()
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
            app.quit = true;
            return;
        }
        "/reset" => {
            app.messages.truncate(1);
            app.entries.clear();
            app.streaming = None;
            app.push_info("History cleared.");
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
        _ if input.starts_with("/include ") => {
            let path_str = input["/include ".len()..].trim();
            cmd_include(app, path_str);
            return;
        }
        _ => {}
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

fn dispatch_turn(app: &mut App) {
    let client = app.client.clone();
    let msgs = app.messages.clone();
    let temp = app.temperature;
    let tools = app.tools.clone();
    let executor = app.executor.clone();
    let tx = app.event_tx.clone();

    tokio::spawn(async move {
        run_model_task(client, msgs, temp, tools, executor, tx).await;
    });
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

async fn run_agent_loop(
    client: &LlamaClient,
    msgs: &mut Vec<Message>,
    temp: f32,
    tools: &[ToolDef],
    executor: &ToolExecutor,
    tx: &mpsc::UnboundedSender<TuiEvent>,
) -> Result<()> {
    for _ in 0..MAX_AGENT_ITERATIONS {
        match client.chat_agent(msgs, temp, tools).await? {
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

                    // Confirm if needed.
                    let should_run = if needs_confirm(call, executor) {
                        let prompt = fmt_confirm_prompt(call);
                        let (reply_tx, reply_rx) = oneshot::channel::<bool>();
                        let _ = tx.send(TuiEvent::NeedsConfirm { prompt, reply_tx });
                        reply_rx.await.unwrap_or(false)
                    } else {
                        true
                    };

                    // Execute the tool (use spawn_blocking to avoid blocking the async runtime).
                    let result = if should_run {
                        let exec = ToolExecutor {
                            confirm_writes: false,
                            confirm_shell: false,
                        };
                        let call2 = call.clone();
                        tokio::task::spawn_blocking(move || exec.execute_quiet(&call2))
                            .await
                            .unwrap_or_else(|e| format!("internal error: {e}"))
                    } else {
                        "Aborted by user.".to_string()
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

                    msgs.push(Message::tool_result(&call.id, result));
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
}
