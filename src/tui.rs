// SPDX-License-Identifier: GPL-3.0-or-later
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

use anyhow::Result;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEvent, KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::{mpsc, oneshot};

use crate::chat::ChatSession;
use crate::client::{AgentTurn, LlamaClient, Message, SamplingParams, ToolCallItem, ToolDef};
use crate::config::SkillDef;
use crate::context;
use crate::tools::ToolExecutor;

mod completion;
mod confirm;
mod context_budget;
mod input;
mod palette;
mod paste;
mod recording;
mod render;
mod skill;
mod supersede;
mod tool_result;
use completion::do_complete;
use confirm::{fmt_confirm_prompt, needs_confirm};
use context_budget::{context_used_pct, msg_size, trim_to_budget, trim_to_budget_before};
use input::{char_end_at, char_start_before, cursor_line_col, line_starts, next_word, prev_word};
use paste::{handle_paste, paste_clipboard};
use recording::{Recorder, default_recording_path};
use render::render;
use skill::expand_skill_prompt;
#[cfg(test)]
use supersede::{SUPERSEDE_STUB_SENTINEL, is_supersede_stub};
use supersede::{
    stub_oldest_tool_results_in_turn, supersede_prior_tool_for_key, supersede_spec_for,
};
use tool_result::{cap_tool_result, result_needs_chunk_nudge};

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
    /// Auto-compact finished; carries the summary text (Ok) or error (Err).
    AutoCompactDone(Result<String, String>),
    /// A background command (e.g. /stats, /model) finished and produced text
    /// to display.  `Ok` is rendered as an info entry, `Err` as an error.
    BackgroundInfo(Result<String, String>),
    /// `/include` finished walking the filesystem.  On success the handler
    /// pushes the file blocks into `app.messages` and emits a summary.
    IncludeResult(Result<IncludeOutcome, String>),
}

/// Result of a successful `/include` walk, prepared off the main loop so the
/// handler only has to push it onto the conversation.
struct IncludeOutcome {
    path_str: String,
    content: String,
    count: usize,
    total_bytes: usize,
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
    Thinking,
    ToolCall,
    ToolResult,
    Info,
    Error,
}

// ── App state ─────────────────────────────────────────────────────────────────

struct App {
    // Chat session state
    messages: Vec<Message>,
    tools: Vec<ToolDef>,
    executor: Option<ToolExecutor>,
    client: LlamaClient,
    sampling: SamplingParams,

    // Display
    entries: Vec<ChatEntry>,
    streaming: Option<String>, // assistant response being built token-by-token
    /// Live thinking text accumulating inside a `<think>…</think>` block.
    thinking: Option<String>,
    /// True while streaming tokens that belong inside a `<think>` block.
    in_think: bool,
    /// Raw token accumulator — held until `<think>`/`</think>` tag boundaries
    /// are resolved before routing to `streaming` or `thinking`.
    raw_buf: String,
    /// Whether to display `<think>` blocks (from config).
    show_thinking: bool,
    scroll: u32,
    auto_scroll: bool,

    // Input
    input: String,
    cursor: usize,
    history: Vec<String>,
    hist_idx: Option<usize>,
    saved: String, // input saved when browsing history

    /// Base64 data-URLs of images attached via Ctrl+V, sent with the next message.
    attached_images: Vec<String>,

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

