// SPDX-License-Identifier: GPL-3.0-or-later
use super::test_support::{executor, path_str, temp_path};
use crate::client::{ToolCallFunction, ToolCallItem};
use std::fs;

#[test]
fn unwrap_nested_tool_args_unwraps_matching_function_name() {
    let args = serde_json::json!({
        "write_file": {
            "path": "out.txt",
            "content": "hello"
        }
    });

    let unwrapped = super::unwrap_nested_tool_args(args, "write_file");
    assert_eq!(
        unwrapped,
        serde_json::json!({
            "path": "out.txt",
            "content": "hello"
        })
    );
}

#[test]
fn unwrap_nested_tool_args_keeps_non_matching_single_key_object() {
    let args = serde_json::json!({ "other": {} });

    let unwrapped = super::unwrap_nested_tool_args(args.clone(), "write_file");
    assert_eq!(unwrapped, args);
}

#[test]
fn parse_call_args_accepts_valid_json_object() {
    let call = ToolCallItem {
        id: "x".into(),
        kind: "function".into(),
        function: ToolCallFunction {
            name: "read_file".into(),
            arguments: serde_json::json!({ "path": "src/main.rs" }).to_string(),
        },
    };

    let parsed = super::parse_call_args(&call).unwrap();
    assert_eq!(parsed, serde_json::json!({ "path": "src/main.rs" }));
}

#[test]
fn parse_call_args_reports_invalid_json() {
    let call = ToolCallItem {
        id: "x".into(),
        kind: "function".into(),
        function: ToolCallFunction {
            name: "read_file".into(),
            arguments: "not json at all".into(),
        },
    };

    let err = super::parse_call_args(&call).unwrap_err();
    assert!(err.starts_with("Error parsing arguments"), "{err}");
}

#[test]
fn execute_quiet_does_not_unwrap_non_matching_single_key() {
    let ex = executor(false, false);
    let call = ToolCallItem {
        id: "x".into(),
        kind: "function".into(),
        function: ToolCallFunction {
            name: "get_working_directory".into(),
            arguments: serde_json::json!({ "other": {} }).to_string(),
        },
    };

    let out = ex.execute_quiet(&call);
    assert!(!out.starts_with("Error"), "{out}");
}

#[test]
fn execute_quiet_invalid_json_returns_error() {
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
    let (_dir, path) = temp_path("shio_infer_write.txt");
    let ex = executor(false, false);
    let call = ToolCallItem {
        id: "call_0".into(),
        kind: "function".into(),
        function: ToolCallFunction {
            name: "cloud_subprocess_filecontent".into(),
            arguments: serde_json::json!({
                "path": path_str(&path),
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
}

#[test]
fn format_tool_warning_preserves_cli_prefix() {
    assert_eq!(
        super::format_tool_warning("tool_defs: schema export failed"),
        "[shio] tool_defs: schema export failed"
    );
}

#[test]
fn try_new_initialises_default_settings() {
    let ex = super::ToolExecutor::try_new().expect("ToolExecutor should initialise");
    assert!(!ex.confirm_writes);
    assert!(!ex.confirm_shell);
    assert_eq!(
        ex.max_tool_result_chars,
        super::DEFAULT_MAX_TOOL_RESULT_CHARS
    );
    assert!(ex.shell_allowlist.is_empty());
    assert!(ex.shell_denylist.is_empty());
    assert!(ex.lsp.is_empty());
}
