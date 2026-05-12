// SPDX-License-Identifier: GPL-3.0-or-later
use crate::client::ToolCallItem;
use crate::tools::ToolExecutor;

pub(super) fn needs_confirm(call: &ToolCallItem, exec: &ToolExecutor) -> bool {
    match call.function.name.as_str() {
        "write_file" | "patch_file" | "delete_file" | "move_file" | "append_file"
        | "insert_after_line" => exec.confirm_writes,
        "run_shell" => exec.confirm_shell,
        _ => false,
    }
}

pub(super) fn fmt_confirm_prompt(call: &ToolCallItem) -> String {
    let args: serde_json::Value =
        serde_json::from_str(&call.function.arguments).unwrap_or_default();
    match call.function.name.as_str() {
        "write_file" => format!("Write to {}?", args["path"].as_str().unwrap_or("?")),
        "patch_file" => format!("Patch {}?", args["path"].as_str().unwrap_or("?")),
        "delete_file" => format!("Delete {}?", args["path"].as_str().unwrap_or("?")),
        "move_file" => format!(
            "Move {} → {}?",
            args["src"].as_str().unwrap_or("?"),
            args["dst"].as_str().unwrap_or("?"),
        ),
        "run_shell" => format!("Run: {}?", args["command"].as_str().unwrap_or("?")),
        name => format!("Execute {name}?"),
    }
}
