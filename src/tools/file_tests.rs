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
fn read_file_returns_content() {
    let path = std::env::temp_dir().join("shio_tool_read.txt");
    fs::write(&path, "hello tool").unwrap();
    let ex = executor(false, false);
    let result = ex.vm.lock().unwrap().call_tool(
        "read_file",
        &serde_json::json!({ "path": path.to_str().unwrap() }).to_string(),
    );
    assert!(result.starts_with("hello tool"), "{result}");
    assert!(result.contains("end of file"), "{result}");
    let _ = fs::remove_file(&path);
}

#[test]
fn read_file_chunked_returns_continuation_hint() {
    let path = std::env::temp_dir().join("shio_tool_read_chunked.txt");
    let body: String = (1..=10).map(|i| format!("line {i}\n")).collect();
    fs::write(&path, &body).unwrap();
    let ex = executor(false, false);
    let result = ex.vm.lock().unwrap().call_tool(
        "read_file",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "cursor": 1,
            "chunk_lines": 4,
        })
        .to_string(),
    );
    assert!(result.contains("line 1"), "{result}");
    assert!(result.contains("line 4"), "{result}");
    assert!(!result.contains("line 5"), "{result}");
    assert!(result.contains("cursor=5"), "{result}");
    let _ = fs::remove_file(&path);
}

#[test]
fn read_file_cursor_past_eof() {
    let path = std::env::temp_dir().join("shio_tool_read_past_eof.txt");
    fs::write(&path, "only line\n").unwrap();
    let ex = executor(false, false);
    let result = ex.vm.lock().unwrap().call_tool(
        "read_file",
        &serde_json::json!({ "path": path.to_str().unwrap(), "cursor": 99 }).to_string(),
    );
    assert!(result.contains("past EOF"), "{result}");
    let _ = fs::remove_file(&path);
}

#[test]
fn read_file_missing_returns_error() {
    let ex = executor(false, false);
    let result = ex.vm.lock().unwrap().call_tool(
        "read_file",
        &serde_json::json!({ "path": "/nonexistent/shio_missing.txt" }).to_string(),
    );
    assert!(result.starts_with("Error"), "{result}");
}

#[test]
fn read_file_range_returns_numbered_lines() {
    let path = std::env::temp_dir().join("shio_range.txt");
    fs::write(&path, "line1\nline2\nline3\nline4\nline5\n").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "read_file_range",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "start_line": 2,
            "end_line": 4
        })
        .to_string(),
    );
    assert!(out.contains("line2"), "{out}");
    assert!(out.contains("line4"), "{out}");
    assert!(!out.contains("line1"), "{out}");
    assert!(!out.contains("line5"), "{out}");
    assert!(out.contains("Lines 2"), "{out}");
    assert!(out.contains("Lines 2") && out.contains('4'), "{out}");
    let _ = fs::remove_file(&path);
}

#[test]
fn read_file_range_without_end_reads_to_eof() {
    let path = std::env::temp_dir().join("shio_range2.txt");
    fs::write(&path, "a\nb\nc\n").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "read_file_range",
        &serde_json::json!({ "path": path.to_str().unwrap(), "start_line": 2 }).to_string(),
    );
    assert!(out.contains('b'), "{out}");
    assert!(out.contains('c'), "{out}");
    assert!(!out.contains("\na\n") && !out.ends_with("\na"), "{out}");
    let _ = fs::remove_file(&path);
}

#[test]
fn read_many_files_returns_all_contents() {
    let a = std::env::temp_dir().join("shio_rmf_a.txt");
    let b = std::env::temp_dir().join("shio_rmf_b.txt");
    fs::write(&a, "content_a").unwrap();
    fs::write(&b, "content_b").unwrap();
    let ex = executor(false, false);
    let result = ex.vm.lock().unwrap().call_tool(
        "read_many_files",
        &serde_json::json!({
            "paths": [a.to_str().unwrap(), b.to_str().unwrap()]
        })
        .to_string(),
    );
    assert!(result.contains("content_a"), "{result}");
    assert!(result.contains("content_b"), "{result}");
    let _ = fs::remove_file(&a);
    let _ = fs::remove_file(&b);
}

#[test]
fn read_many_files_reports_missing_file_inline() {
    let ex = executor(false, false);
    let result = ex.vm.lock().unwrap().call_tool(
        "read_many_files",
        &serde_json::json!({
            "paths": ["/nonexistent/shio_rmf_missing.txt"]
        })
        .to_string(),
    );
    assert!(result.contains("Error"), "{result}");
}

#[test]
fn read_many_files_requires_paths() {
    let ex = executor(false, false);
    let result = ex
        .vm
        .lock()
        .unwrap()
        .call_tool("read_many_files", &serde_json::json!({}).to_string());
    assert!(result.starts_with("Error"), "{result}");
}

