// SPDX-License-Identifier: GPL-3.0-or-later
use std::io::{self, Write};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;

use crate::client::{FunctionSpec, ToolCallItem, ToolDef};
use crate::ruby::vm::ShioVm;

/// Default character limit for `fetch_url` responses.
const DEFAULT_MAX_CHARS: usize = 8_000;

// ── Private helpers ───────────────────────────────────────────────────────────

/// Extract a required `&str` field from a JSON args object.
///
/// Expands to an early `return` with an error message if the field is absent or
/// not a string, so it must be used inside functions that return `String`.
macro_rules! require_str {
    ($args:expr, $field:literal) => {
        match $args[$field].as_str() {
            Some(v) => v,
            None => return format!("Error: missing '{}' argument", $field),
        }
    };
}

// ── Tool definitions (sent to the model) ─────────────────────────────────────

pub fn all_tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "read_file".into(),
                description: "Read the full contents of a file from the filesystem.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file" }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "write_file".into(),
                description: "Write content to a file, creating it or overwriting it.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path":    { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "insert_after_line".into(),
                description: "Insert new content immediately after a specific line number in a \
                    file. Use this when you need to add lines at a precise position — for \
                    example, right after a range you read with read_file_range. \
                    Lines are 1-indexed. The content is inserted after the given line; \
                    existing lines below that point are shifted down. \
                    Do NOT use append_file when you know the insertion point — use this instead."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path":    { "type": "string" },
                        "line":    { "type": "integer", "description": "1-indexed line number after which to insert" },
                        "content": { "type": "string", "description": "Text to insert. A trailing newline is added automatically if absent." }
                    },
                    "required": ["path", "line", "content"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "append_file".into(),
                description: "Append content to the end of a file, creating it if it does not \
                    exist. Use this ONLY when you need to add content at the very end and \
                    do not know or care about the insertion line number. \
                    If you know which line to insert after (e.g. from read_file_range), \
                    use insert_after_line instead. \
                    Do NOT use this to replace, rewrite, or refactor existing lines — \
                    use patch_file for in-place edits."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path":    { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "list_directory".into(),
                description: "List files and directories inside a directory.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Directory path (default: current directory)"
                        }
                    }
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "run_shell".into(),
                description: "Run a shell command and return its stdout and stderr.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to run"
                        }
                    },
                    "required": ["command"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "search_files".into(),
                description: "Find files by glob pattern (e.g. \"src/**/*.rs\").".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string" },
                        "path": {
                            "type": "string",
                            "description": "Root directory to search in (default: .)"
                        }
                    },
                    "required": ["pattern"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "grep_files".into(),
                description: "Search for a regex pattern in files and return matching lines with line numbers.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Regex pattern" },
                        "path": {
                            "type": "string",
                            "description": "File or directory to search (default: .)"
                        },
                        "case_insensitive": { "type": "boolean" }
                    },
                    "required": ["pattern"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "read_file_range".into(),
                description: "Read a specific range of lines from a file. \
                    Prefer this over read_file for large files when you already \
                    know which section you need (e.g. from grep_files results)."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file" },
                        "start_line": {
                            "type": "integer",
                            "description": "First line to return, 1-indexed (inclusive)"
                        },
                        "end_line": {
                            "type": "integer",
                            "description": "Last line to return, 1-indexed (inclusive). \
                                Omit to read to end of file."
                        }
                    },
                    "required": ["path", "start_line"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "patch_file".into(),
                description: "Apply a targeted find-and-replace edit to a file. \
                    Finds the exact string old_str (must appear exactly once) and \
                    replaces it with new_str. Use this for ALL in-place edits: \
                    modifying, refactoring, or rewriting existing lines. \
                    Safer than write_file for focused edits because the rest of the file is untouched. \
                    old_str must be the exact text from the file as returned by read_file or \
                    read_file_range (which outputs raw lines with no line-number prefixes). \
                    new_str is written verbatim."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path":    { "type": "string", "description": "File to edit" },
                        "old_str": {
                            "type": "string",
                            "description": "Exact text to find (must appear exactly once)"
                        },
                        "new_str": {
                            "type": "string",
                            "description": "Replacement text"
                        }
                    },
                    "required": ["path", "old_str", "new_str"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "delete_file".into(),
                description: "Delete a file from the filesystem.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file to delete" }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "move_file".into(),
                description: "Move or rename a file or directory.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "src": { "type": "string", "description": "Source path" },
                        "dst": { "type": "string", "description": "Destination path" }
                    },
                    "required": ["src", "dst"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "fetch_url".into(),
                description: "Fetch the text content of an HTTP or HTTPS URL. \
                    HTML pages are stripped to readable text. \
                    Use this whenever the user shares a URL or asks about a web page."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "The http:// or https:// URL to fetch"
                        },
                        "max_chars": {
                            "type": "integer",
                            "description": "Maximum characters of extracted text to return (default: 8000)"
                        }
                    },
                    "required": ["url"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "create_directory".into(),
                description: "Create a directory and any missing parent directories \
                    (equivalent to `mkdir -p`). Safe to call even if the directory \
                    already exists."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Directory path to create"
                        }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "get_working_directory".into(),
                description: "Return the current working directory. \
                    Call this to resolve relative paths or orient yourself \
                    before constructing file paths."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "web_search".into(),
                description: "Search the web using DuckDuckGo and return a list of results \
                    with titles, URLs, and snippets. Use this when you need current \
                    information, documentation, or examples that may not be in your \
                    training data."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search query"
                        },
                        "max_results": {
                            "type": "integer",
                            "description": "Maximum number of results to return (default: 5, max: 20)"
                        }
                    },
                    "required": ["query"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "save_memory".into(),
                description: "Append a fact or note to SHIO.md for future reference. \
                    Use this to persist important information across sessions: \
                    user preferences, project conventions, architectural decisions, \
                    or anything you want to remember."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "memory": {
                            "type": "string",
                            "description": "The fact or note to remember"
                        },
                        "file": {
                            "type": "string",
                            "description": "Memory file path (default: SHIO.md)"
                        }
                    },
                    "required": ["memory"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "read_many_files".into(),
                description: "Read the contents of multiple files in a single call. \
                    Returns each file's content separated by a header showing its path. \
                    More efficient than calling read_file repeatedly when you need \
                    several related files at once."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "paths": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "List of file paths to read"
                        }
                    },
                    "required": ["paths"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "write_todos".into(),
                description: "Write a task list to TODO.md, replacing the file's entire contents. \
                    Useful for tracking multi-step plans or progress on complex tasks."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "task": {
                                        "type": "string",
                                        "description": "Task description"
                                    },
                                    "status": {
                                        "type": "string",
                                        "enum": ["pending", "in_progress", "completed"],
                                        "description": "Task status (default: pending)"
                                    }
                                },
                                "required": ["task"]
                            },
                            "description": "List of tasks to write"
                        },
                        "file": {
                            "type": "string",
                            "description": "Path to the todo file (default: TODO.md)"
                        }
                    },
                    "required": ["todos"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "lsp".into(),
                description: "Query a Language Server Protocol (LSP) server for semantic \
                    information about source code: type signatures, documentation (hover), \
                    jump-to-definition, find-all-references, and diagnostics (errors/warnings). \
                    The server is started and cached automatically; no setup required."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "operation": {
                            "type": "string",
                            "enum": ["hover", "definition", "references", "diagnostics"],
                            "description": "What to query: \
                                hover = type/doc at position; \
                                definition = where a symbol is declared; \
                                references = all usages of a symbol; \
                                diagnostics = errors and warnings in the file"
                        },
                        "file": {
                            "type": "string",
                            "description": "Path to the source file"
                        },
                        "line": {
                            "type": "integer",
                            "description": "1-indexed line number (required for hover, definition, references)"
                        },
                        "column": {
                            "type": "integer",
                            "description": "1-indexed column number (default: 1)"
                        }
                    },
                    "required": ["operation", "file"]
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "enter_plan_mode".into(),
                description: "Switch to plan mode, restricting tool access to read-only operations \
                    (read_file, search_files, grep_files, lsp, fetch_url, web_search, etc.). \
                    Use this before making changes: explore the codebase, understand the structure, \
                    draft a plan, then call exit_plan_mode to restore full tool access."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "reason": {
                            "type": "string",
                            "description": "Optional: why you are entering plan mode"
                        }
                    }
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "exit_plan_mode".into(),
                description: "Exit plan mode and restore access to all tools \
                    (write_file, patch_file, run_shell, etc.). \
                    Call this when you have finished exploring and are ready to make changes."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "plan": {
                            "type": "string",
                            "description": "Optional: brief summary of the plan before exiting"
                        }
                    }
                }),
            },
        },
    ]
}

