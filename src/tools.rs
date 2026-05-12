// SPDX-License-Identifier: GPL-3.0-or-later
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::client::{FunctionSpec, ToolCallItem, ToolDef};
use crate::ruby::vm::ShioVm;

#[cfg(test)]
mod file_tests;
mod infer;
mod shell;
mod web;
use infer::infer_tool_name_from_args;
pub(crate) use shell::check_shell_policy;
pub(crate) use web::{
    http_client, is_private_host, percent_decode, resolves_to_private, strip_html,
};

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
    /// If non-empty, only commands whose first token matches are allowed.
    pub shell_allowlist: Vec<String>,
    /// Commands whose first token matches are rejected.
    pub shell_denylist: Vec<String>,
    /// mRuby VM for Ruby-hosted tool handlers (Phase B+).
    pub(crate) vm: Arc<Mutex<ShioVm>>,
}

impl ToolExecutor {
    /// Create a new `ToolExecutor` with default settings.
    ///
    /// Returns an error if the mRuby VM fails to initialise.
    pub fn try_new() -> anyhow::Result<Self> {
        Ok(Self {
            confirm_writes: false,
            confirm_shell: false,
            lsp: std::collections::HashMap::new(),
            max_tool_result_chars: DEFAULT_MAX_TOOL_RESULT_CHARS,
            shell_allowlist: Vec::new(),
            shell_denylist: Vec::new(),
            vm: Arc::new(Mutex::new(ShioVm::new()?)),
        })
    }
}

impl Default for ToolExecutor {
    fn default() -> Self {
        Self::try_new().expect("ShioVm init failed")
    }
}

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
        if let Value::Object(map) = &args
            && map.len() == 1
            && let Some(inner) = map.get(call.function.name.as_str())
            && inner.is_object()
        {
            args = inner.clone();
        }
        // Push config into thread-locals for native methods.
        if let Ok(json) = serde_json::to_string(&self.lsp) {
            crate::ruby::native::set_lsp_config_json(&json);
        }
        crate::ruby::native::set_shell_policy(&self.shell_allowlist, &self.shell_denylist);
        let args_json = args.to_string();
        let name = call.function.name.as_str();
        match self.vm.lock() {
            Ok(mut guard) => {
                let result = guard.call_tool(name, &args_json);
                // When a local model hallucinates a tool name (e.g.
                // "cloud_subprocess_filecontent" instead of "write_file"),
                // try to infer the intended tool from the argument keys.
                if result.starts_with("Error: unknown tool:")
                    && let Some(inferred) = infer_tool_name_from_args(&args)
                {
                    eprintln!(
                        "[shio] unknown tool \"{name}\", inferred \"{inferred}\" from arguments"
                    );
                    return guard.call_tool(inferred, &args_json);
                }
                result
            }
            Err(_) => "Error: VM mutex poisoned".to_string(),
        }
    }
}

