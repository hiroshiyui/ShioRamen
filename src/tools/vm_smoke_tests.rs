// SPDX-License-Identifier: GPL-3.0-or-later
use super::test_support::{call_tool, call_tool_raw, executor, path_str, temp_path};
use std::fs;

#[test]
fn ruby_string_interpolation_is_escaped() {
    let (_dir, path) = temp_path("interp.txt");
    let ex = executor(false, false);
    let content = "before #{1+1} after";

    call_tool(
        &ex,
        "write_file",
        serde_json::json!({
            "path": path_str(&path),
            "content": content
        }),
    );

    assert_eq!(fs::read_to_string(&path).unwrap(), content);
}

#[test]
fn get_working_directory_returns_nonempty_path() {
    let ex = executor(false, false);
    let result = call_tool_raw(&ex, "get_working_directory", "{}");
    assert!(!result.is_empty());
    assert!(!result.starts_with("Error"), "{result}");
}

#[test]
fn enter_plan_mode_is_registered_and_callable() {
    let ex = executor(false, false);
    let out = call_tool_raw(&ex, "enter_plan_mode", "{}");
    assert!(!out.starts_with("Error: unknown tool"), "{out}");
}

#[test]
fn exit_plan_mode_is_registered_and_callable() {
    let ex = executor(false, false);
    let out = call_tool_raw(&ex, "exit_plan_mode", "{}");
    assert!(!out.starts_with("Error: unknown tool"), "{out}");
}
