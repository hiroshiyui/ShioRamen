// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::client::{ToolCallFunction, ToolCallItem};
use std::fs;

fn executor(confirm_writes: bool, confirm_shell: bool) -> ToolExecutor {
    ToolExecutor {
        confirm_writes,
        confirm_shell,
        ..Default::default()
    }
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