// ── Shared HTTP client ────────────────────────────────────────────────────────

/// Return the process-wide blocking HTTP client, or an error string if the
/// TLS backend failed to initialise.  The client is constructed once and
/// reused; per-request timeouts are set by each call site via
/// `client.get(url).timeout(…)`.
fn http_client() -> Result<&'static reqwest::blocking::Client, String> {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c);
    }
    let built = reqwest::blocking::Client::builder()
        .user_agent("ShioRamen/0.1 (local AI assistant)")
        .build()
        .map_err(|e| format!("Error: failed to initialise HTTP client: {e}"))?;
    // get_or_init guarantees only one value is stored even under contention;
    // if another thread won the race our `built` is simply dropped.
    Ok(CLIENT.get_or_init(|| built))
}

// ── Executor ─────────────────────────────────────────────────────────────────

/// Fallback cap used when no context size is known.
/// At startup this is overridden to `ctx_size * 4 * 75 / 100` so the
/// limit automatically scales with the configured context window.
pub const DEFAULT_MAX_TOOL_RESULT_CHARS: usize = 24_000;

#[derive(Clone)]
pub struct ToolExecutor {
    pub confirm_writes: bool,
    pub confirm_shell: bool,
    /// LSP server overrides from `[lsp.servers]` in `shio.toml`.
    pub lsp: std::collections::HashMap<String, String>,
    /// Maximum characters returned from a single tool call before truncation.
    /// Computed from `ctx_size` at startup so the cap scales with the context window.
    pub max_tool_result_chars: usize,
    /// mRuby VM for Ruby-hosted tool handlers (Phase B+).
    pub(crate) vm: Arc<Mutex<ShioVm>>,
}

impl Default for ToolExecutor {
    fn default() -> Self {
        Self {
            confirm_writes: false,
            confirm_shell: false,
            lsp: std::collections::HashMap::new(),
            max_tool_result_chars: DEFAULT_MAX_TOOL_RESULT_CHARS,
            vm: Arc::new(Mutex::new(ShioVm::new().expect("ShioVm init failed"))),
        }
    }
}

const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

impl ToolExecutor {
    /// Execute a tool call without producing any terminal output.
    /// Confirmation must be handled externally before calling this.
    /// Used by the TUI agent loop where stderr would corrupt the display.
    pub fn execute_quiet(&self, call: &ToolCallItem) -> String {
        let mut args: Value = match serde_json::from_str(&call.function.arguments) {
            Ok(v) => v,
            Err(e) => return format!("Error parsing arguments: {e}"),
        };
        // Some local models wrap all arguments under the function name, e.g.
        // {"patch_file": {"path": "…"}} instead of {"path": "…"}.  Unwrap one
        // level when that pattern is detected.
        if let Value::Object(ref map) = args.clone()
            && map.len() == 1
            && let Some(inner) = map.get(call.function.name.as_str())
            && inner.is_object()
        {
            args = inner.clone();
        }
        self.dispatch(call.function.name.as_str(), &args)
    }

    fn dispatch(&self, name: &str, args: &Value) -> String {
        if std::env::var("SHIO_USE_RUBY").is_ok() {
            let args_json = args.to_string();
            let result = match self.vm.lock() {
                Ok(mut guard) => guard.call_tool(name, &args_json),
                Err(_) => "Error: VM mutex poisoned".to_string(),
            };
            // Fall through only when the tool is not yet registered in Ruby.
            if !result.starts_with("Error: unknown tool:") {
                return result;
            }
        }
        match name {
            "run_shell" => self.run_shell(args),
            "patch_file" => self.patch_file(args),
            "fetch_url" => self.fetch_url(args),
            "web_search" => self.web_search(args),
            "lsp" => self.lsp_query(args),
            // Plan mode control is handled by the TUI agent loop, not here.
            "enter_plan_mode" | "exit_plan_mode" => {
                "Plan mode control is handled by the agent loop.".to_string()
            }
            _ => format!("Unknown tool: {name}"),
        }
    }

    /// Returns tool definitions for the model — Ruby-registered tools merged
    /// with the static Rust definitions for tools not yet migrated.
    /// In Phase B this always returns `all_tools()`; Phase C progressively
    /// replaces Rust entries with Ruby-sourced ones; Phase D removes the rest.
    #[allow(dead_code)] // unused until Phase D wires this in place of all_tools()
    pub fn tool_defs(&self) -> Vec<ToolDef> {
        all_tools()
    }

    // ── Individual tools ──────────────────────────────────────────────────────

    fn run_shell(&self, args: &Value) -> String {
        let command = require_str!(args, "command");

        if self.confirm_shell && !confirm(&format!("{YELLOW}Run: {command}?{RESET}")) {
            return "Aborted by user.".into();
        }

        match Command::new("sh").args(["-c", command]).output() {
            Err(e) => format!("Error running command: {e}"),
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let mut result = String::new();
                if !stdout.is_empty() {
                    result.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    result.push_str("[stderr]\n");
                    result.push_str(&stderr);
                }
                if !out.status.success() {
                    result.push_str(&format!(
                        "\n[exit code: {}]",
                        out.status.code().unwrap_or(-1)
                    ));
                }
                if result.is_empty() {
                    "(no output)".into()
                } else {
                    result
                }
            }
        }
    }
}

/// Strip the `  N │ ` line-number prefix that `read_file_range` adds to each
/// line.  If the model copies output from `read_file_range` verbatim into
/// `old_str` or `new_str`, this prevents the prefix from breaking the match.
/// Lines that do not start with the prefix are returned unchanged.
///
/// When `box_drawing_only` is `true`, only the `│` (U+2502 box-drawing)
/// separator is recognised.  Plain `|` (U+007C) is left alone so that
/// Markdown tables and similar document content are never mangled.
/// Use `box_drawing_only = false` only for `old_str` matching where the
/// model may have substituted `|` for `│`.
fn strip_line_number_prefix(s: &str, box_drawing_only: bool) -> String {
    s.lines()
        .map(|line| {
            // Match: optional spaces, digits, optional spaces, │ (or | if not
            // box_drawing_only), optional space.
            let mut chars = line.char_indices().peekable();
            // skip leading spaces
            while chars.peek().map(|(_, c)| *c == ' ').unwrap_or(false) {
                chars.next();
            }
            // must have at least one digit
            let mut has_digit = false;
            while chars
                .peek()
                .map(|(_, c)| c.is_ascii_digit())
                .unwrap_or(false)
            {
                has_digit = true;
                chars.next();
            }
            if !has_digit {
                return line;
            }
            // skip spaces between digits and separator
            while chars.peek().map(|(_, c)| *c == ' ').unwrap_or(false) {
                chars.next();
            }
            // must have │ (U+2502) — or plain | when not in box_drawing_only mode
            match chars.peek() {
                Some((_, '│')) => {
                    chars.next();
                }
                Some((_, '|')) if !box_drawing_only => {
                    chars.next();
                }
                _ => return line,
            }
            // skip one optional space after separator
            if chars.peek().map(|(_, c)| *c == ' ').unwrap_or(false) {
                chars.next();
            }
            let content_start = chars.peek().map(|(i, _)| *i).unwrap_or(line.len());
            &line[content_start..]
        })
        .collect::<Vec<_>>()
        .join("\n")
        + if s.ends_with('\n') { "\n" } else { "" }
}

