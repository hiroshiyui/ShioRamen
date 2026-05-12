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
fn append_file_creates_new_file() {
    let path = std::env::temp_dir().join("shio_append_new.txt");
    let _ = fs::remove_file(&path);
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "append_file",
        &serde_json::json!({ "path": path.to_str().unwrap(), "content": "hello" }).to_string(),
    );
    assert!(out.contains("Appended"), "{out}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
    let _ = fs::remove_file(&path);
}

#[test]
fn append_file_preserves_existing_content() {
    let path = std::env::temp_dir().join("shio_append_existing.txt");
    fs::write(&path, "line1\n").unwrap();
    let ex = executor(false, false);
    ex.vm.lock().unwrap().call_tool(
        "append_file",
        &serde_json::json!({ "path": path.to_str().unwrap(), "content": "line2\n" }).to_string(),
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "line1\nline2\n");
    let _ = fs::remove_file(&path);
}

#[test]
fn append_file_creates_parent_directories() {
    let dir = std::env::temp_dir().join("shio_append_dir_test");
    let path = dir.join("sub").join("file.txt");
    let _ = fs::remove_dir_all(&dir);
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "append_file",
        &serde_json::json!({ "path": path.to_str().unwrap(), "content": "data" }).to_string(),
    );
    assert!(out.contains("Appended"), "{out}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "data");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn append_file_missing_path_returns_error() {
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "append_file",
        &serde_json::json!({ "content": "data" }).to_string(),
    );
    assert_eq!(out, "Error: missing 'path' argument");
}

#[test]
fn append_file_missing_content_returns_error() {
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "append_file",
        &serde_json::json!({ "path": "/tmp/shio_whatever.txt" }).to_string(),
    );
    assert_eq!(out, "Error: missing 'content' argument");
}

#[test]
fn append_file_preserves_pipe_table_content() {
    // append_file must preserve loose pipe-table-looking text.
    let path = std::env::temp_dir().join("shio_append_preserve.txt");
    let _ = fs::remove_file(&path);
    let ex = executor(false, false);
    ex.vm.lock().unwrap().call_tool(
        "append_file",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "  3 | value"
        })
        .to_string(),
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "  3 | value");
    let _ = fs::remove_file(&path);
}
