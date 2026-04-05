// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::Result;
use rustyline::completion::{Completer, FilenameCompleter, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};

use crate::client::{AgentTurn, LlamaClient, Message, ToolDef};
use crate::context;
use crate::tools::{ToolExecutor, all_tools};

pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are ShioRamen, a sharp, focused local coding assistant running entirely offline. \
Be concise and accurate. Provide working code with minimal prose unless the user asks \
for explanation. Always fence code blocks with the correct language identifier. \
Prefer modern idioms and standard library solutions. If a question is ambiguous, ask \
one clarifying question before proceeding.\n\n\
When answering questions about code, use read_file and search_files to inspect the \
actual source rather than guessing. Always read a file before editing it. \
Prefer targeted edits over full rewrites. If a task would touch more than 3 files, \
summarize your plan and ask for confirmation before proceeding.";

/// Maximum number of tool-call → result rounds per user turn before bailing.
const MAX_AGENT_ITERATIONS: usize = 20;

/// Total-context warning threshold: warn when a single /include injects more than 512 KB.
const INCLUDE_WARN_BYTES: usize = 512 * 1024;

pub struct ChatSession {
    pub(crate) client: LlamaClient,
    pub(crate) messages: Vec<Message>,
    pub(crate) temperature: f32,
    /// When Some, the agentic loop is active and these tools are offered to the model.
    pub(crate) executor: Option<ToolExecutor>,
    /// Tool definitions, computed once and reused across turns.
    pub(crate) tools: Vec<ToolDef>,
}

impl ChatSession {
    pub fn new(
        client: LlamaClient,
        temperature: f32,
        system_prompt: String,
        executor: Option<ToolExecutor>,
    ) -> Self {
        Self {
            client,
            messages: vec![Message::system(system_prompt)],
            temperature,
            executor,
            tools: all_tools(),
        }
    }

    /// Start the interactive session. Consumes `self` and hands ownership to the TUI.
    pub async fn run(self) -> Result<()> {
        crate::tui::run(self).await
    }

