// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::Result;

use crate::client::{LlamaClient, Message, ToolDef};
use crate::tools::{ToolExecutor, all_tools};

pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are ShioRamen, a sharp, focused local coding assistant running entirely offline. \
Be concise and accurate. Provide working code with minimal prose unless the user asks \
for explanation. Always fence code blocks with the correct language identifier. \
Prefer modern idioms and standard library solutions. If a question is ambiguous, ask \
one clarifying question before proceeding.\n\n\
When answering questions about code, use read_file and search_files to inspect the \
actual source rather than guessing. For large files, use read_file_range to read \
only the relevant section. Always read a file before editing it. \
Prefer patch_file for targeted edits over full rewrites with write_file — \
patch_file is safer because it only modifies the matched region. \
Use get_working_directory to orient yourself before constructing file paths, and \
create_directory (not run_shell) when you need to make new directories. \
If a task would touch more than 3 files, summarize your plan and ask for \
confirmation before proceeding.\n\n\
When the user shares a URL or asks about a web page, always call fetch_url to read \
its contents before answering. Do not guess from training data when you can fetch \
the real page. \
When you need to find documentation, crate examples, or current information and no \
URL is provided, use web_search first, then fetch_url on a promising result. \
Use save_memory to record important facts about this project — user preferences, \
architectural decisions, conventions — so you can recall them in future sessions. \
When handling a complex multi-step task, use write_todos to maintain a visible task \
list so the user can track progress. \
Use read_many_files when you need to inspect several related files at once (e.g. a \
whole module) rather than calling read_file repeatedly.\n\n\
Use plain Unicode symbols (→, ←, ⇒, ×, ≤, ≥, ≠, ≈, …) instead of \
LaTeX math notation ($\\rightarrow$, $\\leq$, etc.). Output is rendered in a \
plain terminal, not a LaTeX or Markdown renderer.";

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
}

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
}