impl ToolExecutor {
    fn patch_file(&self, args: &Value) -> String {
        let path = require_str!(args, "path");
        // old_str: strip both │ and | so matching tolerates either separator.
        let old_str = strip_line_number_prefix(require_str!(args, "old_str"), false);
        let old_str = old_str.as_str();
        // new_str: strip only │ (U+2502, our tool's signature).  Plain | is kept
        // so Markdown tables and similar document content are never mangled.
        let new_str_owned = strip_line_number_prefix(require_str!(args, "new_str"), true);
        let new_str = new_str_owned.as_str();

        if self.confirm_writes && !confirm(&format!("{YELLOW}Patch {path}?{RESET}")) {
            return "Aborted by user.".into();
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => return format!("Error reading {path}: {e}"),
        };

        let count = content.matches(old_str).count();
        match count {
            1 => {
                let patched = content.replacen(old_str, new_str, 1);
                return match std::fs::write(path, &patched) {
                    Ok(()) => format!(
                        "Patched {path}: old_str replaced with new_str in place. \
                         The new content is already written — do NOT call append_file or write_file.",
                    ),
                    Err(e) => format!("Error writing {path}: {e}"),
                };
            }
            n if n > 1 => {
                return format!(
                    "Error: old_str appears {n} times in {path} — \
                     make it more specific so it matches exactly once"
                );
            }
            _ => {} // 0 — fall through to line-by-line fallback
        }

        // ── Line-by-line fallback ─────────────────────────────────────────────
        // Exact substring match failed.  Try matching each line of old_str against
        // a contiguous block of file lines, trimming trailing whitespace from both
        // sides so minor spacing differences do not prevent large edits from landing.
        let old_lines: Vec<&str> = old_str.lines().collect();
        // Guard: a whitespace-only old_str would match any blank line and produce
        // false positives.  Treat it as not-found rather than risking a wrong patch.
        if old_lines.is_empty() || old_lines.iter().all(|l| l.trim().is_empty()) {
            return format!("Error: old_str not found in {path}");
        }
        let file_lines: Vec<&str> = content.lines().collect();
        let n = old_lines.len();

        let hits: Vec<usize> = (0..=file_lines.len().saturating_sub(n))
            .filter(|&i| {
                file_lines[i..i + n]
                    .iter()
                    .zip(old_lines.iter())
                    .all(|(fl, ol)| fl.trim_end() == ol.trim_end())
            })
            .collect();

        match hits.len() {
            0 => {
                // Line-by-line match found nothing.  For large old_str blocks the
                // model often misremembers interior lines while getting the edges
                // right.  Try an anchor-based match: require the first two and last
                // two lines to match exactly (trim_end), then replace the whole
                // block.  Only accepted when there is exactly one such position.
                if n >= 4 {
                    let a0 = old_lines[0].trim_end();
                    let a1 = old_lines[1].trim_end();
                    let z0 = old_lines[n - 2].trim_end();
                    let z1 = old_lines[n - 1].trim_end();
                    // Skip if both start anchors and both end anchors are blank —
                    // that would match almost anything.
                    let start_ok = !a0.trim().is_empty() || !a1.trim().is_empty();
                    let end_ok = !z0.trim().is_empty() || !z1.trim().is_empty();
                    if start_ok && end_ok {
                        let anchor_hits: Vec<usize> = (0..=file_lines.len().saturating_sub(n))
                            .filter(|&i| {
                                file_lines[i].trim_end() == a0
                                    && file_lines[i + 1].trim_end() == a1
                                    && file_lines[i + n - 2].trim_end() == z0
                                    && file_lines[i + n - 1].trim_end() == z1
                            })
                            .collect();
                        if anchor_hits.len() == 1 {
                            let start = anchor_hits[0];
                            let mut result: Vec<&str> = file_lines[..start].to_vec();
                            result.extend(new_str.lines());
                            if new_str.ends_with('\n') {
                                result.push("");
                            }
                            result.extend_from_slice(&file_lines[start + n..]);
                            let mut patched = result.join("\n");
                            if !content.ends_with('\n') && patched.ends_with('\n') {
                                patched.pop();
                            } else if content.ends_with('\n') && !patched.ends_with('\n') {
                                patched.push('\n');
                            }
                            return match std::fs::write(path, &patched) {
                                Ok(()) => format!(
                                    "Patched {path} (anchor fallback): block identified by \
                                     first/last lines of old_str replaced with new_str. \
                                     The new content is already written — do NOT call \
                                     append_file or write_file.",
                                ),
                                Err(e) => format!("Error writing {path}: {e}"),
                            };
                        }
                    }
                }
                format!("Error: old_str not found in {path}")
            }
            1 => {
                let start = hits[0];
                let mut result: Vec<&str> = file_lines[..start].to_vec();
                result.extend(new_str.lines());
                // Preserve a trailing newline that new_str.lines() would drop.
                if new_str.ends_with('\n') {
                    result.push("");
                }
                result.extend_from_slice(&file_lines[start + n..]);
                let mut patched = result.join("\n");
                // If the original file ended with '\n', the joined string already
                // ends with '\n' from the pushed "". Remove the doubled newline only
                // when the file did NOT end with '\n' originally.
                if !content.ends_with('\n') && patched.ends_with('\n') {
                    patched.pop();
                } else if content.ends_with('\n') && !patched.ends_with('\n') {
                    patched.push('\n');
                }
                match std::fs::write(path, &patched) {
                    Ok(()) => format!(
                        "Patched {path} (line-by-line fallback): old_str replaced with new_str. \
                         The new content is already written — do NOT call append_file or write_file.",
                    ),
                    Err(e) => format!("Error writing {path}: {e}"),
                }
            }
            n => format!(
                "Error: old_str matches {n} locations in {path} (line-by-line fallback) — \
                 make it more specific so it matches exactly once"
            ),
        }
    }

    fn fetch_url(&self, args: &Value) -> String {
        let url = require_str!(args, "url");
        let max_chars = args["max_chars"]
            .as_u64()
            .unwrap_or(DEFAULT_MAX_CHARS as u64) as usize;

        // Only allow http/https — no file://, ftp://, etc.
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return format!("Error: only http:// and https:// URLs are supported, got: {url}");
        }

        // Block requests to localhost and private IP ranges to prevent SSRF.
        if is_private_host(url) {
            return format!(
                "Error: requests to localhost and private network addresses are not allowed: {url}"
            );
        }

        let client = match http_client() {
            Ok(c) => c,
            Err(e) => return e,
        };

