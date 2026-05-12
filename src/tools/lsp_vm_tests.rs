// SPDX-License-Identifier: GPL-3.0-or-later
use super::test_support::{call_tool, executor};

#[test]
fn lsp_query_unsupported_extension_returns_error_message() {
    let ex = executor(false, false);
    let result = call_tool(
        &ex,
        "lsp",
        serde_json::json!({
            "operation": "hover",
            "file": "test.xyz",
            "line": 1,
            "column": 1
        }),
    );
    assert!(
        result.contains("No LSP server found") || result.contains("Error"),
        "expected error message, got: {result}"
    );
}

#[test]
fn lsp_query_missing_file_argument() {
    let ex = executor(false, false);
    let result = call_tool(&ex, "lsp", serde_json::json!({ "operation": "hover" }));
    assert!(result.contains("missing 'file'"), "got: {result}");
}

#[test]
fn lsp_query_dispatched_via_execute_quiet() {
    let ex = executor(false, false);
    let result = call_tool(
        &ex,
        "lsp",
        serde_json::json!({"operation": "hover", "file": "test.xyz", "line": 1}),
    );
    assert!(!result.starts_with("Error: unknown tool:"), "got: {result}");
}
