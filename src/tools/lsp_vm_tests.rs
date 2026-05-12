// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

fn executor(confirm_writes: bool, confirm_shell: bool) -> ToolExecutor {
    ToolExecutor {
        confirm_writes,
        confirm_shell,
        ..Default::default()
    }
}

#[test]
fn lsp_query_unsupported_extension_returns_error_message() {
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
    let ex = executor(false, false);
    let result = ex.vm.lock().unwrap().call_tool(
        "lsp",
        &serde_json::json!({"operation": "hover", "file": "test.xyz", "line": 1}).to_string(),
    );
    assert!(!result.starts_with("Error: unknown tool:"), "got: {result}");
}
