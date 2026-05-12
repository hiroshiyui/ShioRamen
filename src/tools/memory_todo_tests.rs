// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use std::fs;

fn executor(confirm_writes: bool, confirm_shell: bool) -> ToolExecutor {
    ToolExecutor {
        confirm_writes,
        confirm_shell,
        ..Default::default()
    }
}

#[test]
fn save_memory_appends_to_file() {
    let path = std::env::temp_dir().join("shio_test_memory.md");
    let _ = fs::remove_file(&path);
    let path_str = path.to_str().unwrap();
    let ex = executor(false, false);

    let result = ex.vm.lock().unwrap().call_tool(
        "save_memory",
        &serde_json::json!({ "memory": "prefer snake_case", "file": path_str }).to_string(),
    );
    assert!(result.contains("Saved"), "{result}");
    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("prefer snake_case"), "{content}");

    let result2 = ex.vm.lock().unwrap().call_tool(
        "save_memory",
        &serde_json::json!({ "memory": "prefer snake_case", "file": path_str }).to_string(),
    );
    assert!(result2.contains("skipped"), "{result2}");

    let _ = fs::remove_file(&path);
}

#[test]
fn save_memory_does_not_false_positive_on_substring() {
    let path = std::env::temp_dir().join("shio_test_memory_substr.md");
    let _ = fs::remove_file(&path);
    let path_str = path.to_str().unwrap();
    let ex = executor(false, false);

    ex.vm.lock().unwrap().call_tool(
        "save_memory",
        &serde_json::json!({
            "memory": "remember to update Cargo.toml",
            "file": path_str
        })
        .to_string(),
    );
    let result = ex.vm.lock().unwrap().call_tool(
        "save_memory",
        &serde_json::json!({
            "memory": "update Cargo.toml",
            "file": path_str
        })
        .to_string(),
    );
    assert!(
        result.contains("Saved"),
        "substring should not be treated as duplicate: {result}"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn save_memory_requires_memory_arg() {
    let ex = executor(false, false);
    let result = ex
        .vm
        .lock()
        .unwrap()
        .call_tool("save_memory", &serde_json::json!({}).to_string());
    assert!(result.starts_with("Error"), "{result}");
}

#[test]
fn write_todos_creates_file_with_checkboxes() {
    let path = std::env::temp_dir().join("shio_todos_test.md");
    let ex = executor(false, false);
    let result = ex.vm.lock().unwrap().call_tool(
        "write_todos",
        &serde_json::json!({
            "todos": [
                { "task": "first task", "status": "completed" },
                { "task": "second task", "status": "in_progress" },
                { "task": "third task" }
            ],
            "file": path.to_str().unwrap()
        })
        .to_string(),
    );
    assert!(result.contains("3"), "{result}");
    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("[x] first task"), "{content}");
    assert!(content.contains("[-] second task"), "{content}");
    assert!(content.contains("[ ] third task"), "{content}");
    let _ = fs::remove_file(&path);
}

#[test]
fn write_todos_requires_todos() {
    let ex = executor(false, false);
    let result = ex
        .vm
        .lock()
        .unwrap()
        .call_tool("write_todos", &serde_json::json!({}).to_string());
    assert!(result.starts_with("Error"), "{result}");
}
