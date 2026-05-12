// SPDX-License-Identifier: GPL-3.0-or-later
use super::test_support::{call_tool, executor, path_str, temp_path};
use std::fs;

#[test]
fn save_memory_appends_to_file() {
    let (_dir, path) = temp_path("memory.md");
    let path_str = path_str(&path);
    let ex = executor(false, false);

    let result = call_tool(
        &ex,
        "save_memory",
        serde_json::json!({ "memory": "prefer snake_case", "file": path_str }),
    );
    assert!(result.contains("Saved"), "{result}");
    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("prefer snake_case"), "{content}");

    let result2 = call_tool(
        &ex,
        "save_memory",
        serde_json::json!({ "memory": "prefer snake_case", "file": path_str }),
    );
    assert!(result2.contains("skipped"), "{result2}");
}

#[test]
fn save_memory_does_not_false_positive_on_substring() {
    let (_dir, path) = temp_path("memory-substr.md");
    let path_str = path_str(&path);
    let ex = executor(false, false);

    call_tool(
        &ex,
        "save_memory",
        serde_json::json!({
            "memory": "remember to update Cargo.toml",
            "file": path_str
        }),
    );
    let result = call_tool(
        &ex,
        "save_memory",
        serde_json::json!({
            "memory": "update Cargo.toml",
            "file": path_str
        }),
    );
    assert!(
        result.contains("Saved"),
        "substring should not be treated as duplicate: {result}"
    );
}

#[test]
fn save_memory_requires_memory_arg() {
    let ex = executor(false, false);
    let result = call_tool(&ex, "save_memory", serde_json::json!({}));
    assert!(result.starts_with("Error"), "{result}");
}

#[test]
fn write_todos_creates_file_with_checkboxes() {
    let (_dir, path) = temp_path("todos.md");
    let ex = executor(false, false);
    let result = call_tool(
        &ex,
        "write_todos",
        serde_json::json!({
            "todos": [
                { "task": "first task", "status": "completed" },
                { "task": "second task", "status": "in_progress" },
                { "task": "third task" }
            ],
            "file": path_str(&path)
        }),
    );
    assert!(result.contains("3"), "{result}");
    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("[x] first task"), "{content}");
    assert!(content.contains("[-] second task"), "{content}");
    assert!(content.contains("[ ] third task"), "{content}");
}

#[test]
fn write_todos_requires_todos() {
    let ex = executor(false, false);
    let result = call_tool(&ex, "write_todos", serde_json::json!({}));
    assert!(result.starts_with("Error"), "{result}");
}
