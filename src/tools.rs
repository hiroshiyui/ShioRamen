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
}

// ── Helpers ───────────────────────────────────────────────────────────────────

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

    // ── all_tools ─────────────────────────────────────────────────────────────

    #[test]
    fn all_tools_has_six_entries() {
        assert_eq!(all_tools().len(), 6);
    }
}
