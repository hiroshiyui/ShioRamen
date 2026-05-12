// SPDX-License-Identifier: GPL-3.0-or-later
use super::test_support::{call_tool, executor, path_str, temp_file};
use std::fs;

#[test]
fn insert_after_line_inserts_in_middle() {
    let file = temp_file("line1\nline2\nline3\n");
    let path = file.path();
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "insert_after_line",
        serde_json::json!({
            "path": path_str(path),
            "line": 2,
            "content": "inserted"
        }),
    );
    assert!(out.contains("Inserted"), "{out}");
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "line1\nline2\ninserted\nline3\n"
    );
}

#[test]
fn insert_after_line_at_end_appends() {
    let file = temp_file("line1\nline2\n");
    let path = file.path();
    let ex = executor(false, false);
    call_tool(
        &ex,
        "insert_after_line",
        serde_json::json!({
            "path": path_str(path),
            "line": 2,
            "content": "appended"
        }),
    );
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "line1\nline2\nappended\n"
    );
}

#[test]
fn insert_after_line_after_line_zero_prepends() {
    let file = temp_file("line1\nline2\n");
    let path = file.path();
    let ex = executor(false, false);
    call_tool(
        &ex,
        "insert_after_line",
        serde_json::json!({
            "path": path_str(path),
            "line": 0,
            "content": "prepended"
        }),
    );
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "prepended\nline1\nline2\n"
    );
}

#[test]
fn insert_after_line_negative_line_returns_error() {
    let file = temp_file("line1\n");
    let path = file.path();
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "insert_after_line",
        serde_json::json!({
            "path": path_str(path),
            "line": -1,
            "content": "bad"
        }),
    );
    assert!(out.contains("Error"), "negative line should error: {out}");
}

#[test]
fn insert_after_line_out_of_range_returns_error() {
    let file = temp_file("line1\n");
    let path = file.path();
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "insert_after_line",
        serde_json::json!({
            "path": path_str(path),
            "line": 99,
            "content": "x"
        }),
    );
    assert!(out.starts_with("Error"), "{out}");
}

#[test]
fn insert_after_line_accepts_string_line_number() {
    let file = temp_file("line1\nline2\n");
    let path = file.path();
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "insert_after_line",
        serde_json::json!({
            "path": path_str(path),
            "line": "1",
            "content": "inserted"
        }),
    );
    assert!(!out.starts_with("Error"), "{out}");
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "line1\ninserted\nline2\n"
    );
}

#[test]
fn insert_after_line_preserves_pipe_table_content() {
    let file = temp_file("line1\nline2\n");
    let path = file.path();
    let ex = executor(false, false);
    call_tool(
        &ex,
        "insert_after_line",
        serde_json::json!({
            "path": path_str(path),
            "line": 1,
            "content": "  3 | value"
        }),
    );
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "line1\n  3 | value\nline2\n"
    );
}
