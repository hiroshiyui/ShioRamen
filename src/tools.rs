// SPDX-License-Identifier: GPL-3.0-or-later
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use serde_json::Value;

use crate::client::{FunctionSpec, ToolCallItem, ToolDef};

/// Default character limit for `fetch_url` responses.
const DEFAULT_MAX_CHARS: usize = 8_000;

// ── Tool definitions (sent to the model) ─────────────────────────────────────

pub fn all_tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "read_file",
                description: "Read the full contents of a file from the filesystem.",
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
                name: "write_file",
                description: "Write content to a file, creating it or overwriting it.",
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
                name: "append_file",
                description: "Append content to the end of a file, creating it if it does not \
                    exist. The existing content is always preserved. No need to read the file \
                    first — use this instead of read_file + write_file when you only need to add \
                    content at the end.",
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
                name: "list_directory",
                description: "List files and directories inside a directory.",
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
                name: "run_shell",
                description: "Run a shell command and return its stdout and stderr.",
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
                name: "search_files",
                description: "Find files by glob pattern (e.g. \"src/**/*.rs\").",
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
                name: "grep_files",
                description: "Search for a regex pattern in files and return matching lines with line numbers.",
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
                name: "read_file_range",
                description: "Read a specific range of lines from a file. \
                    Prefer this over read_file for large files when you already \
                    know which section you need (e.g. from grep_files results).",
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
                name: "patch_file",
                description: "Apply a targeted find-and-replace edit to a file. \
                    Finds the exact string old_str (must appear exactly once) and \
                    replaces it with new_str. Safer than write_file for small edits \
                    because the rest of the file is untouched.",
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
                name: "delete_file",
                description: "Delete a file from the filesystem.",
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
                name: "move_file",
                description: "Move or rename a file or directory.",
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
                name: "fetch_url",
                description: "Fetch the text content of an HTTP or HTTPS URL. \
                    HTML pages are stripped to readable text. \
                    Use this whenever the user shares a URL or asks about a web page.",
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
                name: "create_directory",
                description: "Create a directory and any missing parent directories \
                    (equivalent to `mkdir -p`). Safe to call even if the directory \
                    already exists.",
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
                name: "get_working_directory",
                description: "Return the current working directory. \
                    Call this to resolve relative paths or orient yourself \
                    before constructing file paths.",
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            },
        },
        ToolDef {
            kind: "function",
            function: FunctionSpec {
                name: "web_search",
                description: "Search the web using DuckDuckGo and return a list of results \
                    with titles, URLs, and snippets. Use this when you need current \
                    information, documentation, or examples that may not be in your \
                    training data.",
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
                name: "save_memory",
                description: "Append a fact or note to SHIO.md for future reference. \
                    Use this to persist important information across sessions: \
                    user preferences, project conventions, architectural decisions, \
                    or anything you want to remember.",
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
                name: "read_many_files",
                description: "Read the contents of multiple files in a single call. \
                    Returns each file's content separated by a header showing its path. \
                    More efficient than calling read_file repeatedly when you need \
                    several related files at once.",
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
                name: "write_todos",
                description: "Write a task list to TODO.md, replacing the file's entire contents. \
                    Useful for tracking multi-step plans or progress on complex tasks.",
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
                name: "lsp",
                description: "Query a Language Server Protocol (LSP) server for semantic \
                    information about source code: type signatures, documentation (hover), \
                    jump-to-definition, find-all-references, and diagnostics (errors/warnings). \
                    The server is started and cached automatically; no setup required.",
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
                name: "enter_plan_mode",
                description: "Switch to plan mode, restricting tool access to read-only operations \
                    (read_file, search_files, grep_files, lsp, fetch_url, web_search, etc.). \
                    Use this before making changes: explore the codebase, understand the structure, \
                    draft a plan, then call exit_plan_mode to restore full tool access.",
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
                name: "exit_plan_mode",
                description: "Exit plan mode and restore access to all tools \
                    (write_file, patch_file, run_shell, etc.). \
                    Call this when you have finished exploring and are ready to make changes.",
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

// ── Executor ─────────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct ToolExecutor {
    pub confirm_writes: bool,
    pub confirm_shell: bool,
    /// LSP server overrides from `[lsp.servers]` in `shio.toml`.
    pub lsp: std::collections::HashMap<String, String>,
}

const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

impl ToolExecutor {
    /// Execute a tool call without producing any terminal output.
    /// Confirmation must be handled externally before calling this.
    /// Used by the TUI agent loop where stderr would corrupt the display.
    pub fn execute_quiet(&self, call: &ToolCallItem) -> String {
        let args: Value = match serde_json::from_str(&call.function.arguments) {
            Ok(v) => v,
            Err(e) => return format!("Error parsing arguments: {e}"),
        };
        self.dispatch(call.function.name.as_str(), &args)
    }

    fn dispatch(&self, name: &str, args: &Value) -> String {
        match name {
            "read_file" => self.read_file(args),
            "write_file" => self.write_file(args),
            "append_file" => self.append_file(args),
            "list_directory" => self.list_directory(args),
            "run_shell" => self.run_shell(args),
            "search_files" => self.search_files(args),
            "grep_files" => self.grep_files(args),
            "read_file_range" => self.read_file_range(args),
            "patch_file" => self.patch_file(args),
            "delete_file" => self.delete_file(args),
            "move_file" => self.move_file(args),
            "fetch_url" => self.fetch_url(args),
            "create_directory" => self.create_directory(args),
            "get_working_directory" => self.get_working_directory(),
            "web_search" => self.web_search(args),
            "save_memory" => self.save_memory(args),
            "read_many_files" => self.read_many_files(args),
            "write_todos" => self.write_todos(args),
            "lsp" => self.lsp_query(args),
            // Plan mode control is handled by the TUI agent loop, not here.
            "enter_plan_mode" | "exit_plan_mode" => {
                "Plan mode control is handled by the agent loop.".to_string()
            }
            _ => format!("Unknown tool: {name}"),
        }
    }

    // ── Individual tools ──────────────────────────────────────────────────────

    fn read_file(&self, args: &Value) -> String {
        let path = match args["path"].as_str() {
            Some(p) => p,
            None => return "Error: missing 'path' argument".into(),
        };
        match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(e) => format!("Error reading {path}: {e}"),
        }
    }

    fn write_file(&self, args: &Value) -> String {
        let path = match args["path"].as_str() {
            Some(p) => p,
            None => return "Error: missing 'path'".into(),
        };
        let content = match args["content"].as_str() {
            Some(c) => c,
            None => return "Error: missing 'content'".into(),
        };

        if self.confirm_writes && !confirm(&format!("{YELLOW}Write to {path}?{RESET}")) {
            return "Aborted by user.".into();
        }

        if let Some(parent) = Path::new(path).parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return format!("Error creating directories: {e}");
        }

        match std::fs::write(path, content) {
            Ok(()) => format!("Wrote {} bytes to {path}", content.len()),
            Err(e) => format!("Error writing {path}: {e}"),
        }
    }

    fn append_file(&self, args: &Value) -> String {
        let path = match args["path"].as_str() {
            Some(p) => p,
            None => return "Error: missing 'path'".into(),
        };
        let content = match args["content"].as_str() {
            Some(c) => c,
            None => return "Error: missing 'content'".into(),
        };

        if self.confirm_writes && !confirm(&format!("{YELLOW}Append to {path}?{RESET}")) {
            return "Aborted by user.".into();
        }

        if let Some(parent) = Path::new(path).parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return format!("Error creating directories: {e}");
        }

        use std::io::Write as _;
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Err(e) => format!("Error opening {path}: {e}"),
            Ok(mut f) => match f.write_all(content.as_bytes()) {
                Ok(()) => format!("Appended {} bytes to {path}", content.len()),
                Err(e) => format!("Error appending to {path}: {e}"),
            },
        }
    }

    fn list_directory(&self, args: &Value) -> String {
        let path = args["path"].as_str().unwrap_or(".");
        let entries = match std::fs::read_dir(path) {
            Ok(e) => e,
            Err(e) => return format!("Error listing {path}: {e}"),
        };

        let mut lines: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if e.path().is_dir() {
                    format!("{name}/")
                } else {
                    name
                }
            })
            .collect();
        lines.sort();
        if lines.is_empty() {
            "(empty)".into()
        } else {
            lines.join("\n")
        }
    }

    fn run_shell(&self, args: &Value) -> String {
        let command = match args["command"].as_str() {
            Some(c) => c,
            None => return "Error: missing 'command' argument".into(),
        };

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

    fn search_files(&self, args: &Value) -> String {
        let pattern = match args["pattern"].as_str() {
            Some(p) => p,
            None => return "Error: missing 'pattern' argument".into(),
        };
        let base = args["path"].as_str().unwrap_or(".");
        let full_pattern = if base == "." {
            pattern.to_string()
        } else {
            format!("{base}/{pattern}")
        };

        match glob::glob(&full_pattern) {
            Err(e) => format!("Invalid pattern: {e}"),
            Ok(paths) => {
                let matches: Vec<String> = paths
                    .filter_map(|p| p.ok())
                    .map(|p| p.display().to_string())
                    .collect();
                if matches.is_empty() {
                    "(no matches)".into()
                } else {
                    matches.join("\n")
                }
            }
        }
    }

    fn grep_files(&self, args: &Value) -> String {
        let pattern = match args["pattern"].as_str() {
            Some(p) => p,
            None => return "Error: missing 'pattern' argument".into(),
        };
        let path = args["path"].as_str().unwrap_or(".");
        let case_insensitive = args["case_insensitive"].as_bool().unwrap_or(false);

        let re = match regex::RegexBuilder::new(pattern)
            .case_insensitive(case_insensitive)
            .build()
        {
            Ok(r) => r,
            Err(e) => return format!("Invalid regex: {e}"),
        };

        let mut results = Vec::new();
        grep_path(Path::new(path), &re, &mut results);

        if results.is_empty() {
            "(no matches)".into()
        } else {
            results.join("\n")
        }
    }

    fn read_file_range(&self, args: &Value) -> String {
        let path = match args["path"].as_str() {
            Some(p) => p,
            None => return "Error: missing 'path' argument".into(),
        };
        let start = match args["start_line"].as_u64() {
            Some(n) if n >= 1 => n as usize,
            _ => return "Error: 'start_line' must be a positive integer".into(),
        };
        let end = args["end_line"].as_u64().map(|n| n as usize);

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => return format!("Error reading {path}: {e}"),
        };

        let total = content.lines().count();
        let end = end.unwrap_or(total).min(total);

        if start > end {
            return format!("Error: start_line ({start}) is after end_line ({end})");
        }

        let lines: Vec<String> = content
            .lines()
            .enumerate()
            .filter(|(i, _)| {
                let lineno = i + 1;
                lineno >= start && lineno <= end
            })
            .map(|(i, line)| format!("{:>5} │ {line}", i + 1))
            .collect();

        if lines.is_empty() {
            format!("(no lines in range {start}–{end}; file has {total} lines)")
        } else {
            lines.join("\n")
        }
    }

    fn patch_file(&self, args: &Value) -> String {
        let path = match args["path"].as_str() {
            Some(p) => p,
            None => return "Error: missing 'path'".into(),
        };
        let old_str = match args["old_str"].as_str() {
            Some(s) => s,
            None => return "Error: missing 'old_str'".into(),
        };
        let new_str = match args["new_str"].as_str() {
            Some(s) => s,
            None => return "Error: missing 'new_str'".into(),
        };

        if self.confirm_writes && !confirm(&format!("{YELLOW}Patch {path}?{RESET}")) {
            return "Aborted by user.".into();
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => return format!("Error reading {path}: {e}"),
        };

        let count = content.matches(old_str).count();
        match count {
            0 => return format!("Error: old_str not found in {path}"),
            1 => {}
            n => {
                return format!(
                    "Error: old_str appears {n} times in {path} — \
                     make it more specific so it matches exactly once"
                );
            }
        }

        let patched = content.replacen(old_str, new_str, 1);
        match std::fs::write(path, &patched) {
            Ok(()) => format!(
                "Patched {path}: replaced {} bytes with {} bytes",
                old_str.len(),
                new_str.len()
            ),
            Err(e) => format!("Error writing {path}: {e}"),
        }
    }

    fn delete_file(&self, args: &Value) -> String {
        let path = match args["path"].as_str() {
            Some(p) => p,
            None => return "Error: missing 'path'".into(),
        };

        if self.confirm_writes && !confirm(&format!("{YELLOW}Delete {path}?{RESET}")) {
            return "Aborted by user.".into();
        }

        match std::fs::remove_file(path) {
            Ok(()) => format!("Deleted {path}"),
            Err(e) => format!("Error deleting {path}: {e}"),
        }
    }

    fn move_file(&self, args: &Value) -> String {
        let src = match args["src"].as_str() {
            Some(s) => s,
            None => return "Error: missing 'src'".into(),
        };
        let dst = match args["dst"].as_str() {
            Some(d) => d,
            None => return "Error: missing 'dst'".into(),
        };

        let dst_exists = Path::new(dst).exists();
        let confirm_msg = if dst_exists {
            format!(
                "{YELLOW}Move {src} → {dst}? WARNING: destination already exists and will be overwritten.{RESET}"
            )
        } else {
            format!("{YELLOW}Move {src} → {dst}?{RESET}")
        };
        if self.confirm_writes && !confirm(&confirm_msg) {
            return "Aborted by user.".into();
        }

        // Create destination parent directories if needed.
        if let Some(parent) = Path::new(dst).parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return format!("Error creating destination directory: {e}");
        }

        match std::fs::rename(src, dst) {
            Ok(()) => format!("Moved {src} → {dst}"),
            Err(e) => format!("Error moving {src} → {dst}: {e}"),
        }
    }

    fn fetch_url(&self, args: &Value) -> String {
        let url = match args["url"].as_str() {
            Some(u) => u,
            None => return "Error: missing 'url' argument".into(),
        };
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

        static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
        let client = CLIENT.get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .user_agent("ShioRamen/0.1 (local AI assistant)")
                .build()
                .expect("failed to build reqwest blocking client")
        });

        let response = match client.get(url).send() {
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

    fn create_directory(&self, args: &Value) -> String {
        let path = match args["path"].as_str() {
            Some(p) => p,
            None => return "Error: missing 'path' argument".into(),
        };
        match std::fs::create_dir_all(path) {
            Ok(()) => format!("Created directory: {path}"),
            Err(e) => format!("Error creating {path}: {e}"),
        }
    }

    fn get_working_directory(&self) -> String {
        match std::env::current_dir() {
            Ok(p) => p.display().to_string(),
            Err(e) => format!("Error getting working directory: {e}"),
        }
    }

    fn web_search(&self, args: &Value) -> String {
        static RE_RESULT: OnceLock<regex::Regex> = OnceLock::new();
        static RE_SNIPPET: OnceLock<regex::Regex> = OnceLock::new();
        static RE_UDDG: OnceLock<regex::Regex> = OnceLock::new();

        let query = match args["query"].as_str() {
            Some(q) => q,
            None => return "Error: missing 'query' argument".into(),
        };
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

        static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
        let client = CLIENT.get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .user_agent("Mozilla/5.0 (compatible; ShioRamen/0.1)")
                .build()
                .expect("failed to build reqwest blocking client")
        });

        let body = match client.get(&search_url).send().and_then(|r| r.text()) {
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

    fn save_memory(&self, args: &Value) -> String {
        let memory = match args["memory"].as_str() {
            Some(m) => m,
            None => return "Error: missing 'memory' argument".into(),
        };

        let memory_file = args["file"].as_str().unwrap_or("SHIO.md");
        let existing = std::fs::read_to_string(memory_file).unwrap_or_default();

        // Skip exact duplicates.
        if existing.contains(memory) {
            return format!("Already in {memory_file} (skipped duplicate)");
        }

        let new_content = if existing.is_empty() {
            format!("# Shio Memory\n\n- {memory}\n")
        } else {
            format!("{}\n- {memory}\n", existing.trim_end())
        };

        match std::fs::write(memory_file, &new_content) {
            Ok(()) => format!("Saved to {memory_file}: {memory}"),
            Err(e) => format!("Error saving memory: {e}"),
        }
    }

    fn read_many_files(&self, args: &Value) -> String {
        let paths = match args["paths"].as_array() {
            Some(p) => p,
            None => return "Error: missing 'paths' array".into(),
        };
        if paths.is_empty() {
            return "Error: 'paths' array is empty".into();
        }

        let mut out = String::new();
        for path_val in paths {
            let path = match path_val.as_str() {
                Some(p) => p,
                None => continue,
            };
            out.push_str(&format!("=== {path} ===\n"));
            match std::fs::read_to_string(path) {
                Ok(content) => out.push_str(&content),
                Err(e) => out.push_str(&format!("[Error: {e}]")),
            }
            out.push_str("\n\n");
        }
        out.trim_end().to_string()
    }

    fn lsp_query(&self, args: &Value) -> String {
        let operation = args["operation"].as_str().unwrap_or("hover");
        let file = match args["file"].as_str() {
            Some(f) => f,
            None => return "Error: missing 'file' argument".into(),
        };
        let line = args["line"].as_u64().unwrap_or(1) as u32;
        let column = args["column"].as_u64().unwrap_or(1) as u32;
        crate::lsp::query(operation, file, line, column, &self.lsp)
    }

    fn write_todos(&self, args: &Value) -> String {
        let todos = match args["todos"].as_array() {
            Some(t) => t,
            None => return "Error: missing 'todos' array".into(),
        };

        let file = args["file"].as_str().unwrap_or("TODO.md");

        let mut content = String::from("# TODO\n\n");
        for todo in todos {
            let task = todo["task"].as_str().unwrap_or("(unnamed task)");
            let status = todo["status"].as_str().unwrap_or("pending");
            let checkbox = match status {
                "completed" => "[x]",
                "in_progress" => "[-]",
                _ => "[ ]",
            };
            content.push_str(&format!("- {checkbox} {task}\n"));
        }

        match std::fs::write(file, &content) {
            Ok(()) => format!("Wrote {} todo(s) to {file}", todos.len()),
            Err(e) => format!("Error writing {file}: {e}"),
        }
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
    let host_with_port = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);

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

    // IPv6 loopback (::1 already caught above as a string, but cover parsed form too).
    if let Ok(ipv6) = host.parse::<std::net::Ipv6Addr>() {
        return ipv6.is_loopback();
    }

    false
}

