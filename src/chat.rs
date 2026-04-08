// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::Result;
use std::collections::HashMap;

use crate::client::{LlamaClient, Message, ToolDef};
use crate::config::SkillDef;
use crate::tools::ToolExecutor;

// ── System prompt styles ─────────────────────────────────────────────────────

/// Full prompt — detailed tool-by-tool guidance. Best for large, capable models
/// (30B+ parameters) with wide context windows.
pub const PROMPT_FULL: &str = "\
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
To insert new lines at a specific position (e.g. after a range you read \
with read_file_range), use insert_after_line with the last line number of \
the range — do not use append_file, which always goes to EOF. \
patch_file replaces old_str with new_str atomically — after a successful \
patch_file the new content is already written; never follow it with \
append_file or write_file for the same change. \
old_str must be the exact text from the file as read_file or read_file_range \
returns it; new_str, write_file content, append_file content, and \
insert_after_line content are written verbatim. \
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
Use lsp to get accurate semantic information from the language server: call it with \
operation=\"hover\" to see a symbol's type and documentation, \"definition\" to find \
where it is declared, \"references\" to list all usages, or \"diagnostics\" to get \
current compiler errors and warnings for a file. Prefer lsp over guessing from source \
text for these queries.\n\n\
Before making changes that span multiple files, call enter_plan_mode to switch to \
read-only exploration. In plan mode you can read files, search, and query the LSP \
without being able to write anything. When you have a clear plan, call exit_plan_mode \
to restore write access and then apply the changes.\n\n\
Use plain Unicode symbols (→, ←, ⇒, ×, ≤, ≥, ≠, ≈, …) instead of \
LaTeX math notation ($\\rightarrow$, $\\leq$, etc.). Output is rendered in a \
plain terminal, not a LaTeX or Markdown renderer.\n\n\
When you decide to call a tool, call it immediately — do not emit a \
\"please wait\" or \"I will now…\" message before making the call. \
Announcing an action without performing it forces the user to prompt \
you again to actually do it.";

/// Concise prompt — core rules without per-tool details. Good for capable
/// mid-size models (7B–30B) where context is more constrained.
pub const PROMPT_CONCISE: &str = "\
You are ShioRamen, a local coding assistant running offline. Be concise. \
Provide working code with correct language fences. Prefer standard library solutions.\n\n\
Always read a file before editing it. Use patch_file for targeted edits, not write_file. \
Use read_file_range for large files. Call get_working_directory before constructing paths. \
Use lsp for type info, definitions, and diagnostics instead of guessing from source text. \
Use fetch_url when the user shares a URL. Use web_search + fetch_url to find documentation. \
Use save_memory to persist important project facts across sessions.\n\n\
Call tools immediately — do not announce actions before performing them. \
Use plain Unicode (→, ≤, ≠) instead of LaTeX notation.";

/// Minimal prompt — bare essentials for small models (< 7B) where every token
/// of context matters.
pub const PROMPT_MINIMAL: &str = "\
You are ShioRamen, a local coding assistant. Be concise. Use tools to read/write files \
and run commands. Always read before editing. Use patch_file for edits. \
Call tools immediately without announcing them.";

/// Backward-compatible alias.
pub const DEFAULT_SYSTEM_PROMPT: &str = PROMPT_FULL;

/// Select the system prompt for a given style name.
/// Returns `None` for unrecognised names so the caller can fall back.
pub fn prompt_for_style(style: &str) -> Option<&'static str> {
    match style {
        "full" => Some(PROMPT_FULL),
        "concise" => Some(PROMPT_CONCISE),
        "minimal" => Some(PROMPT_MINIMAL),
        _ => None,
    }
}

/// Guess the best prompt style from a model filename.
///
/// Large/capable models → "full"; mid-size → "concise"; small → "minimal".
/// Falls back to "full" for unrecognised names.
pub fn detect_prompt_style(model_path: &str) -> &'static str {
    let name = model_path.to_ascii_lowercase();

    // Extract parameter count from common patterns like "7b", "14b", "70b", "4b".
    let param_b = extract_param_size(&name);

    if let Some(b) = param_b {
        return if b >= 20.0 {
            "full"
        } else if b >= 4.0 {
            "concise"
        } else {
            "minimal"
        };
    }

    // If we can't determine size, default to full.
    "full"
}

