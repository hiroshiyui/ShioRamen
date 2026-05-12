// SPDX-License-Identifier: GPL-3.0-or-later
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::client::{FunctionSpec, ToolCallItem, ToolDef};
use crate::ruby::vm::ShioVm;

#[cfg(test)]
mod executor_tests;
#[cfg(test)]
mod file_tests;
#[cfg(test)]
mod filesystem_mutation_tests;
mod infer;
#[cfg(test)]
mod insert_after_line_tests;
#[cfg(test)]
mod lsp_vm_tests;
#[cfg(test)]
mod memory_todo_tests;
#[cfg(test)]
mod patch_tests;
#[cfg(test)]
mod read_file_tests;
#[cfg(test)]
mod search_tests;
mod shell;
#[cfg(test)]
mod shell_vm_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod vm_smoke_tests;
mod web;
#[cfg(test)]
mod web_vm_tests;
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
