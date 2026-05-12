// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use std::path::Path;

pub(crate) fn executor(confirm_writes: bool, confirm_shell: bool) -> ToolExecutor {
    ToolExecutor {
        confirm_writes,
        confirm_shell,
        ..Default::default()
    }
}

pub(crate) fn call_tool(ex: &ToolExecutor, name: &str, args: serde_json::Value) -> String {
    ex.vm.lock().unwrap().call_tool(name, &args.to_string())
}

pub(crate) fn call_tool_raw(ex: &ToolExecutor, name: &str, args: &str) -> String {
    ex.vm.lock().unwrap().call_tool(name, args)
}

pub(crate) fn path_str(path: &Path) -> &str {
    path.to_str().expect("test path should be valid UTF-8")
}