/// Try to extract a parameter-count number (in billions) from a model name.
/// Matches patterns like "7b", "14B", "7B-Instruct", "4b-it", "26b-q4".
fn extract_param_size(name: &str) -> Option<f64> {
    // Match a number (possibly with decimal) followed by 'b', at a word boundary.
    // Examples: "7b", "14b", "1.5b", "26b", "70b", "0.5b"
    let bytes = name.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Find a digit.
        if bytes[i].is_ascii_digit() {
            let start = i;
            // Advance past digits and an optional decimal point + more digits.
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'.' {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            // Check if followed by 'b' (case-insensitive) and not by more letters
            // (to avoid matching "32768" from context sizes).
            if i < bytes.len()
                && bytes[i].eq_ignore_ascii_case(&b'b')
                && (i + 1 >= bytes.len()
                    || !bytes[i + 1].is_ascii_alphabetic()
                    || matches!(bytes[i + 1], b'-' | b'_' | b'.'))
            {
                let num_str = &name[start..i];
                if let Ok(n) = num_str.parse::<f64>() {
                    return Some(n);
                }
            }
        }
        i += 1;
    }
    None
}

pub struct ChatSession {
    pub(crate) client: LlamaClient,
    pub(crate) messages: Vec<Message>,
    pub(crate) temperature: f32,
    /// When Some, the agentic loop is active and these tools are offered to the model.
    pub(crate) executor: Option<ToolExecutor>,
    /// Tool definitions, computed once and reused across turns.
    pub(crate) tools: Vec<ToolDef>,
    /// Custom skills loaded from `[skills.*]` in shio.toml.
    pub(crate) skills: HashMap<String, SkillDef>,
    /// Context window size in tokens (0 = unknown).
    pub(crate) ctx_size: u32,
    /// Whether to render `<think>…</think>` blocks in a dimmed style.
    pub(crate) show_thinking: bool,
}

impl ChatSession {
    pub fn new(
        client: LlamaClient,
        temperature: f32,
        system_prompt: String,
        executor: Option<ToolExecutor>,
        skills: HashMap<String, SkillDef>,
        ctx_size: u32,
        show_thinking: bool,
    ) -> Self {
        let tools = executor.as_ref().map_or_else(Vec::new, |e| e.tool_defs());
        Self {
            client,
            messages: vec![Message::system(system_prompt)],
            temperature,
            executor,
            tools,
            skills,
            ctx_size,
            show_thinking,
        }
    }