fn grep_path(path: &Path, re: &regex::Regex, out: &mut Vec<String>) {
    if path.is_dir() {
        let skip = [".git", "target", "node_modules", "vendor"];
        if let Ok(entries) = std::fs::read_dir(path) {
            let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let p = entry.path();
                let name = entry.file_name();
                if p.is_dir() && skip.contains(&name.to_string_lossy().as_ref()) {
                    continue;
                }
                grep_path(&p, re, out);
            }
        }
        return;
    }

    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for (i, line) in text.lines().enumerate() {
        if re.is_match(line) {
            out.push(format!("{}:{}: {}", path.display(), i + 1, line));
        }
    }
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
        let args = serde_json::json!({ "path": path.to_str().unwrap() });
        assert_eq!(ex.read_file(&args), "hello tool");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn read_file_missing_returns_error() {
        let ex = executor(false, false);
        let args = serde_json::json!({ "path": "/nonexistent/shio_missing.txt" });
        assert!(ex.read_file(&args).starts_with("Error"));
    }

    // ── write_file ────────────────────────────────────────────────────────────

    #[test]
    fn write_file_creates_file() {
        let path = std::env::temp_dir().join("shio_tool_write.txt");
        let _ = fs::remove_file(&path);
        let ex = executor(false, false);
        let args = serde_json::json!({ "path": path.to_str().unwrap(), "content": "written" });
        let result = ex.write_file(&args);
        assert!(
            result.contains("written") || result.contains("bytes"),
            "{result}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "written");
        let _ = fs::remove_file(&path);
    }

    // ── list_directory ────────────────────────────────────────────────────────

    #[test]
    fn list_directory_shows_entries() {
        let ex = executor(false, false);
        let args = serde_json::json!({ "path": "src" });
        let out = ex.list_directory(&args);
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
        let args = serde_json::json!({ "pattern": "src/*.rs" });
        let out = ex.search_files(&args);
        assert!(out.contains("main.rs"), "{out}");
    }

    // ── grep_files ────────────────────────────────────────────────────────────

    #[test]
    fn grep_files_finds_pattern() {
        let ex = executor(false, false);
        let args = serde_json::json!({ "pattern": "fn main", "path": "src/main.rs" });
        let out = ex.grep_files(&args);
        assert!(out.contains("fn main"), "{out}");
    }

    // ── read_file_range ───────────────────────────────────────────────────────

    #[test]
    fn read_file_range_returns_numbered_lines() {
        let path = std::env::temp_dir().join("shio_range.txt");
        fs::write(&path, "line1\nline2\nline3\nline4\nline5\n").unwrap();
        let ex = executor(false, false);
        let args = serde_json::json!({
            "path": path.to_str().unwrap(),
            "start_line": 2,
            "end_line": 4
        });
        let out = ex.read_file_range(&args);
        assert!(out.contains("line2"), "{out}");
        assert!(out.contains("line4"), "{out}");
        assert!(!out.contains("line1"), "{out}");
        assert!(!out.contains("line5"), "{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn read_file_range_without_end_reads_to_eof() {
        let path = std::env::temp_dir().join("shio_range2.txt");
        fs::write(&path, "a\nb\nc\n").unwrap();
        let ex = executor(false, false);
        let args = serde_json::json!({ "path": path.to_str().unwrap(), "start_line": 2 });
        let out = ex.read_file_range(&args);
        assert!(out.contains('b'), "{out}");
        assert!(out.contains('c'), "{out}");
        assert!(!out.contains('a'), "{out}");
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

    // ── delete_file ───────────────────────────────────────────────────────────

    #[test]
    fn delete_file_removes_existing_file() {
        let path = std::env::temp_dir().join("shio_delete.txt");
        fs::write(&path, "bye").unwrap();
        let ex = executor(false, false);
        let args = serde_json::json!({ "path": path.to_str().unwrap() });
        let out = ex.delete_file(&args);
        assert!(out.contains("Deleted"), "{out}");
        assert!(!path.exists());
    }

    #[test]
    fn delete_file_errors_on_missing_file() {
        let ex = executor(false, false);
        let args = serde_json::json!({ "path": "/nonexistent/shio_gone.txt" });
        let out = ex.delete_file(&args);
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
        let args = serde_json::json!({
            "src": src.to_str().unwrap(),
            "dst": dst.to_str().unwrap()
        });
        let out = ex.move_file(&args);
        assert!(out.contains("Moved") || out.contains("→"), "{out}");
        assert!(!src.exists());
        assert!(dst.exists());
        let _ = fs::remove_file(&dst);
    }

    // ── all_tools ─────────────────────────────────────────────────────────────

    #[test]
    fn all_tools_has_twenty_one_entries() {
        assert_eq!(all_tools().len(), 21);
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
        let result = ex.create_directory(&serde_json::json!({ "path": dir.to_str().unwrap() }));
        assert!(result.contains("Created"), "{result}");
        assert!(dir.is_dir());
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("shio_test_mkdir"));
    }

    #[test]
    fn create_directory_is_idempotent() {
        let dir = std::env::temp_dir().join("shio_test_mkdir_exist");
        std::fs::create_dir_all(&dir).unwrap();
        let ex = executor(false, false);
        let result = ex.create_directory(&serde_json::json!({ "path": dir.to_str().unwrap() }));
        assert!(result.contains("Created"), "{result}");
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn create_directory_requires_path() {
        let ex = executor(false, false);
        let result = ex.create_directory(&serde_json::json!({}));
        assert!(result.starts_with("Error"), "{result}");
    }

    // ── get_working_directory ─────────────────────────────────────────────────

    #[test]
    fn get_working_directory_returns_nonempty_path() {
        let ex = executor(false, false);
        let result = ex.get_working_directory();
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

        let result =
            ex.save_memory(&serde_json::json!({ "memory": "prefer snake_case", "file": path_str }));
        assert!(result.contains("Saved"), "{result}");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("prefer snake_case"), "{content}");

        // Duplicate is skipped.
        let result2 =
            ex.save_memory(&serde_json::json!({ "memory": "prefer snake_case", "file": path_str }));
        assert!(result2.contains("skipped"), "{result2}");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_memory_requires_memory_arg() {
        let ex = executor(false, false);
        let result = ex.save_memory(&serde_json::json!({}));
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
        let result = ex.read_many_files(&serde_json::json!({
            "paths": [a.to_str().unwrap(), b.to_str().unwrap()]
        }));
        assert!(result.contains("content_a"), "{result}");
        assert!(result.contains("content_b"), "{result}");
        let _ = fs::remove_file(&a);
        let _ = fs::remove_file(&b);
    }

    #[test]
    fn read_many_files_reports_missing_file_inline() {
        let ex = executor(false, false);
        let result = ex.read_many_files(&serde_json::json!({
            "paths": ["/nonexistent/shio_rmf_missing.txt"]
        }));
        assert!(result.contains("Error"), "{result}");
    }

    #[test]
    fn read_many_files_requires_paths() {
        let ex = executor(false, false);
        let result = ex.read_many_files(&serde_json::json!({}));
        assert!(result.starts_with("Error"), "{result}");
    }

    // ── write_todos ───────────────────────────────────────────────────────────

    #[test]
    fn write_todos_creates_file_with_checkboxes() {
        let path = std::env::temp_dir().join("shio_todos_test.md");
        let ex = executor(false, false);
        let result = ex.write_todos(&serde_json::json!({
            "todos": [
                { "task": "first task", "status": "completed" },
                { "task": "second task", "status": "in_progress" },
                { "task": "third task" }
            ],
            "file": path.to_str().unwrap()
        }));
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
        let result = ex.write_todos(&serde_json::json!({}));
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
        let args = serde_json::json!({ "path": path.to_str().unwrap(), "content": "nested" });
        let result = ex.write_file(&args);
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
        let args = serde_json::json!({
            "pattern": "hello",
            "path": path.to_str().unwrap(),
            "case_insensitive": true
        });
        let out = ex.grep_files(&args);
        assert!(out.contains("Hello World"), "got: {out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn grep_files_invalid_regex_returns_error() {
        let ex = executor(false, false);
        let args = serde_json::json!({ "pattern": "[invalid(regex", "path": "src" });
        let out = ex.grep_files(&args);
        assert!(
            out.contains("Invalid regex") || out.starts_with("Error"),
            "got: {out}"
        );
    }
}
