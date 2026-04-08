// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(dead_code)]
use std::ffi::{CStr, CString};
use std::ptr;

use anyhow::{Result, anyhow};

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
    /// Returns the tool result string, or an error message.
    pub fn call_tool(&mut self, name: &str, args_json: &str) -> String {
        // Escape both strings for safe embedding in Ruby source.
        let safe_name = name.replace('\\', "\\\\").replace('"', "\\\"");
        let safe_args = args_json.replace('\\', "\\\\").replace('"', "\\\"");
        let code = format!(
            "begin\n  t = $shio_tools[\"{safe_name}\"]\n  raise \"unknown tool: {safe_name}\" unless t\n  t.call({safe_args})\nrescue => e\n  \"Error: #{{e.message}}\"\nend"
        );
        self.eval(&code).unwrap_or_else(|e| format!("Error: {e}"))
    }

    /// Export all registered tool schemas as Vec<(name, description, params_json)>.
    pub fn tool_schemas(&mut self) -> Result<Vec<(String, String, String)>> {
        let raw = self
            .eval("shio_tool_schemas.inspect")
            .map_err(|e| anyhow!("shio_tool_schemas failed: {e}"))?;
        // shio_tool_schemas returns an Array of [name, description, params_json].
        // In Phase A this returns [] since no tools are loaded yet.
        parse_tool_schemas(&raw)
    }

    fn load_builtin_tools(&mut self) -> Result<()> {
        // Phase A: no built-ins loaded yet. Each batch in Phase C adds an
        // include_str! entry here.
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

fn parse_tool_schemas(_raw: &str) -> Result<Vec<(String, String, String)>> {
    // TODO: implement in Phase B when tool_schemas() is actually used.
    Ok(vec![])
}