impl ToolExecutor {
    /// Returns tool definitions sourced from the Ruby VM.
    pub fn tool_defs(&self) -> Vec<ToolDef> {
        match self.vm.lock() {
            Ok(mut guard) => match guard.tool_schemas() {
                Ok(schemas) => schemas
                    .into_iter()
                    .map(|(name, desc, params)| ToolDef {
                        kind: "function",
                        function: FunctionSpec {
                            name,
                            description: desc,
                            parameters: params,
                        },
                    })
                    .collect(),
                Err(e) => {
                    eprintln!("[shio] tool_defs: schema export failed: {e}");
                    vec![]
                }
            },
            Err(e) => {
                eprintln!("[shio] tool_defs: VM lock poisoned: {e}");
                vec![]
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

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
        assert!(result.starts_with("hello tool"), "{result}");
        assert!(result.contains("end of file"), "{result}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn read_file_chunked_returns_continuation_hint() {
        let path = std::env::temp_dir().join("shio_tool_read_chunked.txt");
        let body: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        fs::write(&path, &body).unwrap();
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "read_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "cursor": 1,
                "chunk_lines": 4,
            })
            .to_string(),
        );
        assert!(result.contains("line 1"), "{result}");
        assert!(result.contains("line 4"), "{result}");
        assert!(!result.contains("line 5"), "{result}");
        assert!(result.contains("cursor=5"), "{result}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn read_file_cursor_past_eof() {
        let path = std::env::temp_dir().join("shio_tool_read_past_eof.txt");
        fs::write(&path, "only line\n").unwrap();
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "read_file",
            &serde_json::json!({ "path": path.to_str().unwrap(), "cursor": 99 }).to_string(),
        );
        assert!(result.contains("past EOF"), "{result}");
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
    fn insert_after_line_negative_line_returns_error() {
        let path = std::env::temp_dir().join("shio_insert_neg.txt");
        fs::write(&path, "line1\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "insert_after_line",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "line": -1,
                "content": "bad"
            })
            .to_string(),
        );
        assert!(out.contains("Error"), "negative line should error: {out}");
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
        let out = ex.vm.lock().unwrap().call_tool(
            "run_shell",
            &serde_json::json!({ "command": "echo hello" }).to_string(),
        );
        assert!(out.contains("hello"), "{out}");
    }

    #[test]
    fn run_shell_includes_exit_code_on_failure() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "run_shell",
            &serde_json::json!({ "command": "exit 1" }).to_string(),
        );
        assert!(out.contains("exit code"), "{out}");
    }

    #[test]
    fn run_shell_large_output_does_not_deadlock() {
        // Regression: the try_wait() polling loop deadlocked when stdout
        // exceeded the OS pipe buffer (~64 KB) because nobody was draining
        // the pipes.  The fix uses wait_with_output() in a background thread.
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "run_shell",
            &serde_json::json!({
                "command": "yes 'abcdefghijklmnopqrstuvwxyz' | head -5000"
            })
            .to_string(),
        );
        assert!(
            !out.contains("timed out"),
            "should not timeout on large output: {out}"
        );
        // 5000 lines × 27 bytes ≈ 135 KB — well above the 64 KB pipe buffer.
        assert!(
            out.len() > 100_000,
            "expected large output, got {} bytes",
            out.len()
        );
    }

    #[test]
    fn run_shell_blocked_by_denylist() {
        let mut ex = executor(false, false);
        ex.shell_denylist = vec!["rm".to_string()];
        // The denylist is checked via the thread-local set by execute_quiet,
        // so we need to go through execute_quiet (not vm.call_tool directly).
        let call = ToolCallItem {
            id: "1".to_string(),
            kind: "function".to_string(),
            function: crate::client::ToolCallFunction {
                name: "run_shell".to_string(),
                arguments: serde_json::json!({ "command": "rm -rf /tmp/junk" }).to_string(),
            },
        };
        let out = ex.execute_quiet(&call);
        assert!(out.contains("denylist"), "{out}");
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

    // ── patch_file ────────────────────────────────────────────────────────────

    #[test]
    fn patch_file_replaces_exact_match() {
        let path = std::env::temp_dir().join("shio_patch.txt");
        fs::write(&path, "hello world\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "hello",
                "new_str": "goodbye"
            })
            .to_string(),
        );
        assert!(out.contains("Patched"), "{out}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "goodbye world\n");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_errors_when_old_str_not_found() {
        let path = std::env::temp_dir().join("shio_patch2.txt");
        fs::write(&path, "hello world\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "nonexistent",
                "new_str": "x"
            })
            .to_string(),
        );
        assert!(out.starts_with("Error"), "{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_errors_when_old_str_ambiguous() {
        let path = std::env::temp_dir().join("shio_patch3.txt");
        fs::write(&path, "a a a\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "a",
                "new_str": "b"
            })
            .to_string(),
        );
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
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "fn foo() {\n    let x = 1;\n}",
                "new_str": "fn foo() {\n    let x = 2;\n}",
            })
            .to_string(),
        );
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
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "fn a() {}",
                "new_str": "fn b() {}",
            })
            .to_string(),
        );
        assert!(out.starts_with("Error"), "{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_fallback_rejects_whitespace_only_old_str() {
        // A whitespace-only old_str would match every blank line — must be rejected.
        let path = std::env::temp_dir().join("shio_patch_ws_guard.txt");
        fs::write(&path, "a\n\nb\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "   ",
                "new_str": "x",
            })
            .to_string(),
        );
        assert!(out.starts_with("Error"), "{out}");
        // File must be unchanged.
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\n\nb\n");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_fallback_preserves_trailing_newline_in_new_str() {
        // new_str ending with '\n' must be preserved verbatim (not dropped by split).
        let path = std::env::temp_dir().join("shio_patch_trail_nl.txt");
        fs::write(&path, "fn foo() {   \n}\n").unwrap(); // trailing space triggers fallback
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "fn foo() {\n}",
                "new_str": "fn foo() {\n    42\n}\n",
            })
            .to_string(),
        );
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
    fn patch_file_fallback_no_spurious_blank_line_with_suffix() {
        // Regression: when new_str ends with \n and there are lines after
        // the match, the old code inserted a spurious blank line.
        // Use a multi-line old_str with trailing whitespace to force Level 2
        // (line-by-line fallback — exact substring match fails).
        let path = std::env::temp_dir().join("shio_patch_spurious_nl.txt");
        fs::write(&path, "line1\nAAA  \nBBB\nline4\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "AAA\nBBB",
                "new_str": "CCC\nDDD\n",
            })
            .to_string(),
        );
        assert!(out.contains("line-by-line fallback"), "{out}");
        let result = fs::read_to_string(&path).unwrap();
        assert_eq!(
            result, "line1\nCCC\nDDD\nline4\n",
            "no spurious blank line between new_str and suffix"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_anchor_fallback_tolerates_wrong_interior_lines() {
        // Simulate a model that reproduced 5 lines but got the middle one wrong.
        let path = std::env::temp_dir().join("shio_patch_anchor.txt");
        fs::write(
            &path,
            "fn foo() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n}\n",
        )
        .unwrap();
        let ex = executor(false, false);
        // Middle line differs from the file — exact and line-by-line fallbacks both fail.
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "fn foo() {\n    let a = 1;\n    let b = WRONG;\n    let c = 3;\n}",
                "new_str": "fn foo() {\n    42\n}",
            })
            .to_string(),
        );
        assert!(out.contains("Patched"), "{out}");
        assert!(out.contains("anchor"), "{out}");
        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("42"), "{result}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_anchor_fallback_rejects_ambiguous_anchors() {
        // Two blocks with the same first two and last two lines — anchor must refuse.
        let path = std::env::temp_dir().join("shio_patch_anchor_ambig.txt");
        fs::write(
            &path,
            "fn foo() {\n    let a = 1;\n    x\n    let z = 9;\n}\nfn foo() {\n    let a = 1;\n    y\n    let z = 9;\n}\n",
        )
        .unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "fn foo() {\n    let a = 1;\n    WRONG\n    let z = 9;\n}",
                "new_str": "fn bar() {}",
            })
            .to_string(),
        );
        assert!(out.starts_with("Error"), "{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_via_vm_call_tool() {
        // Verify patch_file is reachable through the Ruby VM (smoke test for registration).
        let path = std::env::temp_dir().join("shio_patch_vm.txt");
        fs::write(&path, "hello world").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "hello",
                "new_str": "goodbye"
            })
            .to_string(),
        );
        assert!(out.contains("Patched"), "{out}");
        let _ = fs::remove_file(&path);
    }

    // ── value_to_ruby string interpolation safety ──────────────────────────

    #[test]
    fn ruby_string_interpolation_is_escaped() {
        // Verify that Ruby #{} interpolation in tool args is escaped, not evaluated.
        let path = std::env::temp_dir().join("shio_interp_test.txt");
        let _ = fs::remove_file(&path);
        let ex = executor(false, false);
        // If #{} were NOT escaped, mRuby would try to evaluate `1+1` and write "2".
        let content = "before #{1+1} after";
        ex.vm.lock().unwrap().call_tool(
            "write_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "content": content
            })
            .to_string(),
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
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

    #[test]
    fn move_file_nonexistent_source_returns_error() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "move_file",
            &serde_json::json!({
                "src": "/tmp/shio_move_no_such_file.txt",
                "dst": "/tmp/shio_move_dst.txt"
            })
            .to_string(),
        );
        assert!(
            out.contains("Error"),
            "expected error for missing source: {out}"
        );
    }

    // ── lsp ───────────────────────────────────────────────────────────────────

    #[test]
    fn lsp_query_unsupported_extension_returns_error_message() {
        // .xyz is not a known language — exercises lsp::query through Ruby without
        // needing a real LSP server.
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "lsp",
            &serde_json::json!({
                "operation": "hover",
                "file": "test.xyz",
                "line": 1,
                "column": 1
            })
            .to_string(),
        );
        assert!(
            result.contains("No LSP server found") || result.contains("Error"),
            "expected error message, got: {result}"
        );
    }

    #[test]
    fn lsp_query_missing_file_argument() {
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "lsp",
            &serde_json::json!({ "operation": "hover" }).to_string(),
        );
        assert!(result.contains("missing 'file'"), "got: {result}");
    }

    #[test]
    fn lsp_query_dispatched_via_execute_quiet() {
        // Verify the lsp tool is reachable through the Ruby VM.
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "lsp",
            &serde_json::json!({"operation": "hover", "file": "test.xyz", "line": 1}).to_string(),
        );
        assert!(!result.starts_with("Error: unknown tool:"), "got: {result}");
    }

    // ── fetch_url (scheme guard, no network) ─────────────────────────────────

    #[test]
    fn fetch_url_rejects_non_http_schemes() {
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "fetch_url",
            &serde_json::json!({ "url": "file:///etc/passwd" }).to_string(),
        );
        assert!(result.starts_with("Error"), "{result}");
        assert!(result.contains("http"), "{result}");
    }

    #[test]
    fn fetch_url_requires_url_argument() {
        let ex = executor(false, false);
        let result = ex
            .vm
            .lock()
            .unwrap()
            .call_tool("fetch_url", &serde_json::json!({}).to_string());
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
        let result = ex
            .vm
            .lock()
            .unwrap()
            .call_tool("web_search", &serde_json::json!({}).to_string());
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
    fn save_memory_does_not_false_positive_on_substring() {
        // "update Cargo.toml" should NOT be flagged as a duplicate just
        // because "remember to update Cargo.toml" already exists.
        let path = std::env::temp_dir().join("shio_test_memory_substr.md");
        let _ = fs::remove_file(&path);
        let path_str = path.to_str().unwrap();
        let ex = executor(false, false);

        ex.vm.lock().unwrap().call_tool(
            "save_memory",
            &serde_json::json!({
                "memory": "remember to update Cargo.toml",
                "file": path_str
            })
            .to_string(),
        );
        let result = ex.vm.lock().unwrap().call_tool(
            "save_memory",
            &serde_json::json!({
                "memory": "update Cargo.toml",
                "file": path_str
            })
            .to_string(),
        );
        assert!(
            result.contains("Saved"),
            "substring should not be treated as duplicate: {result}"
        );
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

    // ── run_shell — missing command argument ──────────────────────────────────

    #[test]
    fn run_shell_missing_command_returns_error() {
        let ex = executor(false, false);
        let out = ex
            .vm
            .lock()
            .unwrap()
            .call_tool("run_shell", &serde_json::json!({}).to_string());
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

    // ── plan_mode stubs ────────────────────────────────────────────────────────

    #[test]
    fn enter_plan_mode_is_registered_and_callable() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool("enter_plan_mode", "{}");
        assert!(!out.starts_with("Error: unknown tool"), "{out}");
    }

    #[test]
    fn exit_plan_mode_is_registered_and_callable() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool("exit_plan_mode", "{}");
        assert!(!out.starts_with("Error: unknown tool"), "{out}");
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

    #[test]
    fn execute_quiet_infers_write_file_from_hallucinated_name() {
        // Simulates a model calling "cloud_subprocess_filecontent" instead of
        // "write_file" — the exact bug from the issue.
        use crate::client::{ToolCallFunction, ToolCallItem};
        let path = std::env::temp_dir().join("shio_infer_write.txt");
        let _ = fs::remove_file(&path);
        let ex = executor(false, false);
        let call = ToolCallItem {
            id: "call_0".into(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: "cloud_subprocess_filecontent".into(),
                arguments: serde_json::json!({
                    "path": path.to_str().unwrap(),
                    "content": "# Chapter 1\nHello world"
                })
                .to_string(),
            },
        };
        let out = ex.execute_quiet(&call);
        assert!(
            out.contains("bytes") || out.contains("Wrote"),
            "expected success but got: {out}"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# Chapter 1\nHello world"
        );
        let _ = fs::remove_file(&path);
    }
}