    /// Replace the message history with a previously saved session.
    /// The first message (system prompt) is always kept from the current
    /// session so config changes take effect; saved messages from index 1
    /// onward are appended.
    pub fn resume(&mut self, saved: Vec<crate::client::Message>) {
        // Skip the saved system prompt (index 0) — use the current one.
        let history = saved.into_iter().skip(1).collect::<Vec<_>>();
        self.messages.extend(history);
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
        ChatSession::new(
            client,
            0.7,
            "be helpful".to_string(),
            executor,
            HashMap::new(),
            0,
            true,
        )
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
    fn new_session_without_executor_has_no_tools_and_no_executor() {
        let session = make_session(None);
        assert!(session.executor.is_none());
        // No executor → no VM → tools list is empty.
        assert!(session.tools.is_empty());
    }

    #[test]
    fn new_session_with_executor_has_executor() {
        let exec = ToolExecutor {
            confirm_writes: true,
            confirm_shell: true,
            ..Default::default()
        };
        let session = make_session(Some(exec));
        assert!(session.executor.is_some());
    }

    // ── Skills ───────────────────────────────────────────────────────────────

    #[test]
    fn new_session_with_no_skills_has_empty_map() {
        let session = make_session(None);
        assert!(session.skills.is_empty());
    }

    #[test]
    fn new_session_with_skills_retains_them() {
        let mut skills = HashMap::new();
        skills.insert(
            "commit".to_string(),
            SkillDef {
                description: "desc".to_string(),
                prompt: "prompt".to_string(),
            },
        );
        let client = LlamaClient::new("http://127.0.0.1:1".to_string());
        let session = ChatSession::new(client, 0.7, "sys".to_string(), None, skills, 0, true);
        assert_eq!(session.skills.len(), 1);
        assert!(session.skills.contains_key("commit"));
    }

    // ── prompt_for_style ─────────────────────────────────────────────────────

    #[test]
    fn prompt_for_style_returns_known_styles() {
        assert!(prompt_for_style("full").is_some());
        assert!(prompt_for_style("concise").is_some());
        assert!(prompt_for_style("minimal").is_some());
    }

    #[test]
    fn prompt_for_style_returns_none_for_unknown() {
        assert!(prompt_for_style("turbo").is_none());
    }

    #[test]
    fn prompt_styles_differ_in_length() {
        let full = prompt_for_style("full").unwrap();
        let concise = prompt_for_style("concise").unwrap();
        let minimal = prompt_for_style("minimal").unwrap();
        assert!(full.len() > concise.len());
        assert!(concise.len() > minimal.len());
    }

    // ── detect_prompt_style ──────────────────────────────────────────────────

    #[test]
    fn detect_large_model_gets_full() {
        assert_eq!(
            detect_prompt_style("Qwen2.5-Coder-32B-Instruct-Q4_K_M.gguf"),
            "full"
        );
        assert_eq!(detect_prompt_style("llama-70b-chat.gguf"), "full");
    }

    #[test]
    fn detect_mid_model_gets_concise() {
        assert_eq!(
            detect_prompt_style("Qwen2.5-Coder-7B-Instruct-Q4_K_M.gguf"),
            "concise"
        );
        assert_eq!(detect_prompt_style("gemma-4-12b-it-Q8_0.gguf"), "concise");
        assert_eq!(detect_prompt_style("codestral-14b.gguf"), "concise");
    }

    #[test]
    fn detect_small_model_gets_minimal() {
        assert_eq!(
            detect_prompt_style("phi-3-mini-3.8b-instruct.gguf"),
            "minimal"
        );
        assert_eq!(detect_prompt_style("qwen2.5-1.5b-coder.gguf"), "minimal");
    }

    #[test]
    fn detect_unknown_model_gets_full() {
        assert_eq!(detect_prompt_style("mystery-model.gguf"), "full");
    }

    #[test]
    fn detect_empty_path_gets_full() {
        assert_eq!(detect_prompt_style(""), "full");
    }

    // ── extract_param_size ───────────────────────────────────────────────────

    #[test]
    fn extract_param_size_common_patterns() {
        assert_eq!(extract_param_size("qwen-7b-instruct"), Some(7.0));
        assert_eq!(extract_param_size("llama-70b"), Some(70.0));
        assert_eq!(extract_param_size("phi-3.8b"), Some(3.8));
        assert_eq!(extract_param_size("gemma-4-26b-q4"), Some(26.0));
    }

    #[test]
    fn extract_param_size_no_match() {
        assert_eq!(extract_param_size("mystery-model.gguf"), None);
        assert_eq!(extract_param_size(""), None);
    }

    // ── resume ───────────────────────────────────────────────────────────────

    #[test]
    fn resume_skips_saved_system_prompt() {
        let mut session = make_session(None);
        let saved = vec![
            Message::system("old system prompt"),
            Message::user("hello"),
            Message::assistant("hi"),
        ];
        session.resume(saved);
        // System prompt should be the current one, not the saved one.
        assert_eq!(session.messages[0].content.as_deref(), Some("be helpful"));
        assert_eq!(session.messages.len(), 3); // system + user + assistant
        assert_eq!(session.messages[1].role, "user");
    }
}