    /// Legacy line-mode REPL, kept for testing and fallback use.
    #[allow(dead_code)]
    pub async fn run_line_mode(&mut self) -> Result<()> {
        let mut rl: Editor<SlashCompleter, DefaultHistory> = Editor::new()?;
        rl.set_helper(Some(SlashCompleter::new(self.executor.is_some())));

        println!("ShioRamen — local coding assistant");
        if let Some(exec) = &self.executor {
            println!(
                "Commands: /reset  clear history | /include <path>  load file(s) | /tools  list tools | /exit  quit"
            );
            println!("Tool use: ON");
            if !exec.confirm_writes || !exec.confirm_shell {
                let what = match (!exec.confirm_writes, !exec.confirm_shell) {
                    (true, true) => "writes and shell commands",
                    (true, false) => "writes",
                    (false, true) => "shell commands",
                    (false, false) => unreachable!(),
                };
                eprintln!(
                    "\x1b[33m  Warning: confirmation disabled for {what} — \
                    the model can modify files/run commands without prompting.\x1b[0m"
                );
            }
        } else {
            println!(
                "Commands: /reset  clear history | /include <path>  load file(s) | /exit  quit"
            );
        }
        println!();

        loop {
            match rl.readline("you> ") {
                Ok(line) => {
                    let input = line.trim().to_string();
                    if input.is_empty() {
                        continue;
                    }
                    rl.add_history_entry(&input)?;

                    match input.as_str() {
                        "/exit" | "/quit" => {
                            println!("Sayonara!");
                            break;
                        }
                        "/reset" => {
                            self.messages.truncate(1);
                            println!("[history cleared]");
                            continue;
                        }
                        "/tools" => {
                            println!("Available tools ({}):", self.tools.len());
                            for t in &self.tools {
                                println!("  • {} — {}", t.function.name, t.function.description);
                            }
                            continue;
                        }
                        _ if input.starts_with("/include ") => {
                            let path_str = input["/include ".len()..].trim();
                            self.cmd_include(path_str);
                            continue;
                        }
                        _ => {}
                    }

                    // Record baseline BEFORE pushing the user message so that on
                    // error we can restore the history completely (user message +
                    // any partially-accumulated tool-call/result messages).
                    let baseline = self.messages.len();
                    self.messages.push(Message::user(input));

                    let result = match &self.executor {
                        Some(_) => self.run_agent_turn().await,
                        None => self.run_stream_turn().await,
                    };

                    if let Err(e) = result {
                        eprintln!("\n[error] {e}");
                        self.messages.truncate(baseline);
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    println!();
                    continue;
                }
                Err(ReadlineError::Eof) => {
                    println!("Sayonara!");
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }

        Ok(())
    }

    // ── Turn implementations ──────────────────────────────────────────────────

    async fn run_stream_turn(&mut self) -> Result<()> {
        print_flush("shio> ");
        let response = self
            .client
            .chat_stream(&self.messages, self.temperature)
            .await?;
        self.messages.push(Message::assistant(response));
        Ok(())
    }

    async fn run_agent_turn(&mut self) -> Result<()> {
        let executor = self.executor.as_ref().expect("called without executor");

        for _ in 0..MAX_AGENT_ITERATIONS {
            eprint!("shio> ");
            flush_stderr();

            match self
                .client
                .chat_agent(&self.messages, self.temperature, &self.tools)
                .await?
            {
                AgentTurn::Text(text) => {
                    // Clear the "shio> " prompt and print the response.
                    println!("{text}");
                    self.messages.push(Message::assistant(&text));
                    return Ok(());
                }

                AgentTurn::ToolCalls(calls) => {
                    eprintln!(); // newline after "shio> "
                    // Record the assistant's tool-call turn.
                    self.messages
                        .push(Message::assistant_tool_calls(calls.clone()));

                    // Execute each tool and push the result.
                    for call in &calls {
                        let result = executor.execute(call);
                        self.messages.push(Message::tool_result(&call.id, result));
                    }
                    // Loop: give the model another turn with tool results in context.
                }
            }
        }

        anyhow::bail!(
            "agent exceeded {MAX_AGENT_ITERATIONS} tool-call iterations without producing a response"
        )
    }

    // ── /include command ─────────────────────────────────────────────────────

    fn cmd_include(&mut self, path_str: &str) {
        let path = std::path::Path::new(path_str);
        match context::collect(path) {
            Err(e) => eprintln!("[error] {e}"),
            Ok(files) if files.is_empty() => {
                println!("[no source files found in {path_str}]");
            }
            Ok(files) => {
                let count = files.len();
                let total_bytes: usize = files.iter().map(|(_, c)| c.len()).sum();
                let listing: Vec<String> = files
                    .iter()
                    .map(|(p, c)| format!("  {} ({} B)", p.display(), c.len()))
                    .collect();
                let content = context::format_as_blocks(&files);
                self.messages.push(Message::user(content));
                self.messages.push(Message::assistant(format!(
                    "Understood. I've loaded {count} file(s) and am ready for your questions."
                )));
                println!("[included {count} file(s) from {path_str}]");
                for line in &listing {
                    println!("{line}");
                }
                if total_bytes > INCLUDE_WARN_BYTES {
                    eprintln!(
                        "\x1b[33m  Warning: {:.0} KB of context injected — this may fill the context window.\x1b[0m",
                        total_bytes as f64 / 1024.0
                    );
                }
            }
        }
    }
}

fn print_flush(s: &str) {
    use std::io::Write;
    print!("{s}");
    std::io::stdout().flush().ok();
}

fn flush_stderr() {
    use std::io::Write;
    std::io::stderr().flush().ok();
}

// ── Tab-completion for slash commands ────────────────────────────────────────

/// Rustyline helper that completes slash commands and, for `/include`, delegates
/// to `FilenameCompleter` so the user gets filesystem path completion.
pub struct SlashCompleter {
    file_completer: FilenameCompleter,
    /// The full set of slash commands offered for completion.
    commands: Vec<&'static str>,
}

impl SlashCompleter {
    pub fn new(has_tools: bool) -> Self {
        let mut commands = vec!["/exit", "/quit", "/reset", "/include "];
        if has_tools {
            commands.push("/tools");
        }
        Self {
            file_completer: FilenameCompleter::new(),
            commands,
        }
    }
}

impl Completer for SlashCompleter {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        // For `/include <path>`, hand the whole line to FilenameCompleter.
        // It extracts the last word (the path) and returns (word_start, matches).
        if line.starts_with("/include ") {
            return self.file_completer.complete(line, pos, ctx);
        }

        // For any other `/…` prefix, complete from the known command list.
        if line.starts_with('/') {
            let typed = &line[..pos];
            let candidates: Vec<Pair> = self
                .commands
                .iter()
                .filter(|&&cmd| cmd.starts_with(typed))
                .map(|&cmd| Pair {
                    display: cmd.to_string(),
                    replacement: cmd.to_string(),
                })
                .collect();
            return Ok((0, candidates));
        }

        Ok((pos, vec![]))
    }
}

// The remaining traits are required by rustyline's `Helper` supertrait but need
// no custom behaviour here — the defaults do nothing.
impl Hinter for SlashCompleter {
    type Hint = String;
}
impl Highlighter for SlashCompleter {}
impl Validator for SlashCompleter {}
impl Helper for SlashCompleter {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::LlamaClient;

    fn make_session(executor: Option<ToolExecutor>) -> ChatSession {
        let client = LlamaClient::new("http://127.0.0.1:1".to_string());
        ChatSession::new(client, 0.7, "be helpful".to_string(), executor)
    }

    // ── ChatSession::new ──────────────────────────────────────────────────────

    #[test]
    fn new_session_starts_with_system_message() {
        let session = make_session(None);
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].role, "system");
        assert_eq!(session.messages[0].content.as_deref(), Some("be helpful"));
    }