    /// Active conversation recorder (`/record` … `/stop-record`).  When `Some`,
    /// every entry pushed into the chat log is mirrored to the file.  Dropped
    /// on exit, which flushes and closes the file.
    recording: Option<Recorder>,
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
        if let Some(rec) = self.recording.as_mut() {
            rec.write_entry(kind, text);
        }
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
    execute!(
        out,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(out);
    let mut term = Terminal::new(backend)?;

    let result = run_loop(&mut term, session).await;

    // Always restore terminal, even on error.
    disable_raw_mode()?;
    execute!(
        term.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
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
        sampling: session.sampling,
        entries: Vec::new(),
        streaming: None,
        thinking: None,
        in_think: false,
        raw_buf: String::new(),
        show_thinking: session.show_thinking,
        scroll: 0,
        auto_scroll: true,
        input: String::new(),
        cursor: 0,
        history: Vec::new(),
        hist_idx: None,
        saved: String::new(),
        attached_images: Vec::new(),
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
        recording: None,
    };

    let welcome = match (has_tools, has_skills) {
        (true, true) => {
            "ShioRamen ready — tool use ON.  /new /resume /clear /compact /stats /include <path> /tools /skills /record /stop-record /exit   PgUp/Dn to scroll"
        }
        (true, false) => {
            "ShioRamen ready — tool use ON.  /new /resume /clear /compact /stats /include <path> /tools /record /stop-record /exit   PgUp/Dn to scroll"
        }
        (false, true) => {
            "ShioRamen ready.  /new /resume /clear /compact /stats /include <path> /skills /record /stop-record /exit   PgUp/Dn to scroll"
        }
        (false, false) => {
            "ShioRamen ready.  /new /resume /clear /compact /stats /include <path> /record /stop-record /exit   PgUp/Dn to scroll"
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
                    app.quit = handle_key(&mut app, key).await;
                }
                Some(Ok(Event::Mouse(mouse))) => {
                    match mouse.kind {
                        MouseEventKind::ScrollUp   => view_scroll(&mut app, -3),
                        MouseEventKind::ScrollDown => view_scroll(&mut app,  3),
                        _ => {}
                    }
                }
                Some(Ok(Event::Paste(text))) => {
                    handle_paste(&mut app, text);
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

    // Auto-save session to disk (best-effort — don't fail the exit).
    if app.messages.len() > 1
        && let Ok(path) = crate::session::latest_path()
        && let Err(e) = crate::session::save(&app.messages, &path)
    {
        eprintln!("⚠️  Could not save session: {e}");
    }

    Ok(())
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

        // Newline: Alt+Enter, Shift+Enter, or Ctrl+J inserts a literal newline.
        (Enter, m) if m.contains(Mods::ALT) || m.contains(Mods::SHIFT) => {
            app.input.insert(app.cursor, '\n');
            app.cursor += 1;
            app.comp_candidates.clear();
        }
        (Char('j'), m) if m.contains(Mods::CONTROL) => {
            app.input.insert(app.cursor, '\n');
            app.cursor += 1;
            app.comp_candidates.clear();
        }
        // Submit — but if the line ends with `\`, replace it with a newline instead.
        (Enter, _) => {
            if app.cursor > 0 && app.input.as_bytes().get(app.cursor - 1) == Some(&b'\\') {
                app.input.replace_range(app.cursor - 1..app.cursor, "\n");
                // cursor stays at same byte offset (replacing 1 byte with 1 byte)
                app.comp_candidates.clear();
            } else {
                submit(app).await;
            }
        }

        // Delete
        (Backspace, _) if app.cursor > 0 => {
            let new = char_start_before(&app.input, app.cursor);
            app.input.drain(new..app.cursor);
            app.cursor = new;
            app.comp_candidates.clear();
        }
        (Delete, _) if app.cursor < app.input.len() => {
            let next = char_end_at(&app.input, app.cursor);
            app.input.drain(app.cursor..next);
            app.comp_candidates.clear();
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

        // Paste from clipboard (Ctrl+V): attach image or insert text.
        (Char('v'), m) if m.contains(Mods::CONTROL) => {
            paste_clipboard(app);
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

fn view_scroll(app: &mut App, delta: i32) {
    app.auto_scroll = false;
    app.scroll = (app.scroll as i64 + delta as i64).max(0) as u32;
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

// ── Input submission ──────────────────────────────────────────────────────────

async fn submit(app: &mut App) {
    let input = app.input.trim().to_string();
    if input.is_empty() && app.attached_images.is_empty() {
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
        "/new" => {
            app.messages.truncate(1);
            app.entries.clear();
            app.streaming = None;
            if let Ok(path) = crate::session::latest_path()
                && path.exists()
            {
                let _ = std::fs::remove_file(&path);
            }
            app.push_info("New session — history and saved session cleared.");
            return;
        }
        "/resume" => {
            cmd_resume(app);
            return;
        }
        "/compact" => {
            cmd_compact(app);
            return;
        }
        "/model" => {
            cmd_model(app);
            return;
        }
        "/stats" => {
            cmd_stats(app);
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
        "/record" => {
            cmd_record(app, None);
            return;
        }
        _ if input.starts_with("/record ") => {
            let path_str = input["/record ".len()..].trim();
            let custom = if path_str.is_empty() {
                None
            } else {
                Some(path_str.to_string())
            };
            cmd_record(app, custom);
            return;
        }
        "/stop-record" => {
            cmd_stop_record(app);
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

    // Regular user message (with optional images).
    app.push_entry(EntryKind::User, &input);
    if app.attached_images.is_empty() {
        app.messages.push(Message::user(&input));
    } else {
        let images = std::mem::take(&mut app.attached_images);
        app.messages.push(Message::user_with_images(input, images));
    }
    app.status = AppStatus::Waiting;
    app.anim_frame = 0;
    dispatch_turn(app);
}

fn cmd_record(app: &mut App, custom_path: Option<String>) {
    if let Some(rec) = app.recording.as_ref() {
        let p = rec.path.display().to_string();
        app.push_info(&format!(
            "Already recording to {p}. Use /stop-record to stop."
        ));
        return;
    }
    let path = match custom_path {
        Some(s) => PathBuf::from(s),
        None => match default_recording_path() {
            Ok(p) => p,
            Err(e) => {
                app.push_entry(EntryKind::Error, &format!("record: {e}"));
                return;
            }
        },
    };
    match Recorder::open(path) {
        Ok(rec) => {
            let p = rec.path.display().to_string();
            app.recording = Some(rec);
            app.push_info(&format!(
                "Recording started → {p}. Use /stop-record to stop; leaving chat also stops it."
            ));
        }
        Err(e) => app.push_entry(EntryKind::Error, &format!("record: {e:#}")),
    }
}

fn cmd_stop_record(app: &mut App) {
    match app.recording.take() {
        Some(rec) => {
            let p = rec.path.display().to_string();
            // BufWriter is dropped here; trailing buffered bytes flush on drop.
            drop(rec);
            app.push_info(&format!("Recording stopped → {p}"));
        }
        None => app.push_info("Not currently recording."),
    }
}

fn cmd_include(app: &mut App, path_str: &str) {
    // /include mutates app.messages, so it must serialize against any other
    // task that might also touch the conversation (model turn, /compact).
    if !matches!(app.status, AppStatus::Idle) {
        app.push_info("Busy — wait for the current task to finish, then retry /include.");
        return;
    }

    app.push_info(&format!("Including {path_str}…"));
    app.status = AppStatus::Waiting;
    app.anim_frame = 0;

    let path_str = path_str.to_string();
    let path_buf = std::path::PathBuf::from(&path_str);
    let tx = app.event_tx.clone();
    tokio::spawn(async move {
        // The walk + read happens on a blocking thread, but the surrounding
        // task is independent of the main event loop, so the TUI keeps drawing.
        let walk = tokio::task::spawn_blocking(move || context::collect(&path_buf)).await;
        let ev = match walk {
            Err(e) => TuiEvent::IncludeResult(Err(format!("include: {e}"))),
            Ok(Err(e)) => TuiEvent::IncludeResult(Err(format!("include: {e}"))),
            Ok(Ok(files)) => {
                let count = files.len();
                let total_bytes: usize = files.iter().map(|(_, c)| c.len()).sum();
                let content = context::format_as_blocks(&files);
                TuiEvent::IncludeResult(Ok(IncludeOutcome {
                    path_str,
                    content,
                    count,
                    total_bytes,
                }))
            }
        };
        let _ = tx.send(ev);
    });
}

fn cmd_stats(app: &mut App) {
    let client = app.client.clone();
    let tx = app.event_tx.clone();
    tokio::spawn(async move {
        let ev = match client.slots().await {
            Ok(slots) => {
                let lines: Vec<String> = slots
                    .iter()
                    .map(|s| {
                        let state = if s.is_processing { "busy" } else { "idle" };
                        let pct = (s.n_past * 100).checked_div(s.n_ctx).unwrap_or(0);
                        format!(
                            "slot {}: {state}  {}/{} tokens used ({}%)",
                            s.id, s.n_past, s.n_ctx, pct
                        )
                    })
                    .collect();
                TuiEvent::BackgroundInfo(Ok(lines.join("\n")))
            }
            Err(e) => TuiEvent::BackgroundInfo(Err(format!("stats: {e}"))),
        };
        let _ = tx.send(ev);
    });
}

fn cmd_resume(app: &mut App) {
    // Resume rewrites app.messages and app.entries; refuse if a model task or
    // earlier compact is still in flight.
    if !matches!(app.status, AppStatus::Idle) {
        app.push_info("Busy — wait for the current task to finish, then retry /resume.");
        return;
    }

    // File IO is local and tiny (a few KB JSON), so it stays on the main task
    // — no observable freeze and no need for the spawn/event dance.
    let path = match crate::session::find_latest() {
        Ok(Some(p)) => p,
        Ok(None) => {
            app.push_info("No saved session for this project.");
            return;
        }
        Err(e) => {
            app.push_entry(EntryKind::Error, &format!("resume: {e}"));
            return;
        }
    };
    let saved = match crate::session::load(&path) {
        Ok(m) => m,
        Err(e) => {
            app.push_entry(EntryKind::Error, &format!("resume: {e}"));
            return;
        }
    };

    // Discard the saved system prompt — keep the live one so config changes
    // (prompt style, AGENTS.md, etc.) take effect when resuming.
    let history: Vec<Message> = saved.into_iter().skip(1).collect();
    let count = history.len();

    // Replace history: live system prompt + every saved non-system message.
    let sys = app.messages[0].clone();
    app.messages.clear();
    app.messages.push(sys);

    // Replay user/assistant turns as visible display entries so the user can
    // see what they are resuming.  Tool calls and tool results are kept in
    // app.messages (so the model still has full context) but skipped here to
    // keep the recap concise.
    app.entries.clear();
    app.streaming = None;
    app.thinking = None;
    app.in_think = false;
    for m in &history {
        match m.role.as_str() {
            "user" => {
                if let Some(text) = m.text_content() {
                    app.push_entry(EntryKind::User, text);
                }
            }
            "assistant" => {
                if let Some(text) = m.text_content()
                    && !text.is_empty()
                {
                    app.push_entry(EntryKind::Assistant, &replace_latex(text.to_string()));
                }
            }
            _ => {}
        }
    }
    app.messages.extend(history);
    app.push_info(&format!(
        "Resumed {count} message(s) from {}",
        path.display()
    ));
    app.auto_scroll = true;
}

fn cmd_compact(app: &mut App) {
    // Need at least one non-system message to summarize.
    if app.messages.len() <= 1 {
        app.push_info("Nothing to compact — conversation is empty.");
        return;
    }

    // Refuse to start a compact while another async task (model turn or
    // earlier compact) is still in flight — otherwise we would race with it
    // and stack two AutoCompactDone events on the queue.
    if !matches!(app.status, AppStatus::Idle) {
        app.push_info("Busy — wait for the current task to finish, then retry /compact.");
        return;
    }

    app.push_info("Compacting conversation…");
    app.status = AppStatus::Waiting;
    app.anim_frame = 0;

    // Run the summarization off the main loop and deliver the result via
    // TuiEvent::AutoCompactDone, which the event handler already knows how
    // to process (same path as auto-compact).  This keeps the TUI responsive
    // — frames keep drawing, the spinner animates, scroll keys still work.
    let client = app.client.clone();
    let msgs = app.messages.clone();
    let sampling = app.sampling;
    let tx = app.event_tx.clone();
    tokio::spawn(async move {
        let mut summarize_msgs = msgs;
        summarize_msgs.push(Message::user(
            "Summarize our conversation so far in a concise but thorough way. \
             Preserve key decisions, file paths, code changes, and any pending \
             tasks. This summary will replace the conversation history to free \
             up context.",
        ));
        let result = client.chat_collect(&summarize_msgs, sampling).await;
        let ev = match result {
            Ok(text) => TuiEvent::AutoCompactDone(Ok(text)),
            Err(e) => TuiEvent::AutoCompactDone(Err(e.to_string())),
        };
        let _ = tx.send(ev);
    });
}

fn cmd_model(app: &mut App) {
    // Snapshot the inputs we need for the context-usage line so the background
    // task does not borrow `app`.  The percentage reflects state at the moment
    // the command was issued, which is what the user expects.
    let client = app.client.clone();
    let tx = app.event_tx.clone();
    let ctx_size = app.ctx_size;
    let ctx_pct = context_used_pct(&app.messages, &app.tools, app.ctx_size);
    tokio::spawn(async move {
        let ev = match client.props().await {
            Ok(props) => {
                let gs = &props.default_generation_settings;
                let model_name = if gs.model.is_empty() {
                    "(unknown)".to_string()
                } else {
                    gs.model.clone()
                };
                let ctx_line = if let Some(pct) = ctx_pct {
                    format!("{ctx_size} tokens ({pct}% used)")
                } else if ctx_size > 0 {
                    format!("{ctx_size} tokens")
                } else {
                    format!("{} tokens", gs.n_ctx)
                };
                let lines = [
                    format!("Model:          {model_name}"),
                    format!("Context:        {ctx_line}"),
                    format!("Slots:          {}", props.total_slots),
                    format!("Temperature:    {:.2}", gs.temperature),
                    format!("Top-p:          {:.2}", gs.top_p),
                    format!("Repeat penalty: {:.2}", gs.repeat_penalty),
                ];
                TuiEvent::BackgroundInfo(Ok(lines.join("\n")))
            }
            Err(e) => TuiEvent::BackgroundInfo(Err(format!("model: {e}"))),
        };
        let _ = tx.send(ev);
    });
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

fn dispatch_turn(app: &mut App) {
    // Safety-net trim: drop old messages only when very close to the context
    // ceiling (95%).  The normal path is auto-compact at 80% after TurnDone,
    // so this should rarely fire.
    if app.ctx_size > 0 {
        let tools_overhead = serde_json::to_string(&app.tools).map_or(0, |s| s.len());
        let budget = (app.ctx_size as usize * 4 * 95 / 100).saturating_sub(tools_overhead);
        let dropped = trim_to_budget(&mut app.messages, budget);
        if dropped > 0 {
            app.push_info(&format!(
                "Context limit critical — dropped {dropped} old message(s) from history."
            ));
        }
    }

    let client = app.client.clone();
    let msgs = app.messages.clone();
    let sampling = app.sampling;
    let tools = app.tools.clone();
    let executor = app.executor.clone();
    let tx = app.event_tx.clone();

    let ctx_size = app.ctx_size;
    app.model_task = Some(tokio::spawn(async move {
        run_model_task(client, msgs, sampling, tools, executor, tx, ctx_size).await;
    }));
}

// ── Model event handling ──────────────────────────────────────────────────────

fn handle_model_event(app: &mut App, ev: TuiEvent) {
    match ev {
        TuiEvent::StreamToken(token) => {
            app.raw_buf.push_str(&token);
            consume_raw_buf(
                &mut app.raw_buf,
                &mut app.in_think,
                &mut app.streaming,
                &mut app.thinking,
            );
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
            // When text was already streamed via StreamToken events,
            // finalize_streaming has already created the entry and text
            // arrives empty — skip the duplicate push.
            if !text.is_empty() {
                app.push_entry(EntryKind::Assistant, &replace_latex(text));
            }
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

            // Auto-compact when context usage is high.
            if let Some(pct) = context_used_pct(&app.messages, &app.tools, app.ctx_size)
                && pct >= 80
                && app.messages.len() > 2
            {
                app.push_info(&format!(
                    "Context {pct}% full — auto-compacting conversation…"
                ));
                app.status = AppStatus::Waiting;
                app.anim_frame = 0;
                let client = app.client.clone();
                let msgs = app.messages.clone();
                let sampling = app.sampling;
                let tx = app.event_tx.clone();
                tokio::spawn(async move {
                    let mut summarize_msgs = msgs;
                    summarize_msgs.push(Message::user(
                        "Summarize our conversation so far in a concise but thorough way. \
                         Preserve key decisions, file paths, code changes, and any pending \
                         tasks. This summary will replace the conversation history to free \
                         up context.",
                    ));
                    let result = client.chat_collect(&summarize_msgs, sampling).await;
                    let ev = match result {
                        Ok(text) => TuiEvent::AutoCompactDone(Ok(text)),
                        Err(e) => TuiEvent::AutoCompactDone(Err(e.to_string())),
                    };
                    let _ = tx.send(ev);
                });
            }
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
        TuiEvent::AutoCompactDone(result) => {
            match result {
                Ok(summary) => {
                    let sys = app.messages[0].clone();
                    app.messages.clear();
                    app.messages.push(sys);
                    app.messages.push(Message::user(format!(
                        "[Conversation compacted — summary of prior context]\n\n{summary}"
                    )));
                    app.messages.push(Message::assistant(
                        "Understood. I have the context from our previous conversation. \
                         How can I help?"
                            .to_string(),
                    ));
                    app.entries.clear();
                    let pct =
                        context_used_pct(&app.messages, &app.tools, app.ctx_size).unwrap_or(0);
                    app.push_info(&format!(
                        "Auto-compacted conversation. Context now {pct}% used."
                    ));
                }
                Err(e) => {
                    app.push_entry(EntryKind::Error, &format!("auto-compact failed: {e}"));
                }
            }
            app.status = AppStatus::Idle;
            app.auto_scroll = true;
        }
        TuiEvent::BackgroundInfo(result) => match result {
            Ok(text) => app.push_info(&text),
            Err(e) => app.push_entry(EntryKind::Error, &e),
        },
        TuiEvent::IncludeResult(result) => {
            match result {
                Err(e) => app.push_entry(EntryKind::Error, &e),
                Ok(outcome) if outcome.count == 0 => {
                    app.push_info(&format!("No source files found in {}", outcome.path_str));
                }
                Ok(outcome) => {
                    let IncludeOutcome {
                        path_str,
                        content,
                        count,
                        total_bytes,
                    } = outcome;
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
            app.status = AppStatus::Idle;
            app.auto_scroll = true;
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

/// Route tokens from `raw_buf` to `thinking` or `streaming` by splitting at
/// `<think>` / `</think>` tag boundaries.
///
/// Holds back up to `tag_len - 1` bytes at the tail so a tag split across two
/// consecutive tokens is always caught on the next call.
fn consume_raw_buf(
    raw_buf: &mut String,
    in_think: &mut bool,
    streaming: &mut Option<String>,
    thinking: &mut Option<String>,
) {
    const THINK_OPEN: &str = "<think>";
    const THINK_CLOSE: &str = "</think>";
    // Hold-back: tag length minus 1 so a partial tag at the buffer tail is
    // never flushed prematurely.
    const HOLD_OPEN: usize = THINK_OPEN.len() - 1; // 6
    const HOLD_CLOSE: usize = THINK_CLOSE.len() - 1; // 7

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
                    // Retreat to a char boundary so we don't slice mid-character.
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
                    // Retreat to a char boundary so we don't slice mid-character.
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

fn finalize_streaming(app: &mut App) {
    // Flush whatever is still buffered (no more tokens will arrive).
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

    // Thinking always precedes the response; push it first.
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
    sampling: SamplingParams,
    tools: Vec<ToolDef>,
    executor: Option<ToolExecutor>,
    tx: mpsc::UnboundedSender<TuiEvent>,
    ctx_size: u32,
) {
    let result = if let Some(exec) = &executor {
        run_agent_loop(&client, &mut msgs, sampling, &tools, exec, &tx, ctx_size).await
    } else {
        run_stream_turn(&client, &mut msgs, sampling, &tx).await
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
    sampling: SamplingParams,
    tx: &mpsc::UnboundedSender<TuiEvent>,
) -> Result<()> {
    let tx_clone = tx.clone();
    let full_text = client
        .chat_stream_cb(msgs, sampling, move |token| {
            let _ = tx_clone.send(TuiEvent::StreamToken(token.to_string()));
        })
        .await?;
    msgs.push(Message::assistant(&full_text));
    Ok(())
}

async fn run_agent_loop(
    client: &LlamaClient,
    msgs: &mut Vec<Message>,
    sampling: SamplingParams,
    tools: &[ToolDef],
    executor: &ToolExecutor,
    tx: &mpsc::UnboundedSender<TuiEvent>,
    ctx_size: u32,
) -> Result<()> {
    let mut planning_mode = false;
    // Built lazily on first enter_plan_mode; reused for all subsequent iterations.
    let mut plan_mode_tools: Option<Vec<ToolDef>> = None;

    // Index of the first message that belongs to this agent turn.
    // Messages before this point are cross-turn history that may be trimmed
    // if the context fills up; messages at or after it are never touched.
    let turn_start = msgs.len();

    for _ in 0..MAX_AGENT_ITERATIONS {
        // If accumulated tool results are pushing the history toward the context
        // ceiling, trim only the pre-turn cross-turn history.  This preserves
        // every tool result from the current turn so the model can keep reasoning
        // over them, while still making room to avoid truncated responses.
        // Budget: 85 % of ctx, estimated at 4 bytes per token,
        // minus serialized tool definitions (fixed per-request overhead).
        if ctx_size > 0 {
            let tools_overhead = serde_json::to_string(tools).map_or(0, |s| s.len());
            let budget = (ctx_size as usize * 4 * 85 / 100).saturating_sub(tools_overhead);
            trim_to_budget_before(msgs, budget, turn_start);
            // If current-turn tool history alone still exceeds budget,
            // stub the oldest tool_results from this turn.  Pre-turn trim
            // can't help here.  The newest tool_result is always preserved.
            stub_oldest_tool_results_in_turn(msgs, turn_start, budget);
        }
        // In plan mode, filter the tool list to read-only operations only.
        // `plan_mode_tools` is computed at most once per session.
        let tools_for_call: &[ToolDef] = if planning_mode {
            plan_mode_tools.get_or_insert_with(|| {
                tools
                    .iter()
                    .filter(|t| PLAN_MODE_ALLOWED.contains(&t.function.name.as_str()))
                    .cloned()
                    .collect()
            })
        } else {
            tools
        };

        // Some local models (e.g. Gemma4 with peg-gemma4 template) emit EOS
        // immediately when the last message has role "tool".  Many chat
        // templates also silently drop `role: "tool"` messages, leaving the
        // model unable to see what the tool returned.
        //
        // To work around both issues, replace trailing tool-result messages
        // and their preceding assistant tool-call message with a single user
        // message containing the results as plain text, then append a nudge.
        // We do NOT modify `msgs` — the rewritten version is only used for
        // this one request.
        let nudged: Vec<Message>;
        let msgs_to_send: &[Message] = if msgs.last().map(|m| m.role.as_str()) == Some("tool") {
            nudged = {
                let mut v = msgs.to_vec();
                // Pop trailing tool-result messages (and the assistant
                // tool-call message that precedes them) so we can re-emit
                // the content as a user message the model can always see.
                let mut tool_texts: Vec<String> = Vec::new();
                while v.last().map(|m| m.role.as_str()) == Some("tool") {
                    if let Some(m) = v.pop()
                        && let Some(t) = m.text_content()
                    {
                        tool_texts.push(t.to_string());
                    }
                }
                tool_texts.reverse();
                // Also pop the assistant tool-call message (content: None,
                // tool_calls: Some(...)) that triggered these results.
                if v.last().map(|m| m.tool_calls.is_some()).unwrap_or(false) {
                    v.pop();
                }
                let tool_summary = tool_texts.join("\n---\n");
                let nudge = if tool_summary.is_empty() {
                    "Continue the task based on the tool results above. \
                     Call more tools if needed, or provide your final answer."
                        .to_string()
                } else {
                    format!(
                        "Here are the tool results:\n\n{tool_summary}\n\n\
                         Continue the task based on these results. \
                         Call more tools if needed, or provide your final answer."
                    )
                };
                v.push(Message::user(nudge));
                v
            };
            &nudged
        } else {
            msgs
        };

        // Stream the agentic turn — tokens are sent to the TUI as they arrive,
        // while tool-call deltas are accumulated internally by the client.
        // Retry on empty responses (some local models emit EOS too eagerly).
        let turn = {
            const MAX_EMPTY_RETRIES: usize = 3;
            let mut last_err = anyhow::anyhow!("unreachable");
            let mut turn_opt = None;
            for attempt in 0..MAX_EMPTY_RETRIES {
                let tx_stream = tx.clone();
                match client
                    .chat_agent_stream(msgs_to_send, sampling, tools_for_call, move |token| {
                        let _ = tx_stream.send(TuiEvent::StreamToken(token.to_string()));
                    })
                    .await
                {
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
                // Text was already streamed via StreamToken events; now finalize.
                let _ = tx.send(TuiEvent::AssistantText(String::new()));
                msgs.push(Message::assistant(&text));
                return Ok(());
            }
            AgentTurn::ToolCalls(calls) => {
                msgs.push(Message::assistant_tool_calls(calls.clone()));

                // ── Phase 1: plan-mode control & confirmations (sequential) ──
                // Pre-resolved results for control tools; None = needs execution.
                let mut pre_results: Vec<Option<String>> = Vec::with_capacity(calls.len());
                let mut abort = false;

                for call in &calls {
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
                        pre_results.push(Some(String::new())); // already handled
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
                        pre_results.push(Some(String::new()));
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
                        pre_results.push(Some(String::new()));
                        continue;
                    }

                    // Confirm if needed.
                    if needs_confirm(call, executor) {
                        let prompt = fmt_confirm_prompt(call);
                        let (reply_tx, reply_rx) = oneshot::channel::<bool>();
                        let _ = tx.send(TuiEvent::NeedsConfirm { prompt, reply_tx });
                        let allowed = reply_rx.await.unwrap_or(false);
                        if !allowed {
                            let _ = tx.send(TuiEvent::ToolDone("Denied by user.".to_string()));
                            let _ = tx.send(TuiEvent::TurnDone(msgs.clone()));
                            abort = true;
                            break;
                        }
                    }

                    pre_results.push(None); // needs execution
                }

                if abort {
                    return Ok(());
                }

                // ── Phase 2: execute approved tools concurrently ─────────────
                // Spawn all pending tool calls in parallel.  The VM mutex
                // serialises Ruby dispatch today, but native-only tools (or
                // future per-tool VMs) benefit from true concurrency.
                let mut handles: Vec<Option<tokio::task::JoinHandle<String>>> =
                    Vec::with_capacity(calls.len());

                for (call, pre) in calls.iter().zip(pre_results.iter()) {
                    if pre.is_some() {
                        handles.push(None); // already handled in phase 1
                    } else {
                        let exec = ToolExecutor {
                            confirm_writes: false,
                            confirm_shell: false,
                            lsp: executor.lsp.clone(),
                            max_tool_result_chars: executor.max_tool_result_chars,
                            shell_allowlist: executor.shell_allowlist.clone(),
                            shell_denylist: executor.shell_denylist.clone(),
                            vm: executor.vm.clone(),
                        };
                        let call2 = call.clone();
                        handles.push(Some(tokio::task::spawn_blocking(move || {
                            exec.execute_quiet(&call2)
                        })));
                    }
                }

                // ── Phase 3: collect results in order ────────────────────────
                for (i, handle_opt) in handles.into_iter().enumerate() {
                    let Some(handle) = handle_opt else {
                        continue; // already pushed to msgs in phase 1
                    };
                    let call = &calls[i];
                    let result = handle
                        .await
                        .unwrap_or_else(|e| format!("internal error: {e}"));

                    // Show a one-line preview of the result.
                    let preview = result
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(120)
                        .collect::<String>();
                    let _ = tx.send(TuiEvent::ToolDone(preview));

                    // Cap the result to the remaining context budget so a single
                    // large tool result cannot push the total over the limit.
                    // Always leave at least 512 chars so error messages come through.
                    let result_cap = if ctx_size > 0 {
                        let tools_overhead = serde_json::to_string(tools).map_or(0, |s| s.len());
                        let current_size: usize = msgs.iter().map(msg_size).sum();
                        let budget =
                            (ctx_size as usize * 4 * 85 / 100).saturating_sub(tools_overhead);
                        budget.saturating_sub(current_size).max(512)
                    } else {
                        executor.max_tool_result_chars
                    }
                    .min(executor.max_tool_result_chars);

                    // Supersede earlier results from the same read-shaped tool
                    // on the same key (path/url) so context doesn't grow
                    // O(N_calls) when chunked or repeated.
                    if let Some((tool_name, key_name, default_val)) =
                        supersede_spec_for(&call.function.name)
                    {
                        let args: serde_json::Value =
                            serde_json::from_str(&call.function.arguments).unwrap_or_default();
                        let key_val = args
                            .get(*key_name)
                            .and_then(|v| v.as_str())
                            .or(*default_val);
                        if let Some(key_val) = key_val {
                            supersede_prior_tool_for_key(
                                msgs, tool_name, key_name, key_val, &call.id,
                            );
                        }
                    }

                    let capped = cap_tool_result(result, result_cap);
                    let needs_chunk_nudge = result_needs_chunk_nudge(&call.function.name, &capped);

                    msgs.push(Message::tool_result(&call.id, capped));

                    // After a chunked read_file, nudge the model to commit its
                    // outline to an assistant message before requesting the
                    // next chunk — supersede will wipe the bytes once the next
                    // chunk arrives, so any analysis must be persisted now.
                    // Use a user message with [system-reminder] prefix rather
                    // than Message::system: many jinja chat templates (Gemma
                    // in particular, which we depend on via --jinja) reject
                    // or silently drop additional system messages after the
                    // initial one at index 0.
                    if needs_chunk_nudge {
                        msgs.push(Message::user(
                            "[system-reminder] Before requesting the next chunk, append to your \
                             running outline of this file (modules, types, public fns, key call \
                             edges) in your reply. Earlier chunks are replaced with stubs once \
                             the next chunk is read, so any analysis must be persisted in your \
                             own message now.",
                        ));
                    }
                }
            }
        }
    }

    anyhow::bail!("agent exceeded {MAX_AGENT_ITERATIONS} iterations without a text response")
}

fn fmt_call(call: &ToolCallItem) -> String {
    let args: serde_json::Value =
        serde_json::from_str(&call.function.arguments).unwrap_or_default();
    let name = &call.function.name;
    if let Some(map) = args.as_object() {
        let mut keys: Vec<&String> = map.keys().collect();
        keys.sort();
        let parts: Vec<String> = keys
            .iter()
            .filter_map(|k| {
                map[k.as_str()].as_str().map(|s| {
                    let s: String = s.chars().take(60).collect();
                    format!("{k}=\"{s}\"")
                })
            })
            .take(2)
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
        assert!(!msgs.iter().any(|m| m.text_content() == Some("hello")));
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
        let m0 = sys();
        let m1 = usr("aa");
        let m2 = ast("bb");
        let m3 = usr("cc");
        // Budget: fits exactly sys + usr("cc") but not sys + ast("bb") + usr("cc").
        // So two messages must be dropped.
        let budget = msg_size(&m0) + msg_size(&m3);
        let mut msgs = vec![m0, m1, m2, m3];
        let dropped = trim_to_budget(&mut msgs, budget);
        assert_eq!(dropped, 2);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[1].text_content(), Some("cc"));
    }

    #[test]
    fn trim_to_budget_counts_tool_call_messages() {
        // tool_call messages have content = None but their JSON is still counted.
        // With a large budget everything fits and nothing is dropped.
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

    // ── trim_to_budget_before — protected region ─────────────────────────────

    #[test]
    fn trim_to_budget_before_never_removes_protected_messages() {
        // sys + 3 history messages + 2 "current turn" messages.
        // protected_from = 4 means indices 4..5 are off-limits.
        let mut msgs = vec![sys(), usr("h1"), ast("h2"), usr("h3"), ast("turn-result")];
        let budget = 0; // force maximum trimming
        let dropped = trim_to_budget_before(&mut msgs, budget, 4);
        // h1, h2, h3 can be dropped (indices 1-3), but "turn-result" must survive.
        assert!(dropped <= 3);
        assert!(
            msgs.iter().any(|m| m.text_content() == Some("turn-result")),
            "current-turn message must not be removed"
        );
    }

    #[test]
    fn trim_to_budget_before_protected_from_zero_drops_nothing() {
        let mut msgs = vec![sys(), usr("hello"), ast("world")];
        // protected_from = 0 means nothing is safe to drop (protected_from <= 1 stops the loop).
        let dropped = trim_to_budget_before(&mut msgs, 0, 0);
        assert_eq!(dropped, 0);
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn trim_to_budget_before_drops_only_pre_turn_messages() {
        // sys + 2 pre-turn messages + 1 current-turn message.
        let mut msgs = vec![sys(), usr("old1"), usr("old2"), usr("current")];
        // Budget too small to hold everything; protected_from = 3 protects "current".
        let dropped = trim_to_budget_before(&mut msgs, 5, 3);
        assert!(dropped >= 1);
        assert!(
            msgs.iter().any(|m| m.text_content() == Some("current")),
            "current-turn message must survive"
        );
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

    // ── supersede_prior_tool_for_key ──────────────────────────────────────────

    fn assistant_call(id: &str, name: &str, args_json: &str) -> Message {
        Message::assistant_tool_calls(vec![ToolCallItem {
            id: id.into(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: name.into(),
                arguments: args_json.into(),
            },
        }])
    }

    #[test]
    fn supersede_stubs_only_same_path() {
        let mut msgs = vec![
            assistant_call("c1", "read_file", r#"{"path":"a.rs","cursor":1}"#),
            Message::tool_result("c1", "lines 1-400 of a.rs..."),
            assistant_call("c2", "read_file", r#"{"path":"b.rs","cursor":1}"#),
            Message::tool_result("c2", "lines 1-400 of b.rs..."),
            assistant_call("c3", "read_file", r#"{"path":"a.rs","cursor":401}"#),
            Message::tool_result("c3", "lines 401-800 of a.rs..."),
        ];
        supersede_prior_tool_for_key(&mut msgs, "read_file", "path", "a.rs", "c3");

        assert!(
            is_supersede_stub(msgs[1].text_content().unwrap()),
            "prior a.rs chunk should be marked as a stub, got: {:?}",
            msgs[1].text_content()
        );
        assert!(
            msgs[1]
                .text_content()
                .unwrap()
                .contains("read_file result for a.rs"),
            "stub body should mention the tool and key"
        );
        assert_eq!(
            msgs[3].text_content(),
            Some("lines 1-400 of b.rs..."),
            "b.rs chunk must remain intact"
        );
        assert_eq!(
            msgs[5].text_content(),
            Some("lines 401-800 of a.rs..."),
            "current chunk must not be stubbed"
        );
    }

    #[test]
    fn supersede_skips_current_id() {
        let mut msgs = vec![
            assistant_call("c1", "read_file", r#"{"path":"a.rs"}"#),
            Message::tool_result("c1", "first read"),
        ];
        supersede_prior_tool_for_key(&mut msgs, "read_file", "path", "a.rs", "c1");
        assert_eq!(msgs[1].text_content(), Some("first read"));
    }

    #[test]
    fn supersede_ignores_other_tools() {
        let mut msgs = vec![
            assistant_call("c1", "grep_files", r#"{"path":"a.rs","pattern":"x"}"#),
            Message::tool_result("c1", "grep hits in a.rs"),
            assistant_call("c2", "read_file", r#"{"path":"a.rs","cursor":1}"#),
            Message::tool_result("c2", "lines of a.rs"),
        ];
        supersede_prior_tool_for_key(&mut msgs, "read_file", "path", "a.rs", "c2");
        assert_eq!(
            msgs[1].text_content(),
            Some("grep hits in a.rs"),
            "non-read_file tool result must not be stubbed"
        );
    }

    #[test]
    fn supersede_works_for_fetch_url_with_url_key() {
        let mut msgs = vec![
            assistant_call("c1", "fetch_url", r#"{"url":"https://x/y"}"#),
            Message::tool_result("c1", "html body 1"),
            assistant_call("c2", "fetch_url", r#"{"url":"https://x/y"}"#),
            Message::tool_result("c2", "html body 2"),
        ];
        supersede_prior_tool_for_key(&mut msgs, "fetch_url", "url", "https://x/y", "c2");
        assert!(
            is_supersede_stub(msgs[1].text_content().unwrap()),
            "prior fetch_url should be marked as a stub"
        );
        assert!(
            msgs[1]
                .text_content()
                .unwrap()
                .contains("fetch_url result for https://x/y"),
        );
        assert_eq!(msgs[3].text_content(), Some("html body 2"));
    }

    #[test]
    fn is_supersede_stub_detects_sentinel_only() {
        let stub = format!("{SUPERSEDE_STUB_SENTINEL}[earlier read_file result for x]");
        assert!(is_supersede_stub(&stub));
        // A model output that *looks* like a stub but lacks the sentinel must
        // not be misclassified — this is the whole point of the sentinel.
        assert!(!is_supersede_stub(
            "[earlier in this section we noted that…]"
        ));
        assert!(!is_supersede_stub(""));
    }

    #[test]
    fn supersede_spec_for_known_and_unknown_tools() {
        assert!(supersede_spec_for("read_file").is_some());
        assert!(supersede_spec_for("list_directory").is_some());
        assert!(supersede_spec_for("fetch_url").is_some());
        assert!(supersede_spec_for("grep_files").is_none());
        assert!(supersede_spec_for("write_file").is_none());
    }

    #[test]
    fn supersede_dispatch_defaults_list_directory_path_to_dot() {
        let spec = supersede_spec_for("list_directory").unwrap();
        assert_eq!(spec.0, "list_directory");
        assert_eq!(spec.1, "path");
        assert_eq!(spec.2, Some("."));

        // Two list_directory calls without an explicit `path` must supersede
        // each other thanks to the registered default.
        let mut msgs = vec![
            assistant_call("c1", "list_directory", "{}"),
            Message::tool_result("c1", "files in cwd (old)"),
            assistant_call("c2", "list_directory", "{}"),
            Message::tool_result("c2", "files in cwd (new)"),
        ];
        supersede_prior_tool_for_key(&mut msgs, "list_directory", "path", ".", "c2");
        assert!(
            is_supersede_stub(msgs[1].text_content().unwrap()),
            "implicit-path list_directory should supersede via the default"
        );
        assert_eq!(msgs[3].text_content(), Some("files in cwd (new)"));
    }

    #[test]
    fn supersede_default_does_not_apply_across_tool_names() {
        // A read_file call without `path` must not be matched by the
        // list_directory default.
        let mut msgs = vec![
            assistant_call("c1", "read_file", "{}"),
            Message::tool_result("c1", "previous body"),
            assistant_call("c2", "list_directory", "{}"),
            Message::tool_result("c2", "files in cwd"),
        ];
        supersede_prior_tool_for_key(&mut msgs, "list_directory", "path", ".", "c2");
        assert_eq!(
            msgs[1].text_content(),
            Some("previous body"),
            "read_file with implicit args must not be touched by list_directory's default"
        );
    }

    #[test]
    fn supersede_silently_skips_when_args_are_malformed_json() {
        let mut msgs = vec![
            // Assistant message has malformed JSON in arguments.
            assistant_call("c1", "read_file", "{not valid json"),
            Message::tool_result("c1", "previous body"),
            assistant_call("c2", "read_file", r#"{"path":"a.rs"}"#),
            Message::tool_result("c2", "current body"),
        ];
        // Must not panic and must leave the prior message untouched.
        supersede_prior_tool_for_key(&mut msgs, "read_file", "path", "a.rs", "c2");
        assert_eq!(msgs[1].text_content(), Some("previous body"));
        assert_eq!(msgs[3].text_content(), Some("current body"));
    }

    #[test]
    fn result_needs_chunk_nudge_detects_continuation_hint() {
        let body = "fn foo(){}\n\n[lines 1\u{2013}400 of 1873; call read_file again with cursor=401 to continue]";
        assert!(result_needs_chunk_nudge("read_file", body));
    }

    #[test]
    fn result_needs_chunk_nudge_false_at_eof() {
        let body = "fn foo(){}\n\n[lines 1\u{2013}50 of 50; end of file]";
        assert!(!result_needs_chunk_nudge("read_file", body));
    }

    #[test]
    fn result_needs_chunk_nudge_false_for_other_tools() {
        let body = "[lines 1\u{2013}400 of 1873; call read_file again with cursor=401 to continue]";
        // Even if the body looks chunked, only read_file should trigger.
        assert!(!result_needs_chunk_nudge("read_file_range", body));
        assert!(!result_needs_chunk_nudge("grep_files", body));
    }

    #[test]
    fn stub_oldest_no_op_when_no_tool_results_in_turn() {
        // Only system+user messages in this turn — no tool_results to stub.
        let mut msgs = vec![Message::system("sys"), Message::user("hi")];
        let n = stub_oldest_tool_results_in_turn(&mut msgs, 0, 1);
        assert_eq!(n, 0, "must be a no-op when there are no tool_results");
    }

    #[test]
    fn supersede_works_for_list_directory() {
        let mut msgs = vec![
            assistant_call("c1", "list_directory", r#"{"path":"src"}"#),
            Message::tool_result("c1", "files in src (old)"),
            assistant_call("c2", "list_directory", r#"{"path":"src"}"#),
            Message::tool_result("c2", "files in src (new)"),
        ];
        supersede_prior_tool_for_key(&mut msgs, "list_directory", "path", "src", "c2");
        assert!(
            is_supersede_stub(msgs[1].text_content().unwrap()),
            "prior list_directory should be marked as a stub"
        );
    }

    // ── stub_oldest_tool_results_in_turn ──────────────────────────────────────

    #[test]
    fn stub_oldest_no_op_when_under_budget() {
        let mut msgs = vec![
            Message::user("hi"),
            assistant_call("c1", "read_file", r#"{"path":"a"}"#),
            Message::tool_result("c1", "small"),
        ];
        let n = stub_oldest_tool_results_in_turn(&mut msgs, 0, 100_000);
        assert_eq!(n, 0);
        assert_eq!(msgs[2].text_content(), Some("small"));
    }

    #[test]
    fn stub_oldest_preserves_newest_tool_result() {
        // Three big tool results in this turn; budget forces us to stub.
        let big = "x".repeat(2_000);
        let mut msgs = vec![
            assistant_call("c1", "read_file", r#"{"path":"a"}"#),
            Message::tool_result("c1", big.clone()),
            assistant_call("c2", "read_file", r#"{"path":"b"}"#),
            Message::tool_result("c2", big.clone()),
            assistant_call("c3", "read_file", r#"{"path":"c"}"#),
            Message::tool_result("c3", big.clone()),
        ];
        let n = stub_oldest_tool_results_in_turn(&mut msgs, 0, 2_500);
        assert!(n >= 1, "should stub at least one");
        // The newest tool_result must remain intact.
        assert_eq!(
            msgs[5].text_content().map(|s| s.len()),
            Some(big.len()),
            "newest tool_result must not be stubbed"
        );
    }

    #[test]
    fn stub_oldest_skips_already_stubbed() {
        let big = "y".repeat(2_000);
        let stubbed = format!("{SUPERSEDE_STUB_SENTINEL}[earlier read_file result for a]");
        let mut msgs = vec![
            assistant_call("c1", "read_file", r#"{"path":"a"}"#),
            Message::tool_result("c1", stubbed),
            assistant_call("c2", "read_file", r#"{"path":"b"}"#),
            Message::tool_result("c2", big.clone()),
        ];
        // Only one non-stub tool_result remains, so nothing more should be stubbed.
        let n = stub_oldest_tool_results_in_turn(&mut msgs, 0, 100);
        assert_eq!(n, 0);
        assert_eq!(msgs[3].text_content().map(|s| s.len()), Some(big.len()));
    }

    #[test]
    fn stub_oldest_respects_turn_start() {
        let big = "z".repeat(2_000);
        let mut msgs = vec![
            // pre-turn history (must not be touched by this helper)
            assistant_call("p1", "read_file", r#"{"path":"old"}"#),
            Message::tool_result("p1", big.clone()),
            // current turn starts at index 2
            assistant_call("c1", "read_file", r#"{"path":"a"}"#),
            Message::tool_result("c1", big.clone()),
            assistant_call("c2", "read_file", r#"{"path":"b"}"#),
            Message::tool_result("c2", big.clone()),
        ];
        stub_oldest_tool_results_in_turn(&mut msgs, 2, 2_500);
        // Pre-turn message remains untouched.
        assert_eq!(msgs[1].text_content().map(|s| s.len()), Some(big.len()));
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
        for name in &[
            "patch_file",
            "delete_file",
            "move_file",
            "append_file",
            "insert_after_line",
        ] {
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

    // ── consume_raw_buf ───────────────────────────────────────────────────────

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
        // "# 🏠" is 6 bytes: '#'(1) ' '(1) '🏠'(4). With HOLD_OPEN=6 the
        // hold-back arithmetic can land inside the emoji if we don't snap to
        // a char boundary.
        let input = "# \u{1F3E0}";
        let (streaming, thinking) = run_consume(input);
        assert!(thinking.is_none());
        // The full string must appear once the buffer is flushed.
        let s = streaming.unwrap_or_default();
        assert!(s.contains("# \u{1F3E0}") || s.is_empty());
    }
}
