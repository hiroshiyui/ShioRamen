// SPDX-License-Identifier: GPL-3.0-or-later
use super::test_support::{call_tool, executor, path_str, temp_file};

#[test]
fn read_file_returns_content() {
    let file = temp_file("hello tool");
    let path = file.path();
    let ex = executor(false, false);
    let result = call_tool(
        &ex,
        "read_file",
        serde_json::json!({ "path": path_str(path) }),
    );
    assert!(result.starts_with("hello tool"), "{result}");
    assert!(result.contains("end of file"), "{result}");
}

#[test]
fn read_file_chunked_returns_continuation_hint() {
    let body: String = (1..=10).map(|i| format!("line {i}\n")).collect();
    let file = temp_file(&body);
    let path = file.path();
    let ex = executor(false, false);
    let result = call_tool(
        &ex,
        "read_file",
        serde_json::json!({
            "path": path_str(path),
            "cursor": 1,
            "chunk_lines": 4,
        }),
    );
    assert!(result.contains("line 1"), "{result}");
    assert!(result.contains("line 4"), "{result}");
    assert!(!result.contains("line 5"), "{result}");
    assert!(result.contains("cursor=5"), "{result}");
}

#[test]
fn read_file_cursor_past_eof() {
    let file = temp_file("only line\n");
    let path = file.path();
    let ex = executor(false, false);
    let result = call_tool(
        &ex,
        "read_file",
        serde_json::json!({ "path": path_str(path), "cursor": 99 }),
    );
    assert!(result.contains("past EOF"), "{result}");
}

#[test]
fn read_file_missing_returns_error() {
    let ex = executor(false, false);
    let result = call_tool(
        &ex,
        "read_file",
        serde_json::json!({ "path": "/nonexistent/shio_missing.txt" }),
    );
    assert!(result.starts_with("Error"), "{result}");
}

#[test]
fn read_file_range_returns_numbered_lines() {
    let file = temp_file("line1\nline2\nline3\nline4\nline5\n");
    let path = file.path();
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "read_file_range",
        serde_json::json!({
            "path": path_str(path),
            "start_line": 2,
            "end_line": 4
        }),
    );
    assert!(out.contains("line2"), "{out}");
    assert!(out.contains("line4"), "{out}");
    assert!(!out.contains("line1"), "{out}");
    assert!(!out.contains("line5"), "{out}");
    assert!(out.contains("Lines 2"), "{out}");
    assert!(out.contains("Lines 2") && out.contains('4'), "{out}");
}

#[test]
fn read_file_range_without_end_reads_to_eof() {
    let file = temp_file("a\nb\nc\n");
    let path = file.path();
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "read_file_range",
        serde_json::json!({ "path": path_str(path), "start_line": 2 }),
    );
    assert!(out.contains('b'), "{out}");
    assert!(out.contains('c'), "{out}");
    assert!(!out.contains("\na\n") && !out.ends_with("\na"), "{out}");
}

#[test]
fn read_many_files_returns_all_contents() {
    let a = temp_file("content_a");
    let b = temp_file("content_b");
    let ex = executor(false, false);
    let result = call_tool(
        &ex,
        "read_many_files",
        serde_json::json!({
            "paths": [path_str(a.path()), path_str(b.path())]
        }),
    );
    assert!(result.contains("content_a"), "{result}");
    assert!(result.contains("content_b"), "{result}");
}

#[test]
fn read_many_files_reports_missing_file_inline() {
    let ex = executor(false, false);
    let result = call_tool(
        &ex,
        "read_many_files",
        serde_json::json!({
            "paths": ["/nonexistent/shio_rmf_missing.txt"]
        }),
    );
    assert!(result.contains("Error"), "{result}");
}

#[test]
fn read_many_files_requires_paths() {
    let ex = executor(false, false);
    let result = call_tool(&ex, "read_many_files", serde_json::json!({}));
    assert!(result.starts_with("Error"), "{result}");
}
