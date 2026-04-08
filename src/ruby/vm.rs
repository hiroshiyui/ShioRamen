// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(dead_code)]
use std::ffi::{CStr, CString};
use std::ptr;

use anyhow::{Result, anyhow};
use serde_json::Value;

use super::ffi;

const PRELUDE: &str = include_str!("prelude.rb");

pub struct ShioVm {
    mrb: *mut ffi::MrbState,
}

// SAFETY: ShioVm is only ever held behind Arc<Mutex<>> — never shared without locking.
unsafe impl Send for ShioVm {}

impl ShioVm {
    pub fn new() -> Result<Self> {
        let mrb = unsafe { ffi::mrb_open() };
        anyhow::ensure!(!mrb.is_null(), "mrb_open() failed: out of memory");
        let mut vm = Self { mrb };
        vm.eval(PRELUDE)
            .map_err(|e| anyhow!("prelude failed: {e}"))?;
        unsafe { ffi::shio_register_native(mrb) };
        // Load built-in tool scripts (embedded at compile time — added in Phase C)
        vm.load_builtin_tools()?;
        // Load user tool scripts from ~/.config/shio/tools/*.rb
        vm.load_user_tools()?;
        Ok(vm)
    }

    pub fn eval(&mut self, code: &str) -> Result<String, String> {
        let c_code = CString::new(code).map_err(|e| e.to_string())?;
        let mut error_ptr: *const std::ffi::c_char = ptr::null();
        let result_ptr = unsafe { ffi::shio_mrb_eval(self.mrb, c_code.as_ptr(), &mut error_ptr) };
        if result_ptr.is_null() {
            let msg = if error_ptr.is_null() {
                "unknown error".to_string()
            } else {
                unsafe { CStr::from_ptr(error_ptr).to_string_lossy().into_owned() }
            };
            Err(msg)
        } else {
            Ok(unsafe { CStr::from_ptr(result_ptr).to_string_lossy().into_owned() })
        }
    }

    /// Execute a registered tool by name with a JSON args string.
    /// Returns the tool result string, or an error message prefixed with
    /// `"Error: unknown tool: <name>"` when the tool is not registered in Ruby
    /// (so the caller can fall through to the Rust dispatch arm).
    pub fn call_tool(&mut self, name: &str, args_json: &str) -> String {
        let safe_name = name.replace('\\', "\\\\").replace('"', "\\\"");
        // Parse JSON on the Rust side and convert to a Ruby Hash literal with
        // string keys ("key" => value) so mRuby tool handlers can use args["key"].
        let ruby_args = match serde_json::from_str::<Value>(args_json) {
            Ok(v) => value_to_ruby(&v),
            Err(_) => "{}".to_string(),
        };
        let code = format!(
            "begin\n  t = $shio_tools[\"{safe_name}\"]\n  raise \"unknown tool: {safe_name}\" unless t\n  t.call({ruby_args})\nrescue => e\n  \"Error: #{{e.message}}\"\nend"
        );
        self.eval(&code).unwrap_or_else(|e| format!("Error: {e}"))
    }

    /// Export all registered tool schemas as `Vec<(name, description, parameters)>`.
    ///
    /// Calls the Ruby helper `shio_tool_schemas_json` which returns one JSON
    /// object per line (newline-joined).  Each object has keys `name`,
    /// `description`, and `parameters`.  Returns an empty vec when no tools
    /// are loaded yet.
    pub fn tool_schemas(&mut self) -> Result<Vec<(String, String, serde_json::Value)>> {
        let raw = self
            .eval("shio_tool_schemas_json")
            .map_err(|e| anyhow!("shio_tool_schemas_json failed: {e}"))?;
        if raw.is_empty() {
            return Ok(vec![]);
        }
        raw.lines()
            .map(|line| {
                let v: serde_json::Value = serde_json::from_str(line)
                    .map_err(|e| anyhow!("failed to parse tool schema line: {e}\nline: {line}"))?;
                let name = v["name"].as_str().unwrap_or("").to_string();
                let desc = v["description"].as_str().unwrap_or("").to_string();
                let params = v["parameters"].clone();
                Ok((name, desc, params))
            })
            .collect()
    }

    fn load_builtin_tools(&mut self) -> Result<()> {
        for (name, code) in &[
            (
                "get_working_directory",
                include_str!("../../tools/builtin/get_working_directory.rb"),
            ),
            (
                "create_directory",
                include_str!("../../tools/builtin/create_directory.rb"),
            ),
            (
                "read_file",
                include_str!("../../tools/builtin/read_file.rb"),
            ),
            (
                "list_directory",
                include_str!("../../tools/builtin/list_directory.rb"),
            ),
            (
                "delete_file",
                include_str!("../../tools/builtin/delete_file.rb"),
            ),
            (
                "move_file",
                include_str!("../../tools/builtin/move_file.rb"),
            ),
            (
                "write_file",
                include_str!("../../tools/builtin/write_file.rb"),
            ),
            (
                "append_file",
                include_str!("../../tools/builtin/append_file.rb"),
            ),
            (
                "insert_after_line",
                include_str!("../../tools/builtin/insert_after_line.rb"),
            ),
            (
                "read_file_range",
                include_str!("../../tools/builtin/read_file_range.rb"),
            ),
            (
                "read_many_files",
                include_str!("../../tools/builtin/read_many_files.rb"),
            ),
            (
                "search_files",
                include_str!("../../tools/builtin/search_files.rb"),
            ),
            (
                "grep_files",
                include_str!("../../tools/builtin/grep_files.rb"),
            ),
            (
                "save_memory",
                include_str!("../../tools/builtin/save_memory.rb"),
            ),
            (
                "write_todos",
                include_str!("../../tools/builtin/write_todos.rb"),
            ),
            (
                "fetch_url",
                include_str!("../../tools/builtin/fetch_url.rb"),
            ),
            (
                "web_search",
                include_str!("../../tools/builtin/web_search.rb"),
            ),
            (
                "patch_file",
                include_str!("../../tools/builtin/patch_file.rb"),
            ),
        ] {
            self.eval(code)
                .map_err(|e| anyhow!("builtin tool {name} failed: {e}"))?;
        }
        Ok(())
    }

    fn load_user_tools(&mut self) -> Result<()> {
        let Some(cfg_dir) = dirs::config_dir() else {
            return Ok(());
        };
        let tools_dir = cfg_dir.join("shio/tools");
        if !tools_dir.is_dir() {
            return Ok(());
        }
        let mut entries: Vec<_> = std::fs::read_dir(&tools_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rb"))
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let code = std::fs::read_to_string(entry.path())
                .map_err(|e| anyhow!("cannot read {}: {e}", entry.path().display()))?;
            self.eval(&code)
                .map_err(|e| anyhow!("error in {}: {e}", entry.path().display()))?;
        }
        Ok(())
    }
}

impl Drop for ShioVm {
    fn drop(&mut self) {
        unsafe { ffi::mrb_close(self.mrb) }
    }
}

/// Convert a `serde_json::Value` into a Ruby literal that mRuby can eval.
///
/// Objects become `{"key" => value}` Hash literals with string keys so that
/// tool handler blocks can look up arguments with `args["key"]`.
fn value_to_ruby(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let pairs: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{} => {}",
                        value_to_ruby(&Value::String(k.clone())),
                        value_to_ruby(v)
                    )
                })
                .collect();
            format!("{{{}}}", pairs.join(", "))
        }
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(value_to_ruby).collect();
            format!("[{}]", items.join(", "))
        }
        Value::String(s) => format!(
            "\"{}\"",
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
        ),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "nil".to_string(),
    }
}
