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
fn search_files_finds_rust_sources() {
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "search_files",
        &serde_json::json!({ "pattern": "src/*.rs" }).to_string(),
    );
    assert!(out.contains("main.rs"), "{out}");
}

#[test]
fn grep_files_finds_pattern() {
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "grep_files",
        &serde_json::json!({ "pattern": "fn main", "path": "src/main.rs" }).to_string(),
    );
    assert!(out.contains("fn main"), "{out}");
}

#[test]
fn grep_files_case_insensitive_flag() {
    let path = std::env::temp_dir().join("shio_grep_ci.txt");
    fs::write(&path, "Hello World\nlower case\n").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "grep_files",
        &serde_json::json!({
            "pattern": "hello",
            "path": path.to_str().unwrap(),
            "case_insensitive": true
        })
        .to_string(),
    );
    assert!(out.contains("Hello World"), "got: {out}");
    let _ = fs::remove_file(&path);
}

#[test]
fn grep_files_invalid_regex_returns_error() {
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "grep_files",
        &serde_json::json!({ "pattern": "[invalid(regex", "path": "src" }).to_string(),
    );
    assert!(
        out.contains("Invalid regex") || out.starts_with("Error"),
        "got: {out}"
    );
}