        let response = match client
            .get(url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
        {
            Ok(r) => r,
            Err(e) => return format!("Error fetching {url}: {e}"),
        };

        if !response.status().is_success() {
            return format!("Error: server returned {} for {url}", response.status());
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        // Read up to 2 MB to avoid filling memory on huge pages.
        // Truncate at a char boundary so we never split a multi-byte codepoint.
        let body = match response.text() {
            Ok(t) => {
                const BODY_LIMIT: usize = 2 * 1024 * 1024;
                if t.len() > BODY_LIMIT {
                    t[..t.floor_char_boundary(BODY_LIMIT)].to_string()
                } else {
                    t
                }
            }
            Err(e) => return format!("Error reading response body: {e}"),
        };

        let text = if content_type.contains("text/html") {
            strip_html(&body)
        } else {
            body
        };

        // Trim and truncate at a char boundary so we never split a multi-byte codepoint.
        let text = text.trim().to_string();
        if text.len() > max_chars {
            let limit = text.floor_char_boundary(max_chars);
            format!(
                "{}\n\n[… truncated at {max_chars} chars — use max_chars to get more]",
                &text[..limit]
            )
        } else {
            text
        }
    }

    fn web_search(&self, args: &Value) -> String {
        static RE_RESULT: OnceLock<regex::Regex> = OnceLock::new();
        static RE_SNIPPET: OnceLock<regex::Regex> = OnceLock::new();
        static RE_UDDG: OnceLock<regex::Regex> = OnceLock::new();

        let query = require_str!(args, "query");
        let max_results = args["max_results"].as_u64().unwrap_or(5).min(20) as usize;

        // Percent-encode the query for use in a URL.
        let mut encoded = String::new();
        for c in query.chars() {
            if c == ' ' {
                encoded.push('+');
            } else if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
                encoded.push(c);
            } else {
                for byte in c.encode_utf8(&mut [0u8; 4]).as_bytes() {
                    encoded.push_str(&format!("%{byte:02X}"));
                }
            }
        }

        let search_url = format!("https://lite.duckduckgo.com/lite/?q={encoded}");

        let client = match http_client() {
            Ok(c) => c,
            Err(e) => return e,
        };

        let body = match client
            .get(&search_url)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .and_then(|r| r.text())
        {
            Ok(b) => b,
            Err(e) => return format!("Error fetching search results: {e}"),
        };

        // DDG lite wraps result links in <a href="...uddg=REAL_URL...">Title</a>.
        let re_result = RE_RESULT.get_or_init(|| {
            regex::Regex::new(r#"(?s)<a[^>]+href="[^"]*uddg=[^"]*"[^>]*>(.*?)</a>"#).unwrap()
        });
        // Snippets appear in <td class="result-snippet">…</td>.
        let re_snippet = RE_SNIPPET.get_or_init(|| {
            regex::Regex::new(r#"(?s)<td[^>]*class="result-snippet"[^>]*>(.*?)</td>"#).unwrap()
        });
        // Extract the real URL from the uddg= redirect parameter.
        let re_uddg = RE_UDDG.get_or_init(|| regex::Regex::new(r"uddg=([^&\s]+)").unwrap());

        let results: Vec<(String, String)> = re_result
            .captures_iter(&body)
            .filter_map(|cap| {
                let full = cap.get(0)?.as_str();
                let title = strip_html(cap.get(1)?.as_str()).trim().to_string();
                if title.is_empty() {
                    return None;
                }
                let real_url = re_uddg
                    .captures(full)
                    .and_then(|c| c.get(1))
                    .map(|m| percent_decode(m.as_str()))
                    .unwrap_or_default();
                if real_url.is_empty() {
                    return None;
                }
                Some((title, real_url))
            })
            .take(max_results)
            .collect();

        if results.is_empty() {
            return format!(
                "No results found for: {query}\n\
                 (try rephrasing or use fetch_url with a known URL)"
            );
        }

        let snippets: Vec<String> = re_snippet
            .captures_iter(&body)
            .map(|cap| {
                strip_html(cap.get(1).map_or("", |m| m.as_str()))
                    .trim()
                    .to_string()
            })
            .take(max_results)
            .collect();

        let mut out = format!("Web search results for \"{query}\":\n\n");
        for (i, (title, url)) in results.iter().enumerate() {
            out.push_str(&format!("{}. {title}\n   {url}\n", i + 1));
            if let Some(snippet) = snippets.get(i)
                && !snippet.is_empty()
            {
                out.push_str(&format!("   {snippet}\n"));
            }
            out.push('\n');
        }
        out.trim_end().to_string()
    }

    fn lsp_query(&self, args: &Value) -> String {
        let operation = args["operation"].as_str().unwrap_or("hover");
        let file = require_str!(args, "file");
        let line = args["line"].as_u64().unwrap_or(1) as u32;
        let column = args["column"].as_u64().unwrap_or(1) as u32;
        crate::lsp::query(operation, file, line, column, &self.lsp)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Decode a percent-encoded URL string (e.g. `%2F` → `/`, `+` → space).
/// Handles multi-byte UTF-8 sequences correctly.
fn percent_decode(s: &str) -> String {
    let input = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%'
            && i + 2 < input.len()
            && input[i + 1].is_ascii_hexdigit()
            && input[i + 2].is_ascii_hexdigit()
        {
            let hi = (input[i + 1] as char).to_digit(16).unwrap() as u8;
            let lo = (input[i + 2] as char).to_digit(16).unwrap() as u8;
            out.push(hi << 4 | lo);
            i += 3;
        } else if input[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Strip HTML markup from a page, returning readable plain text.
///
/// Strategy:
/// 1. Remove `<script>` and `<style>` blocks with their content entirely.
/// 2. Replace block-level tags with newlines so paragraphs stay separate.
/// 3. Strip all remaining tags.
/// 4. Decode common HTML entities.
/// 5. Collapse runs of whitespace.
///
/// All regexes are compiled once and reused across calls via `OnceLock`.
fn strip_html(html: &str) -> String {
    static RE_SCRIPT: OnceLock<regex::Regex> = OnceLock::new();
    static RE_STYLE: OnceLock<regex::Regex> = OnceLock::new();
    static RE_BLOCK: OnceLock<regex::Regex> = OnceLock::new();
    static RE_TAG: OnceLock<regex::Regex> = OnceLock::new();
    static RE_SPACES: OnceLock<regex::Regex> = OnceLock::new();
    static RE_NEWLINES: OnceLock<regex::Regex> = OnceLock::new();

    let re_script =
        RE_SCRIPT.get_or_init(|| regex::Regex::new(r"(?si)<script[^>]*>.*?</script>").unwrap());
    let re_style =
        RE_STYLE.get_or_init(|| regex::Regex::new(r"(?si)<style[^>]*>.*?</style>").unwrap());
    let re_block = RE_BLOCK.get_or_init(|| {
        regex::Regex::new(
            r"(?i)</?(?:p|div|h[1-6]|li|tr|br|hr|blockquote|pre|article|section|header|footer|nav|main)[^>]*>",
        )
        .unwrap()
    });
    let re_tag = RE_TAG.get_or_init(|| regex::Regex::new(r"<[^>]+>").unwrap());
    let re_spaces = RE_SPACES.get_or_init(|| regex::Regex::new(r"[ \t]+").unwrap());
    let re_newlines = RE_NEWLINES.get_or_init(|| regex::Regex::new(r"\n{3,}").unwrap());

    // 1. Drop script / style blocks (content included).
    let s = re_script.replace_all(html, " ");
    let s = re_style.replace_all(&s, " ");

    // 2. Block-level tags → newline so paragraphs break visually.
    let s = re_block.replace_all(&s, "\n");

    // 3. Strip all remaining tags.
    let s = re_tag.replace_all(&s, "");

    // 4. Decode common HTML entities.
    let s = s
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…")
        .replace("&laquo;", "«")
        .replace("&raquo;", "»");

    // 5. Collapse runs of whitespace; preserve paragraph breaks (2+ newlines → blank line).
    let s = re_spaces.replace_all(&s, " ");
    let s = re_newlines.replace_all(&s, "\n\n");

    s.trim().to_string()
}

/// Return `true` if the URL's host is localhost, a loopback address, or a
/// private/link-local IP range — any destination that should not be reachable
/// from an SSRF attack.
fn is_private_host(url: &str) -> bool {
    // Strip scheme.
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    // Strip path, query, fragment — everything after the first '/', '?', '#'.
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);

    // Strip userinfo (e.g. "user:pass@host" → "host").  Without this an
    // attacker could bypass the private-host check with a URL like
    // http://x@192.168.1.1/ whose authority parses as host "x@192.168.1.1",
    // which fails the IPv4 parse and slips through.
    let host_with_port = authority.rsplit('@').next().unwrap_or(authority);

    // Strip port.  IPv6 addresses are enclosed in brackets: [::1]:8080.
    let host = if host_with_port.starts_with('[') {
        host_with_port
            .trim_start_matches('[')
            .split(']')
            .next()
            .unwrap_or(host_with_port)
    } else {
        // For plain hostnames / IPv4 we can't just split on ':' because colons
        // appear in IPv6 too — but we've already handled the bracket form above,
        // so here it's safe to strip the port with rsplit_once.
        host_with_port
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(host_with_port)
    };

    let host_lc = host.to_ascii_lowercase();

    // Textual localhost / unspecified.
    if matches!(host_lc.as_str(), "localhost" | "::1" | "0.0.0.0") {
        return true;
    }
    // .local mDNS names resolve to the LAN.
    if host_lc.ends_with(".local") {
        return true;
    }

    // IPv4 private / loopback / link-local ranges.
    if let Ok(ipv4) = host.parse::<std::net::Ipv4Addr>() {
        let o = ipv4.octets();
        return o[0] == 127                                    // 127.0.0.0/8 loopback
            || o[0] == 10                                     // 10.0.0.0/8
            || (o[0] == 172 && (16..=31).contains(&o[1]))    // 172.16.0.0/12
            || (o[0] == 192 && o[1] == 168)                  // 192.168.0.0/16
            || (o[0] == 169 && o[1] == 254); // 169.254.0.0/16 IMDS / link-local
    }

    // IPv6: loopback, unique-local (fc00::/7), and link-local (fe80::/10).
    if let Ok(ipv6) = host.parse::<std::net::Ipv6Addr>() {
        let segs = ipv6.segments();
        return ipv6.is_loopback()                          // ::1
            || (segs[0] & 0xfe00) == 0xfc00               // fc00::/7  unique local
            || (segs[0] & 0xffc0) == 0xfe80; // fe80::/10 link-local
    }

    false
}

fn confirm(prompt: &str) -> bool {
    eprint!("{prompt} [y/N] ");
    io::stderr().flush().ok();
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).ok();
    answer.trim().eq_ignore_ascii_case("y")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn executor(confirm_writes: bool, confirm_shell: bool) -> ToolExecutor {
        ToolExecutor {
            confirm_writes,
            confirm_shell,
            ..Default::default()
        }
    }

    // ── read_file ─────────────────────────────────────────────────────────────

    #[test]
    fn read_file_returns_content() {
        let path = std::env::temp_dir().join("shio_tool_read.txt");
        fs::write(&path, "hello tool").unwrap();
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "read_file",
            &serde_json::json!({ "path": path.to_str().unwrap() }).to_string(),
        );
        assert_eq!(result, "hello tool");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn read_file_missing_returns_error() {
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "read_file",
            &serde_json::json!({ "path": "/nonexistent/shio_missing.txt" }).to_string(),
        );
        assert!(result.starts_with("Error"), "{result}");
    }

    // ── write_file ────────────────────────────────────────────────────────────

    #[test]
    fn write_file_creates_file() {
        let path = std::env::temp_dir().join("shio_tool_write.txt");
        let _ = fs::remove_file(&path);
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "write_file",
            &serde_json::json!({ "path": path.to_str().unwrap(), "content": "written" })
                .to_string(),
        );
        assert!(
            result.contains("written") || result.contains("bytes"),
            "{result}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "written");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn write_file_preserves_pipe_table_content() {
        // write_file must NOT strip "  N | text" patterns — users may write
        // loose documents with pipe-separated tables or similar structure.
        let path = std::env::temp_dir().join("shio_write_preserve.txt");
        let _ = fs::remove_file(&path);
        let ex = executor(false, false);
        ex.vm.lock().unwrap().call_tool(
            "write_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "content": "  3 | value\n| col | col |"
            })
            .to_string(),
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "  3 | value\n| col | col |"
        );
        let _ = fs::remove_file(&path);
    }

    // ── insert_after_line ─────────────────────────────────────────────────────

    #[test]
    fn insert_after_line_inserts_in_middle() {
        let path = std::env::temp_dir().join("shio_insert_mid.txt");
        fs::write(&path, "line1\nline2\nline3\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "insert_after_line",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "line": 2,
                "content": "inserted"
            })
            .to_string(),
        );
        assert!(out.contains("Inserted"), "{out}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "line1\nline2\ninserted\nline3\n"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn insert_after_line_at_end_appends() {
        let path = std::env::temp_dir().join("shio_insert_end.txt");
        fs::write(&path, "line1\nline2\n").unwrap();
        let ex = executor(false, false);
        ex.vm.lock().unwrap().call_tool(
            "insert_after_line",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "line": 2,
                "content": "appended"
            })
            .to_string(),
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "line1\nline2\nappended\n"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn insert_after_line_after_line_zero_prepends() {
        let path = std::env::temp_dir().join("shio_insert_zero.txt");
        fs::write(&path, "line1\nline2\n").unwrap();
        let ex = executor(false, false);
        ex.vm.lock().unwrap().call_tool(
            "insert_after_line",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "line": 0,
                "content": "prepended"
            })
            .to_string(),
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "prepended\nline1\nline2\n"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn insert_after_line_out_of_range_returns_error() {
        let path = std::env::temp_dir().join("shio_insert_oor.txt");
        fs::write(&path, "line1\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "insert_after_line",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "line": 99,
                "content": "x"
            })
            .to_string(),
        );
        assert!(out.starts_with("Error"), "{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn insert_after_line_accepts_string_line_number() {
        // Local models sometimes stringify numbers; must tolerate "42" as well as 42.
        let path = std::env::temp_dir().join("shio_insert_strnum.txt");
        fs::write(&path, "line1\nline2\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "insert_after_line",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "line": "1",
                "content": "inserted"
            })
            .to_string(),
        );
        assert!(!out.starts_with("Error"), "{out}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "line1\ninserted\nline2\n"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn insert_after_line_preserves_pipe_table_content() {
        // insert_after_line must NOT strip "  N | text" — same reason as write_file.
        let path = std::env::temp_dir().join("shio_insert_preserve.txt");
        fs::write(&path, "line1\nline2\n").unwrap();
        let ex = executor(false, false);
        ex.vm.lock().unwrap().call_tool(
            "insert_after_line",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "line": 1,
                "content": "  3 | value"
            })
            .to_string(),
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "line1\n  3 | value\nline2\n"
        );
        let _ = fs::remove_file(&path);
    }

    // ── list_directory ────────────────────────────────────────────────────────

    #[test]
    fn list_directory_shows_entries() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "list_directory",
            &serde_json::json!({ "path": "src" }).to_string(),
        );
        assert!(out.contains("main.rs"), "{out}");
    }

    // ── run_shell ─────────────────────────────────────────────────────────────

    #[test]
    fn run_shell_captures_stdout() {
        let ex = executor(false, false);
        let args = serde_json::json!({ "command": "echo hello" });
        let out = ex.run_shell(&args);
        assert!(out.contains("hello"), "{out}");
    }

    #[test]
    fn run_shell_includes_exit_code_on_failure() {
        let ex = executor(false, false);
        let args = serde_json::json!({ "command": "exit 1" });
        let out = ex.run_shell(&args);
        assert!(out.contains("exit code"), "{out}");
    }

    // ── search_files ─────────────────────────────────────────────────────────

    #[test]
    fn search_files_finds_rust_sources() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "search_files",
            &serde_json::json!({ "pattern": "src/*.rs" }).to_string(),
        );
        assert!(out.contains("main.rs"), "{out}");
    }

    // ── grep_files ────────────────────────────────────────────────────────────

    #[test]
    fn grep_files_finds_pattern() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "grep_files",
            &serde_json::json!({ "pattern": "fn main", "path": "src/main.rs" }).to_string(),
        );
        assert!(out.contains("fn main"), "{out}");
    }

    // ── read_file_range ───────────────────────────────────────────────────────

    #[test]
    fn read_file_range_returns_numbered_lines() {
        let path = std::env::temp_dir().join("shio_range.txt");
        fs::write(&path, "line1\nline2\nline3\nline4\nline5\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "read_file_range",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "start_line": 2,
                "end_line": 4
            })
            .to_string(),
        );
        assert!(out.contains("line2"), "{out}");
        assert!(out.contains("line4"), "{out}");
        assert!(!out.contains("line1"), "{out}");
        assert!(!out.contains("line5"), "{out}");
        // Header must report the range so the model knows where it is.
        assert!(out.contains("Lines 2"), "{out}");
        assert!(out.contains("Lines 2") && out.contains('4'), "{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn read_file_range_without_end_reads_to_eof() {
        let path = std::env::temp_dir().join("shio_range2.txt");
        fs::write(&path, "a\nb\nc\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "read_file_range",
            &serde_json::json!({ "path": path.to_str().unwrap(), "start_line": 2 }).to_string(),
        );
        assert!(out.contains('b'), "{out}");
        assert!(out.contains('c'), "{out}");
        // "a\n" would be the first line content; the header may contain path chars
        assert!(!out.contains("\na\n") && !out.ends_with("\na"), "{out}");
        let _ = fs::remove_file(&path);
    }

    // ── strip_line_number_prefix ──────────────────────────────────────────────

    #[test]
    fn strip_prefix_removes_box_drawing_prefix() {
        // read_file_range format: "    2 │ content" — stripped in both modes.
        assert_eq!(strip_line_number_prefix("    2 │ hello", false), "hello");
        assert_eq!(strip_line_number_prefix("    2 │ hello", true), "hello");
    }

    #[test]
    fn strip_prefix_removes_pipe_prefix_in_full_mode() {
        // plain | is stripped when box_drawing_only=false (old_str matching).
        assert_eq!(
            strip_line_number_prefix("   21 | content here", false),
            "content here"
        );
    }

    #[test]
    fn strip_prefix_preserves_pipe_prefix_in_box_drawing_only_mode() {
        // plain | is preserved when box_drawing_only=true (write paths).
        assert_eq!(
            strip_line_number_prefix("   21 | content here", true),
            "   21 | content here"
        );
    }

    #[test]
    fn strip_prefix_leaves_plain_lines_unchanged() {
        assert_eq!(
            strip_line_number_prefix("no prefix at all", false),
            "no prefix at all"
        );
        assert_eq!(
            strip_line_number_prefix("no prefix at all", true),
            "no prefix at all"
        );
    }

    #[test]
    fn strip_prefix_handles_multiline_mixed() {
        let input = "    1 │ first\nsecond line\n    3 │ third";
        let out = strip_line_number_prefix(input, false);
        assert_eq!(out, "first\nsecond line\nthird");
    }

    #[test]
    fn patch_file_tolerates_line_number_prefix_in_old_str() {
        let path = std::env::temp_dir().join("shio_patch_prefix.txt");
        fs::write(&path, "hello world\n").unwrap();
        let ex = executor(false, false);
        // old_str: │ prefix stripped for matching (both separators).
        // new_str: │ prefix also stripped (box-drawing only); plain | preserved.
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "    1 │ hello world",
            "new_str": "    1 │ goodbye world",
        });
        let out = ex.patch_file(&args);
        assert!(out.contains("Patched") || out.contains("bytes"), "{out}");
        // │ prefix stripped from new_str, so file contains clean content.
        assert_eq!(fs::read_to_string(&path).unwrap(), "goodbye world\n");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_new_str_preserves_plain_pipe() {
        let path = std::env::temp_dir().join("shio_patch_pipe.txt");
        fs::write(&path, "old content\n").unwrap();
        let ex = executor(false, false);
        // Plain | in new_str must be kept (Markdown table content).
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "old content",
            "new_str": "  3 | table value",
        });
        ex.patch_file(&args);
        assert_eq!(fs::read_to_string(&path).unwrap(), "  3 | table value\n");
        let _ = fs::remove_file(&path);
    }

    // ── patch_file ────────────────────────────────────────────────────────────

    #[test]
    fn patch_file_replaces_exact_match() {
        let path = std::env::temp_dir().join("shio_patch.txt");
        fs::write(&path, "hello world\n").unwrap();
        let ex = executor(false, false);
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "hello",
            "new_str": "goodbye"
        });
        let out = ex.patch_file(&args);
        assert!(
            out.contains("Patched") || out.contains("patched") || out.contains("bytes"),
            "{out}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "goodbye world\n");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_errors_when_old_str_not_found() {
        let path = std::env::temp_dir().join("shio_patch2.txt");
        fs::write(&path, "hello world\n").unwrap();
        let ex = executor(false, false);
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "nonexistent",
            "new_str": "x"
        });
        let out = ex.patch_file(&args);
        assert!(out.starts_with("Error"), "{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_errors_when_old_str_ambiguous() {
        let path = std::env::temp_dir().join("shio_patch3.txt");
        fs::write(&path, "a a a\n").unwrap();
        let ex = executor(false, false);
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "a",
            "new_str": "b"
        });
        let out = ex.patch_file(&args);
        assert!(out.starts_with("Error"), "{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_fallback_tolerates_trailing_whitespace() {
        let path = std::env::temp_dir().join("shio_patch_fallback.txt");
        // File has trailing spaces on the second line.
        fs::write(&path, "fn foo() {\n    let x = 1;   \n}\n").unwrap();
        let ex = executor(false, false);
        // old_str has no trailing spaces — exact match would fail.
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "fn foo() {\n    let x = 1;\n}",
            "new_str": "fn foo() {\n    let x = 2;\n}",
        });
        let out = ex.patch_file(&args);
        assert!(out.contains("Patched"), "{out}");
        assert!(out.contains("fallback"), "{out}");
        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("let x = 2;"), "{result}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_fallback_errors_when_ambiguous() {
        let path = std::env::temp_dir().join("shio_patch_fallback_ambig.txt");
        // Two identical blocks (old_str matches both via line-by-line).
        fs::write(&path, "fn a() {}\nfn a() {}\n").unwrap();
        let ex = executor(false, false);
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "fn a() {}",
            "new_str": "fn b() {}",
        });
        let out = ex.patch_file(&args);
        assert!(out.starts_with("Error"), "{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_fallback_rejects_whitespace_only_old_str() {
        // A whitespace-only old_str would match every blank line — must be rejected.
        let path = std::env::temp_dir().join("shio_patch_ws_guard.txt");
        fs::write(&path, "a\n\nb\n").unwrap();
        let ex = executor(false, false);
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "   ",   // all whitespace
            "new_str": "x",
        });
        let out = ex.patch_file(&args);
        assert!(out.starts_with("Error"), "{out}");
        // File must be unchanged.
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\n\nb\n");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_fallback_preserves_trailing_newline_in_new_str() {
        // new_str ending with '\n' must be preserved verbatim (not dropped by lines()).
        let path = std::env::temp_dir().join("shio_patch_trail_nl.txt");
        fs::write(&path, "fn foo() {   \n}\n").unwrap(); // trailing space triggers fallback
        let ex = executor(false, false);
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "fn foo() {\n}",
            "new_str": "fn foo() {\n    42\n}\n",
        });
        let out = ex.patch_file(&args);
        assert!(out.contains("Patched"), "{out}");
        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("42"), "{result}");
        // File should end with exactly one newline.
        assert!(
            result.ends_with('\n'),
            "missing trailing newline: {result:?}"
        );
        assert!(
            !result.ends_with("\n\n"),
            "double trailing newline: {result:?}"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_anchor_fallback_tolerates_wrong_interior_lines() {
        // Simulate a model that reproduced 5 lines but got the middle one wrong.
        // The file has the real content; old_str has a mutated interior line.
        let path = std::env::temp_dir().join("shio_patch_anchor.txt");
        fs::write(
            &path,
            "fn foo() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n}\n",
        )
        .unwrap();
        let ex = executor(false, false);
        // Middle line differs from the file — exact and line-by-line fallbacks both fail.
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "fn foo() {\n    let a = 1;\n    let b = WRONG;\n    let c = 3;\n}",
            "new_str": "fn foo() {\n    42\n}",
        });
        let out = ex.patch_file(&args);
        assert!(out.contains("Patched"), "{out}");
        assert!(out.contains("anchor"), "{out}");
        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("42"), "{result}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_anchor_fallback_rejects_ambiguous_anchors() {
        // Two blocks with the same first two and last two lines — anchor match must
        // refuse rather than silently pick one.
        let path = std::env::temp_dir().join("shio_patch_anchor_ambig.txt");
        fs::write(
            &path,
            "fn foo() {\n    let a = 1;\n    x\n    let z = 9;\n}\nfn foo() {\n    let a = 1;\n    y\n    let z = 9;\n}\n",
        )
        .unwrap();
        let ex = executor(false, false);
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "fn foo() {\n    let a = 1;\n    WRONG\n    let z = 9;\n}",
            "new_str": "fn bar() {}",
        });
        let out = ex.patch_file(&args);
        assert!(out.starts_with("Error"), "{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn execute_quiet_unwraps_function_name_wrapped_args() {
        // Some local models send {"patch_file": {"path": …}} instead of {"path": …}.
        use crate::client::{ToolCallFunction, ToolCallItem};
        let path = std::env::temp_dir().join("shio_wrap_args.txt");
        fs::write(&path, "hello world").unwrap();
        let ex = executor(false, false);
        let call = ToolCallItem {
            id: "x".into(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: "patch_file".into(),
                arguments: serde_json::json!({
                    "patch_file": {
                        "path": path.to_str().unwrap(),
                        "old_str": "hello",
                        "new_str": "goodbye"
                    }
                })
                .to_string(),
            },
        };
        let out = ex.execute_quiet(&call);
        assert!(out.contains("Patched"), "{out}");
        let _ = fs::remove_file(&path);
    }

    // ── delete_file ───────────────────────────────────────────────────────────

    #[test]
    fn delete_file_removes_existing_file() {
        let path = std::env::temp_dir().join("shio_delete.txt");
        fs::write(&path, "bye").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "delete_file",
            &serde_json::json!({ "path": path.to_str().unwrap() }).to_string(),
        );
        assert!(out.contains("Deleted"), "{out}");
        assert!(!path.exists());
    }

    #[test]
    fn delete_file_errors_on_missing_file() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "delete_file",
            &serde_json::json!({ "path": "/nonexistent/shio_gone.txt" }).to_string(),
        );
        assert!(out.starts_with("Error"), "{out}");
    }

    // ── move_file ─────────────────────────────────────────────────────────────

    #[test]
    fn move_file_renames_file() {
        let src = std::env::temp_dir().join("shio_move_src.txt");
        let dst = std::env::temp_dir().join("shio_move_dst.txt");
        let _ = fs::remove_file(&dst);
        fs::write(&src, "content").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "move_file",
            &serde_json::json!({
                "src": src.to_str().unwrap(),
                "dst": dst.to_str().unwrap()
            })
            .to_string(),
        );
        assert!(out.contains("Moved") || out.contains("→"), "{out}");
        assert!(!src.exists());
        assert!(dst.exists());
        let _ = fs::remove_file(&dst);
    }

    // ── all_tools ─────────────────────────────────────────────────────────────

    #[test]
    fn all_tools_has_twenty_two_entries() {
        assert_eq!(all_tools().len(), 22);
    }

    // ── lsp_query ─────────────────────────────────────────────────────────────

    #[test]
    fn lsp_query_unsupported_extension_returns_error_message() {
        // .xyz is not a known language; no server will be found without an install.
        // This exercises lsp_query dispatch all the way through lsp::query without
        // needing a real LSP server.
        let ex = executor(false, false);
        let args = serde_json::json!({
            "operation": "hover",
            "file": "test.xyz",
            "line": 1,
            "column": 1
        });
        let result = ex.lsp_query(&args);
        assert!(
            result.contains("No LSP server found") || result.contains("Error"),
            "expected error message, got: {result}"
        );
    }

    #[test]
    fn lsp_query_missing_file_argument() {
        let ex = executor(false, false);
        let args = serde_json::json!({ "operation": "hover" });
        let result = ex.lsp_query(&args);
        assert!(result.contains("missing 'file'"), "got: {result}");
    }

    #[test]
    fn lsp_query_dispatched_via_execute_quiet() {
        let ex = executor(false, false);
        let call = crate::client::ToolCallItem {
            id: "call1".to_string(),
            kind: "function".to_string(),
            function: crate::client::ToolCallFunction {
                name: "lsp".to_string(),
                arguments: r#"{"operation":"hover","file":"test.xyz","line":1}"#.to_string(),
            },
        };
        let result = ex.execute_quiet(&call);
        // Should not return "Unknown tool" — the dispatch must reach lsp_query.
        assert!(!result.contains("Unknown tool"), "got: {result}");
    }

    // ── strip_html ────────────────────────────────────────────────────────────

    #[test]
    fn strip_html_removes_tags_and_decodes_entities() {
        let html = "<p>Hello &amp; <b>world</b>!&nbsp;It&#39;s fine.</p>";
        let out = strip_html(html);
        assert!(out.contains("Hello & world!"), "{out}");
        assert!(out.contains("It's fine."), "{out}");
        assert!(!out.contains('<'), "{out}");
    }

    #[test]
    fn strip_html_drops_script_and_style_content() {
        let html = "<style>body{color:red}</style><script>alert(1)</script><p>visible</p>";
        let out = strip_html(html);
        assert!(out.contains("visible"), "{out}");
        assert!(!out.contains("color:red"), "{out}");
        assert!(!out.contains("alert"), "{out}");
    }

    // ── fetch_url (scheme guard, no network) ─────────────────────────────────

    #[test]
    fn fetch_url_rejects_non_http_schemes() {
        let ex = executor(false, false);
        let result = ex.fetch_url(&serde_json::json!({ "url": "file:///etc/passwd" }));
        assert!(result.starts_with("Error:"), "{result}");
        assert!(result.contains("http"), "{result}");
    }

    #[test]
    fn fetch_url_requires_url_argument() {
        let ex = executor(false, false);
        let result = ex.fetch_url(&serde_json::json!({}));
        assert!(result.starts_with("Error"), "{result}");
    }

    // ── create_directory ──────────────────────────────────────────────────────

    #[test]
    fn create_directory_creates_nested_dirs() {
        let dir = std::env::temp_dir().join("shio_test_mkdir/a/b/c");
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "create_directory",
            &serde_json::json!({ "path": dir.to_str().unwrap() }).to_string(),
        );
        assert!(result.contains("Created"), "{result}");
        assert!(dir.is_dir());
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("shio_test_mkdir"));
    }

    #[test]
    fn create_directory_is_idempotent() {
        let dir = std::env::temp_dir().join("shio_test_mkdir_exist");
        std::fs::create_dir_all(&dir).unwrap();
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "create_directory",
            &serde_json::json!({ "path": dir.to_str().unwrap() }).to_string(),
        );
        assert!(result.contains("Created"), "{result}");
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn create_directory_requires_path() {
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool("create_directory", "{}");
        assert!(result.starts_with("Error"), "{result}");
    }

    // ── get_working_directory ─────────────────────────────────────────────────

    #[test]
    fn get_working_directory_returns_nonempty_path() {
        let ex = executor(false, false);
        let result = ex
            .vm
            .lock()
            .unwrap()
            .call_tool("get_working_directory", "{}");
        assert!(!result.is_empty());
        assert!(!result.starts_with("Error"), "{result}");
    }

    // ── web_search ────────────────────────────────────────────────────────────

    #[test]
    fn web_search_requires_query() {
        let ex = executor(false, false);
        let result = ex.web_search(&serde_json::json!({}));
        assert!(result.starts_with("Error"), "{result}");
    }

    // ── save_memory ───────────────────────────────────────────────────────────

    #[test]
    fn save_memory_appends_to_file() {
        let path = std::env::temp_dir().join("shio_test_memory.md");
        let _ = fs::remove_file(&path);
        let path_str = path.to_str().unwrap();
        let ex = executor(false, false);

        let result = ex.vm.lock().unwrap().call_tool(
            "save_memory",
            &serde_json::json!({ "memory": "prefer snake_case", "file": path_str }).to_string(),
        );
        assert!(result.contains("Saved"), "{result}");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("prefer snake_case"), "{content}");

        // Duplicate is skipped.
        let result2 = ex.vm.lock().unwrap().call_tool(
            "save_memory",
            &serde_json::json!({ "memory": "prefer snake_case", "file": path_str }).to_string(),
        );
        assert!(result2.contains("skipped"), "{result2}");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_memory_requires_memory_arg() {
        let ex = executor(false, false);
        let result = ex
            .vm
            .lock()
            .unwrap()
            .call_tool("save_memory", &serde_json::json!({}).to_string());
        assert!(result.starts_with("Error"), "{result}");
    }

    // ── read_many_files ───────────────────────────────────────────────────────

    #[test]
    fn read_many_files_returns_all_contents() {
        let a = std::env::temp_dir().join("shio_rmf_a.txt");
        let b = std::env::temp_dir().join("shio_rmf_b.txt");
        fs::write(&a, "content_a").unwrap();
        fs::write(&b, "content_b").unwrap();
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "read_many_files",
            &serde_json::json!({
                "paths": [a.to_str().unwrap(), b.to_str().unwrap()]
            })
            .to_string(),
        );
        assert!(result.contains("content_a"), "{result}");
        assert!(result.contains("content_b"), "{result}");
        let _ = fs::remove_file(&a);
        let _ = fs::remove_file(&b);
    }

    #[test]
    fn read_many_files_reports_missing_file_inline() {
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "read_many_files",
            &serde_json::json!({
                "paths": ["/nonexistent/shio_rmf_missing.txt"]
            })
            .to_string(),
        );
        assert!(result.contains("Error"), "{result}");
    }

    #[test]
    fn read_many_files_requires_paths() {
        let ex = executor(false, false);
        let result = ex
            .vm
            .lock()
            .unwrap()
            .call_tool("read_many_files", &serde_json::json!({}).to_string());
        assert!(result.starts_with("Error"), "{result}");
    }

    // ── write_todos ───────────────────────────────────────────────────────────

    #[test]
    fn write_todos_creates_file_with_checkboxes() {
        let path = std::env::temp_dir().join("shio_todos_test.md");
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "write_todos",
            &serde_json::json!({
                "todos": [
                    { "task": "first task", "status": "completed" },
                    { "task": "second task", "status": "in_progress" },
                    { "task": "third task" }
                ],
                "file": path.to_str().unwrap()
            })
            .to_string(),
        );
        assert!(result.contains("3"), "{result}");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("[x] first task"), "{content}");
        assert!(content.contains("[-] second task"), "{content}");
        assert!(content.contains("[ ] third task"), "{content}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn write_todos_requires_todos() {
        let ex = executor(false, false);
        let result = ex
            .vm
            .lock()
            .unwrap()
            .call_tool("write_todos", &serde_json::json!({}).to_string());
        assert!(result.starts_with("Error"), "{result}");
    }

    // ── percent_decode ────────────────────────────────────────────────────────

    #[test]
    fn percent_decode_handles_basic_encoding() {
        assert_eq!(percent_decode("hello+world"), "hello world");
        assert_eq!(percent_decode("foo%2Fbar"), "foo/bar");
        assert_eq!(percent_decode("a%20b"), "a b");
    }

    #[test]
    fn percent_decode_handles_utf8_sequences() {
        // "é" encodes as %C3%A9 in UTF-8
        assert_eq!(percent_decode("%C3%A9"), "é");
    }

    // ── is_private_host ───────────────────────────────────────────────────────

    #[test]
    fn is_private_host_localhost() {
        assert!(is_private_host("http://localhost/path"));
        assert!(is_private_host("https://localhost:8080/x"));
    }

    #[test]
    fn is_private_host_loopback_ipv4() {
        assert!(is_private_host("http://127.0.0.1/"));
        assert!(is_private_host("http://127.1.2.3/"));
    }

    #[test]
    fn is_private_host_private_ipv4_ranges() {
        assert!(is_private_host("http://10.0.0.1/"));
        assert!(is_private_host("http://10.255.255.255/"));
        assert!(is_private_host("http://172.16.0.1/"));
        assert!(is_private_host("http://172.31.255.255/"));
        assert!(is_private_host("http://192.168.1.100/"));
    }

    #[test]
    fn is_private_host_link_local_imds() {
        // 169.254.169.254 is the AWS/GCP IMDS endpoint — must be blocked.
        assert!(is_private_host("http://169.254.169.254/latest/meta-data/"));
    }

    #[test]
    fn is_private_host_ipv6_loopback() {
        assert!(is_private_host("http://[::1]/"));
        assert!(is_private_host("http://[::1]:9000/"));
    }

    #[test]
    fn is_private_host_ipv6_unique_local() {
        // fc00::/7 — unique local addresses (fd00:: is in range, fc00:: is too)
        assert!(is_private_host("http://[fd12:3456:789a::1]/"));
        assert!(is_private_host("http://[fc00::1]/path"));
    }

    #[test]
    fn is_private_host_ipv6_link_local() {
        // fe80::/10 — link-local addresses
        assert!(is_private_host("http://[fe80::1]/"));
        assert!(is_private_host("http://[fe80::dead:beef]:8080/api"));
    }

    #[test]
    fn is_private_host_ipv6_public_returns_false() {
        // 2001:db8::/32 is documentation range (publicly routable prefix)
        assert!(!is_private_host("https://[2001:db8::1]/"));
        assert!(!is_private_host("https://[2606:4700:4700::1111]/dns")); // Cloudflare
    }

    #[test]
    fn is_private_host_mdns_local() {
        assert!(is_private_host("http://mydevice.local/api"));
    }

    #[test]
    fn is_private_host_public_address_returns_false() {
        assert!(!is_private_host("https://example.com/"));
        assert!(!is_private_host("https://8.8.8.8/dns"));
        assert!(!is_private_host("https://172.32.0.1/")); // just outside 172.16-31
    }

    #[test]
    fn is_private_host_strips_port_correctly() {
        assert!(is_private_host("http://192.168.0.1:3000/api"));
        assert!(!is_private_host("http://93.184.216.34:443/"));
    }

    // ── write_file — parent directory auto-creation ───────────────────────────

    #[test]
    fn write_file_creates_parent_dirs() {
        let dir = std::env::temp_dir().join("shio_write_nested/a/b");
        let path = dir.join("out.txt");
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "write_file",
            &serde_json::json!({ "path": path.to_str().unwrap(), "content": "nested" }).to_string(),
        );
        assert!(
            result.contains("bytes") || result.contains("nested"),
            "{result}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "nested");
        let _ = fs::remove_dir_all(std::env::temp_dir().join("shio_write_nested"));
    }

    // ── run_shell — missing command argument ──────────────────────────────────

    #[test]
    fn run_shell_missing_command_returns_error() {
        let ex = executor(false, false);
        let out = ex.run_shell(&serde_json::json!({}));
        assert!(
            out.starts_with("Error"),
            "expected error for missing command, got: {out}"
        );
    }

    // ── grep_files — case-insensitive and invalid regex ───────────────────────

    #[test]
    fn grep_files_case_insensitive_flag() {
        let path = std::env::temp_dir().join("shio_grep_ci.txt");
        fs::write(&path, "Hello World\nlower case\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "grep_files",
            &serde_json::json!({
                "pattern": "hello",
                "path": path.to_str().unwrap(),
                "case_insensitive": true
            })
            .to_string(),
        );
        assert!(out.contains("Hello World"), "got: {out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn grep_files_invalid_regex_returns_error() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "grep_files",
            &serde_json::json!({ "pattern": "[invalid(regex", "path": "src" }).to_string(),
        );
        assert!(
            out.contains("Invalid regex") || out.starts_with("Error"),
            "got: {out}"
        );
    }

    // ── append_file ───────────────────────────────────────────────────────────

    #[test]
    fn append_file_creates_new_file() {
        let path = std::env::temp_dir().join("shio_append_new.txt");
        let _ = fs::remove_file(&path);
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "append_file",
            &serde_json::json!({ "path": path.to_str().unwrap(), "content": "hello" }).to_string(),
        );
        assert!(out.contains("Appended"), "{out}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn append_file_preserves_existing_content() {
        let path = std::env::temp_dir().join("shio_append_existing.txt");
        fs::write(&path, "line1\n").unwrap();
        let ex = executor(false, false);
        ex.vm.lock().unwrap().call_tool(
            "append_file",
            &serde_json::json!({ "path": path.to_str().unwrap(), "content": "line2\n" })
                .to_string(),
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "line1\nline2\n");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn append_file_creates_parent_directories() {
        let dir = std::env::temp_dir().join("shio_append_dir_test");
        let path = dir.join("sub").join("file.txt");
        let _ = fs::remove_dir_all(&dir);
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "append_file",
            &serde_json::json!({ "path": path.to_str().unwrap(), "content": "data" }).to_string(),
        );
        assert!(out.contains("Appended"), "{out}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "data");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_file_missing_path_returns_error() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "append_file",
            &serde_json::json!({ "content": "data" }).to_string(),
        );
        assert_eq!(out, "Error: missing 'path' argument");
    }

    #[test]
    fn append_file_missing_content_returns_error() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "append_file",
            &serde_json::json!({ "path": "/tmp/shio_whatever.txt" }).to_string(),
        );
        assert_eq!(out, "Error: missing 'content' argument");
    }

    #[test]
    fn append_file_preserves_pipe_table_content() {
        // append_file must NOT strip "  N | text" — same reason as write_file.
        let path = std::env::temp_dir().join("shio_append_preserve.txt");
        let _ = fs::remove_file(&path);
        let ex = executor(false, false);
        ex.vm.lock().unwrap().call_tool(
            "append_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "content": "  3 | value"
            })
            .to_string(),
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "  3 | value");
        let _ = fs::remove_file(&path);
    }

    // ── execute_quiet: argument-unwrap edge cases ─────────────────────────────

    #[test]
    fn execute_quiet_does_not_unwrap_non_matching_single_key() {
        // A single-key object whose key does NOT match the function name must
        // not be unwrapped — it should go to dispatch as-is (and produce an
        // error about the missing argument, not a panic or wrong behaviour).
        use crate::client::{ToolCallFunction, ToolCallItem};
        let ex = executor(false, false);
        let call = ToolCallItem {
            id: "x".into(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: "get_working_directory".into(),
                // Single key but named "other", not "get_working_directory".
                arguments: serde_json::json!({ "other": {} }).to_string(),
            },
        };
        // get_working_directory ignores args entirely, so it must still succeed.
        let out = ex.execute_quiet(&call);
        assert!(!out.starts_with("Error"), "{out}");
    }

    #[test]
    fn execute_quiet_invalid_json_returns_error() {
        use crate::client::{ToolCallFunction, ToolCallItem};
        let ex = executor(false, false);
        let call = ToolCallItem {
            id: "x".into(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: "read_file".into(),
                arguments: "not json at all".into(),
            },
        };
        let out = ex.execute_quiet(&call);
        assert!(out.starts_with("Error parsing arguments"), "{out}");
    }
}
