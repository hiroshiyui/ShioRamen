// SPDX-License-Identifier: GPL-3.0-or-later
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::client::{FunctionSpec, ToolCallItem, ToolDef};
use crate::ruby::vm::ShioVm;

#[cfg(test)]
mod file_tests;
mod infer;
#[cfg(test)]
mod lsp_vm_tests;
#[cfg(test)]
mod patch_tests;
#[cfg(test)]
mod search_tests;
mod shell;
#[cfg(test)]
mod shell_vm_tests;
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
