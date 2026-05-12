// SPDX-License-Identifier: GPL-3.0-or-later
use super::test_support::{call_tool, executor, path_str, temp_file, temp_path};
use std::fs;

#[test]
fn write_file_creates_file() {
    let (_dir, path) = temp_path("write.txt");
    let ex = executor(false, false);
    let result = call_tool(
        &ex,
        "write_file",
        serde_json::json!({ "path": path_str(&path), "content": "written" }),
    );
    assert!(
        result.contains("written") || result.contains("bytes"),
        "{result}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "written");
}

#[test]
fn write_file_preserves_pipe_table_content() {
    let (_dir, path) = temp_path("write-preserve.txt");
    let ex = executor(false, false);
    call_tool(
        &ex,
        "write_file",
        serde_json::json!({
            "path": path_str(&path),
            "content": "  3 | value\n| col | col |"
        }),
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "  3 | value\n| col | col |"
    );
}

#[test]
fn write_file_empty_content_creates_empty_file() {
    let (_dir, path) = temp_path("write-empty.txt");
    let ex = executor(false, false);
    let result = call_tool(
        &ex,
        "write_file",
        serde_json::json!({ "path": path_str(&path), "content": "" }),
    );
    assert!(result.contains("0 bytes"), "{result}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "");
}

#[test]
fn write_file_creates_parent_dirs() {
    let (_dir, path) = temp_path("a/b/out.txt");
    let ex = executor(false, false);
    let result = call_tool(
        &ex,
        "write_file",
        serde_json::json!({ "path": path_str(&path), "content": "nested" }),
    );
    assert!(
        result.contains("bytes") || result.contains("nested"),
        "{result}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "nested");
}

#[test]
fn append_file_creates_new_file() {
    let (_dir, path) = temp_path("append-new.txt");
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "append_file",
        serde_json::json!({ "path": path_str(&path), "content": "hello" }),
    );
    assert!(out.contains("Appended"), "{out}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
}

#[test]
fn append_file_preserves_existing_content() {
    let file = temp_file("line1\n");
    let path = file.path();
    let ex = executor(false, false);
    call_tool(
        &ex,
        "append_file",
        serde_json::json!({ "path": path_str(path), "content": "line2\n" }),
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "line1\nline2\n");
}

#[test]
fn append_file_creates_parent_directories() {
    let (_dir, path) = temp_path("sub/file.txt");
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "append_file",
        serde_json::json!({ "path": path_str(&path), "content": "data" }),
    );
    assert!(out.contains("Appended"), "{out}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "data");
}

#[test]
fn append_file_missing_path_returns_error() {
    let ex = executor(false, false);
    let out = call_tool(&ex, "append_file", serde_json::json!({ "content": "data" }));
    assert_eq!(out, "Error: missing 'path' argument");
}

#[test]
fn append_file_missing_content_returns_error() {
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "append_file",
        serde_json::json!({ "path": "/tmp/shio_whatever.txt" }),
    );
    assert_eq!(out, "Error: missing 'content' argument");
}

#[test]
fn append_file_preserves_pipe_table_content() {
    let (_dir, path) = temp_path("append-preserve.txt");
    let ex = executor(false, false);
    call_tool(
        &ex,
        "append_file",
        serde_json::json!({
            "path": path_str(&path),
            "content": "  3 | value"
        }),
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "  3 | value");
}