    #[test]
    fn new_session_without_executor_has_tools_but_no_executor() {
        let session = make_session(None);
        assert!(session.executor.is_none());
        // Tools are always populated (used by /tools command)
        assert!(!session.tools.is_empty());
    }

    #[test]
    fn new_session_with_executor_has_executor() {
        let exec = ToolExecutor {
            confirm_writes: true,
            confirm_shell: true,
        };
        let session = make_session(Some(exec));
        assert!(session.executor.is_some());
    }

    // ── cmd_include ───────────────────────────────────────────────────────────

    #[test]
    fn cmd_include_nonexistent_path_does_not_add_messages() {
        let mut session = make_session(None);
        let before = session.messages.len();
        session.cmd_include("/nonexistent/shio_chat_test");
        assert_eq!(session.messages.len(), before);
    }

    #[test]
    fn cmd_include_adds_two_messages_for_a_file() {
        let path = std::env::temp_dir().join("shio_chat_include.rs");
        std::fs::write(&path, "fn main() {}").unwrap();

        let mut session = make_session(None);
        let before = session.messages.len();
        session.cmd_include(path.to_str().unwrap());

        // Expect one user message (file content) + one assistant acknowledgement.
        assert_eq!(session.messages.len(), before + 2);
        assert_eq!(session.messages[before].role, "user");
        assert_eq!(session.messages[before + 1].role, "assistant");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cmd_include_empty_dir_does_not_add_messages() {
        let dir = std::env::temp_dir().join("shio_chat_empty_dir");
        std::fs::create_dir_all(&dir).unwrap();

        let mut session = make_session(None);
        let before = session.messages.len();
        session.cmd_include(dir.to_str().unwrap());
        assert_eq!(session.messages.len(), before);

        let _ = std::fs::remove_dir(&dir);
    }
}
