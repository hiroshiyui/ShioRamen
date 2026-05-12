// SPDX-License-Identifier: GPL-3.0-or-later
use super::test_support::{call_tool, call_tool_raw, executor, path_str, temp_file, temp_path};
use std::fs;

#[test]
fn list_directory_shows_entries() {
    let ex = executor(false, false);
    let out = call_tool(&ex, "list_directory", serde_json::json!({ "path": "src" }));
    assert!(out.contains("main.rs"), "{out}");
}

#[test]
fn delete_file_removes_existing_file() {
    let file = temp_file("bye");
    let path = file.path().to_path_buf();
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "delete_file",
        serde_json::json!({ "path": path_str(&path) }),
    );
    assert!(out.contains("Deleted"), "{out}");
    assert!(!path.exists());
}

#[test]
fn delete_file_errors_on_missing_file() {
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "delete_file",
        serde_json::json!({ "path": "/nonexistent/shio_gone.txt" }),
    );
    assert!(out.starts_with("Error"), "{out}");
}

#[test]
fn move_file_renames_file() {
    let (_dir, src) = temp_path("move-src.txt");
    let dst = src.with_file_name("move-dst.txt");
    fs::write(&src, "content").unwrap();
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "move_file",
        serde_json::json!({
            "src": path_str(&src),
            "dst": path_str(&dst)
        }),
    );
    assert!(out.contains("Moved") || out.contains("->"), "{out}");
    assert!(!src.exists());
    assert!(dst.exists());
}

#[test]
fn move_file_nonexistent_source_returns_error() {
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "move_file",
        serde_json::json!({
            "src": "/tmp/shio_move_no_such_file.txt",
            "dst": "/tmp/shio_move_dst.txt"
        }),
    );
    assert!(
        out.contains("Error"),
        "expected error for missing source: {out}"
    );
}

#[test]
fn create_directory_creates_nested_dirs() {
    let (_root, dir) = temp_path("a/b/c");
    let ex = executor(false, false);
    let result = call_tool(
        &ex,
        "create_directory",
        serde_json::json!({ "path": path_str(&dir) }),
    );
    assert!(result.contains("Created"), "{result}");
    assert!(dir.is_dir());
}

#[test]
fn create_directory_is_idempotent() {
    let (_root, dir) = temp_path("existing");
    fs::create_dir_all(&dir).unwrap();
    let ex = executor(false, false);
    let result = call_tool(
        &ex,
        "create_directory",
        serde_json::json!({ "path": path_str(&dir) }),
    );
    assert!(result.contains("Created"), "{result}");
}

#[test]
fn create_directory_requires_path() {
    let ex = executor(false, false);
    let result = call_tool_raw(&ex, "create_directory", "{}");
    assert!(result.starts_with("Error"), "{result}");
}
