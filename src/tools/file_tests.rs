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
fn write_file_creates_file() {
    let path = std::env::temp_dir().join("shio_tool_write.txt");
    let _ = fs::remove_file(&path);
    let ex = executor(false, false);
    let result = ex.vm.lock().unwrap().call_tool(
        "write_file",
        &serde_json::json!({ "path": path.to_str().unwrap(), "content": "written" }).to_string(),
    );
    assert!(
        result.contains("written") || result.contains("bytes"),
        "{result}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "written");
    let _ = fs::remove_file(&path);
}

#[test]
fn write_file_preserves_pipe_table_content() {
    // write_file must preserve loose pipe-table-looking text.
    let path = std::env::temp_dir().join("shio_write_preserve.txt");
    let _ = fs::remove_file(&path);
    let ex = executor(false, false);
    ex.vm.lock().unwrap().call_tool(
        "write_file",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "  3 | value\n| col | col |"
        })
        .to_string(),
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "  3 | value\n| col | col |"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn write_file_empty_content_creates_empty_file() {
    let path = std::env::temp_dir().join("shio_write_empty.txt");
    let _ = fs::remove_file(&path);
    let ex = executor(false, false);
    let result = ex.vm.lock().unwrap().call_tool(
        "write_file",
        &serde_json::json!({ "path": path.to_str().unwrap(), "content": "" }).to_string(),
    );
    assert!(result.contains("0 bytes"), "{result}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "");
    let _ = fs::remove_file(&path);
}

#[test]
fn write_file_creates_parent_dirs() {
    let dir = std::env::temp_dir().join("shio_write_nested/a/b");
    let path = dir.join("out.txt");
    let ex = executor(false, false);
    let result = ex.vm.lock().unwrap().call_tool(
        "write_file",
        &serde_json::json!({ "path": path.to_str().unwrap(), "content": "nested" }).to_string(),
    );
    assert!(
        result.contains("bytes") || result.contains("nested"),
        "{result}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "nested");
    let _ = fs::remove_dir_all(std::env::temp_dir().join("shio_write_nested"));
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