#[test]
fn insert_after_line_inserts_in_middle() {
    let path = std::env::temp_dir().join("shio_insert_mid.txt");
    fs::write(&path, "line1\nline2\nline3\n").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "insert_after_line",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "line": 2,
            "content": "inserted"
        })
        .to_string(),
    );
    assert!(out.contains("Inserted"), "{out}");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "line1\nline2\ninserted\nline3\n"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn insert_after_line_at_end_appends() {
    let path = std::env::temp_dir().join("shio_insert_end.txt");
    fs::write(&path, "line1\nline2\n").unwrap();
    let ex = executor(false, false);
    ex.vm.lock().unwrap().call_tool(
        "insert_after_line",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "line": 2,
            "content": "appended"
        })
        .to_string(),
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "line1\nline2\nappended\n"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn insert_after_line_after_line_zero_prepends() {
    let path = std::env::temp_dir().join("shio_insert_zero.txt");
    fs::write(&path, "line1\nline2\n").unwrap();
    let ex = executor(false, false);
    ex.vm.lock().unwrap().call_tool(
        "insert_after_line",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "line": 0,
            "content": "prepended"
        })
        .to_string(),
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "prepended\nline1\nline2\n"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn insert_after_line_negative_line_returns_error() {
    let path = std::env::temp_dir().join("shio_insert_neg.txt");
    fs::write(&path, "line1\n").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "insert_after_line",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "line": -1,
            "content": "bad"
        })
        .to_string(),
    );
    assert!(out.contains("Error"), "negative line should error: {out}");
    let _ = fs::remove_file(&path);
}

#[test]
fn insert_after_line_out_of_range_returns_error() {
    let path = std::env::temp_dir().join("shio_insert_oor.txt");
    fs::write(&path, "line1\n").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "insert_after_line",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "line": 99,
            "content": "x"
        })
        .to_string(),
    );
    assert!(out.starts_with("Error"), "{out}");
    let _ = fs::remove_file(&path);
}

#[test]
fn insert_after_line_accepts_string_line_number() {
    // Local models sometimes stringify numbers; must tolerate "42" as well as 42.
    let path = std::env::temp_dir().join("shio_insert_strnum.txt");
    fs::write(&path, "line1\nline2\n").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "insert_after_line",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "line": "1",
            "content": "inserted"
        })
        .to_string(),
    );
    assert!(!out.starts_with("Error"), "{out}");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "line1\ninserted\nline2\n"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn insert_after_line_preserves_pipe_table_content() {
    // insert_after_line must preserve loose pipe-table-looking text.
    let path = std::env::temp_dir().join("shio_insert_preserve.txt");
    fs::write(&path, "line1\nline2\n").unwrap();
    let ex = executor(false, false);
    ex.vm.lock().unwrap().call_tool(
        "insert_after_line",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "line": 1,
            "content": "  3 | value"
        })
        .to_string(),
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "line1\n  3 | value\nline2\n"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn list_directory_shows_entries() {
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "list_directory",
        &serde_json::json!({ "path": "src" }).to_string(),
    );
    assert!(out.contains("main.rs"), "{out}");
}

#[test]
fn delete_file_removes_existing_file() {
    let path = std::env::temp_dir().join("shio_delete.txt");
    fs::write(&path, "bye").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "delete_file",
        &serde_json::json!({ "path": path.to_str().unwrap() }).to_string(),
    );
    assert!(out.contains("Deleted"), "{out}");
    assert!(!path.exists());
}

#[test]
fn delete_file_errors_on_missing_file() {
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "delete_file",
        &serde_json::json!({ "path": "/nonexistent/shio_gone.txt" }).to_string(),
    );
    assert!(out.starts_with("Error"), "{out}");
}

#[test]
fn move_file_renames_file() {
    let src = std::env::temp_dir().join("shio_move_src.txt");
    let dst = std::env::temp_dir().join("shio_move_dst.txt");
    let _ = fs::remove_file(&dst);
    fs::write(&src, "content").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "move_file",
        &serde_json::json!({
            "src": src.to_str().unwrap(),
            "dst": dst.to_str().unwrap()
        })
        .to_string(),
    );
    assert!(out.contains("Moved") || out.contains("->"), "{out}");
    assert!(!src.exists());
    assert!(dst.exists());
    let _ = fs::remove_file(&dst);
}

#[test]
fn move_file_nonexistent_source_returns_error() {
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "move_file",
        &serde_json::json!({
            "src": "/tmp/shio_move_no_such_file.txt",
            "dst": "/tmp/shio_move_dst.txt"
        })
        .to_string(),
    );
    assert!(
        out.contains("Error"),
        "expected error for missing source: {out}"
    );
}

#[test]
fn create_directory_creates_nested_dirs() {
    let dir = std::env::temp_dir().join("shio_test_mkdir/a/b/c");
    let ex = executor(false, false);
    let result = ex.vm.lock().unwrap().call_tool(
        "create_directory",
        &serde_json::json!({ "path": dir.to_str().unwrap() }).to_string(),
    );
    assert!(result.contains("Created"), "{result}");
    assert!(dir.is_dir());
    let _ = fs::remove_dir_all(std::env::temp_dir().join("shio_test_mkdir"));
}

#[test]
fn create_directory_is_idempotent() {
    let dir = std::env::temp_dir().join("shio_test_mkdir_exist");
    fs::create_dir_all(&dir).unwrap();
    let ex = executor(false, false);
    let result = ex.vm.lock().unwrap().call_tool(
        "create_directory",
        &serde_json::json!({ "path": dir.to_str().unwrap() }).to_string(),
    );
    assert!(result.contains("Created"), "{result}");
    let _ = fs::remove_dir(&dir);
}

#[test]
fn create_directory_requires_path() {
    let ex = executor(false, false);
    let result = ex.vm.lock().unwrap().call_tool("create_directory", "{}");
    assert!(result.starts_with("Error"), "{result}");
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
