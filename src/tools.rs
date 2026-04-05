// SPDX-License-Identifier: GPL-3.0-or-later
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::client::{FunctionSpec, ToolCallItem, ToolDef};

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
    ]
}

// ── Executor ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ToolExecutor {
    pub confirm_writes: bool,
    pub confirm_shell: bool,
}

const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

impl ToolExecutor {
    /// Dispatch a tool call and return the result string sent back to the model.
    /// Logs the call and result to stderr.
    pub fn execute(&self, call: &ToolCallItem) -> String {
        let args: Value = match serde_json::from_str(&call.function.arguments) {
            Ok(v) => v,
            Err(e) => return format!("Error parsing arguments: {e}"),
        };

        let name = call.function.name.as_str();
        let short_args = short_display(&args);
        eprintln!("  {CYAN}{BOLD}⚡ {name}({short_args}){RESET}");

        let result = self.dispatch(name, &args);

        let preview = result
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect::<String>();
        eprintln!("  {GREEN}→ {preview}{RESET}");

        result
    }

    /// Like `execute` but produces no terminal output.
    /// Use in TUI mode where stderr would corrupt the display.
    /// Confirmation must be handled externally before calling this.
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
            "list_directory" => self.list_directory(args),
            "run_shell" => self.run_shell(args),
            "search_files" => self.search_files(args),
            "grep_files" => self.grep_files(args),
            "read_file_range" => self.read_file_range(args),
            "patch_file" => self.patch_file(args),
            "delete_file" => self.delete_file(args),
            "move_file" => self.move_file(args),
            "fetch_url" => self.fetch_url(args),
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

        if self.confirm_writes && !confirm(&format!("{YELLOW}Move {src} → {dst}?{RESET}")) {
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
        let max_chars = args["max_chars"].as_u64().unwrap_or(8_000) as usize;

        // Only allow http/https — no file://, ftp://, etc.
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return format!("Error: only http:// and https:// URLs are supported, got: {url}");
        }

        let client = match reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("ShioRamen/0.1 (local AI assistant)")
            .build()
        {
            Ok(c) => c,
            Err(e) => return format!("Error building HTTP client: {e}"),
        };

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
        let body = match response.text() {
            Ok(t) => {
                if t.len() > 2 * 1024 * 1024 {
                    t[..2 * 1024 * 1024].to_string()
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

        // Trim and truncate.
        let text = text.trim().to_string();
        if text.len() > max_chars {
            format!(
                "{}\n\n[… truncated at {max_chars} chars — use max_chars to get more]",
                &text[..max_chars]
            )
        } else {
            text
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Strip HTML markup from a page, returning readable plain text.
///
/// Strategy:
/// 1. Remove `<script>` and `<style>` blocks with their content entirely.
/// 2. Replace block-level tags with newlines so paragraphs stay separate.
/// 3. Strip all remaining tags.
/// 4. Decode common HTML entities.
/// 5. Collapse runs of whitespace.
fn strip_html(html: &str) -> String {
    // 1. Drop script / style blocks (content included).
    let re_script = regex::Regex::new(r"(?si)<script[^>]*>.*?</script>").unwrap();
    let re_style = regex::Regex::new(r"(?si)<style[^>]*>.*?</style>").unwrap();
    let s = re_script.replace_all(html, " ");
    let s = re_style.replace_all(&s, " ");

    // 2. Block-level tags → newline so paragraphs break visually.
    let re_block = regex::Regex::new(
        r"(?i)</?(?:p|div|h[1-6]|li|tr|br|hr|blockquote|pre|article|section|header|footer|nav|main)[^>]*>",
    )
    .unwrap();
    let s = re_block.replace_all(&s, "\n");

    // 3. Strip all remaining tags.
    let re_tag = regex::Regex::new(r"<[^>]+>").unwrap();
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
    let re_spaces = regex::Regex::new(r"[ \t]+").unwrap();
    let s = re_spaces.replace_all(&s, " ");
    let re_newlines = regex::Regex::new(r"\n{3,}").unwrap();
    let s = re_newlines.replace_all(&s, "\n\n");

    s.trim().to_string()
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

/// Summarise JSON args to a short display string for the tool-call log line.
fn short_display(args: &Value) -> String {
    let Value::Object(map) = args else {
        return args.to_string();
    };
    map.iter()
        .map(|(k, v)| {
            let val = match v {
                Value::String(s) => {
                    let s = s.replace('\n', "↵");
                    if s.chars().count() > 60 {
                        format!("\"{}…\"", s.chars().take(60).collect::<String>())
                    } else {
                        format!("\"{s}\"")
                    }
                }
                other => other.to_string(),
            };
            format!("{k}={val}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn executor(confirm_writes: bool, confirm_shell: bool) -> ToolExecutor {
        ToolExecutor {
            confirm_writes,
            confirm_shell,
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

    // ── short_display ─────────────────────────────────────────────────────────

    #[test]
    fn short_display_formats_string_args() {
        let v = serde_json::json!({ "path": "src/main.rs" });
        let s = short_display(&v);
        assert!(s.contains("src/main.rs"), "{s}");
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
    fn all_tools_has_eleven_entries() {
        assert_eq!(all_tools().len(), 11);
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
}
