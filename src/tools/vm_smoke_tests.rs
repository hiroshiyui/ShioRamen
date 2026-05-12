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
fn ruby_string_interpolation_is_escaped() {
    let path = std::env::temp_dir().join("shio_interp_test.txt");
    let _ = fs::remove_file(&path);
    let ex = executor(false, false);
    let content = "before #{1+1} after";

    ex.vm.lock().unwrap().call_tool(
        "write_file",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": content
        })
        .to_string(),
    );

    assert_eq!(fs::read_to_string(&path).unwrap(), content);
    let _ = fs::remove_file(&path);
}

#[test]
fn get_working_directory_returns_nonempty_path() {
    let ex = executor(false, false);
    let result = ex
        .vm
        .lock()
        .unwrap()
        .call_tool("get_working_directory", "{}");
    assert!(!result.is_empty());
    assert!(!result.starts_with("Error"), "{result}");
}

#[test]
fn enter_plan_mode_is_registered_and_callable() {
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool("enter_plan_mode", "{}");
    assert!(!out.starts_with("Error: unknown tool"), "{out}");
}

#[test]
fn exit_plan_mode_is_registered_and_callable() {
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool("exit_plan_mode", "{}");
    assert!(!out.starts_with("Error: unknown tool"), "{out}");
}
