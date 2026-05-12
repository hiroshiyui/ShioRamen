// SPDX-License-Identifier: GPL-3.0-or-later
use super::test_support::{call_tool, executor};
use super::*;

#[test]
fn run_shell_captures_stdout() {
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "run_shell",
        serde_json::json!({ "command": "echo hello" }),
    );
    assert!(out.contains("hello"), "{out}");
}

#[test]
fn run_shell_includes_exit_code_on_failure() {
    let ex = executor(false, false);
    let out = call_tool(&ex, "run_shell", serde_json::json!({ "command": "exit 1" }));
    assert!(out.contains("exit code"), "{out}");
}

#[test]
fn run_shell_large_output_does_not_deadlock() {
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "run_shell",
        serde_json::json!({
            "command": "yes 'abcdefghijklmnopqrstuvwxyz' | head -5000"
        }),
    );
    assert!(
        !out.contains("timed out"),
        "should not timeout on large output: {out}"
    );
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

#[test]
fn run_shell_missing_command_returns_error() {
    let ex = executor(false, false);
    let out = call_tool(&ex, "run_shell", serde_json::json!({}));
    assert!(
        out.starts_with("Error"),
        "expected error for missing command, got: {out}"
    );
}
