// SPDX-License-Identifier: GPL-3.0-or-later
use super::test_support::{call_tool, executor, path_str};
use std::fs;

#[test]
fn search_files_finds_rust_sources() {
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "search_files",
        serde_json::json!({ "pattern": "src/*.rs" }),
    );
    assert!(out.contains("main.rs"), "{out}");
}

#[test]
fn grep_files_finds_pattern() {
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "grep_files",
        serde_json::json!({ "pattern": "fn main", "path": "src/main.rs" }),
    );
    assert!(out.contains("fn main"), "{out}");
}

#[test]
fn grep_files_case_insensitive_flag() {
    let path = std::env::temp_dir().join("shio_grep_ci.txt");
    fs::write(&path, "Hello World\nlower case\n").unwrap();
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "grep_files",
        serde_json::json!({
            "pattern": "hello",
            "path": path_str(&path),
            "case_insensitive": true
        }),
    );
    assert!(out.contains("Hello World"), "got: {out}");
    let _ = fs::remove_file(&path);
}

#[test]
fn grep_files_invalid_regex_returns_error() {
    let ex = executor(false, false);
    let out = call_tool(
        &ex,
        "grep_files",
        serde_json::json!({ "pattern": "[invalid(regex", "path": "src" }),
    );
    assert!(
        out.contains("Invalid regex") || out.starts_with("Error"),
        "got: {out}"
    );
}
