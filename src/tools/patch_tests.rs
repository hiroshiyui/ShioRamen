// SPDX-License-Identifier: GPL-3.0-or-later
use super::test_support::{path_str, temp_file};
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
fn patch_file_replaces_exact_match() {
    let file = temp_file("hello world\n");
    let path = file.path();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path_str(path),
            "old_str": "hello",
            "new_str": "goodbye"
        })
        .to_string(),
    );
    assert!(out.contains("Patched"), "{out}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "goodbye world\n");
}

#[test]
fn patch_file_errors_when_old_str_not_found() {
    let file = temp_file("hello world\n");
    let path = file.path();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path_str(path),
            "old_str": "nonexistent",
            "new_str": "x"
        })
        .to_string(),
    );
    assert!(out.starts_with("Error"), "{out}");
}

#[test]
fn patch_file_errors_when_old_str_ambiguous() {
    let file = temp_file("a a a\n");
    let path = file.path();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path_str(path),
            "old_str": "a",
            "new_str": "b"
        })
        .to_string(),
    );
    assert!(out.starts_with("Error"), "{out}");
}

#[test]
fn patch_file_fallback_tolerates_trailing_whitespace() {
    let file = temp_file("fn foo() {\n    let x = 1;   \n}\n");
    let path = file.path();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path_str(path),
            "old_str": "fn foo() {\n    let x = 1;\n}",
            "new_str": "fn foo() {\n    let x = 2;\n}",
        })
        .to_string(),
    );
    assert!(out.contains("Patched"), "{out}");
    assert!(out.contains("fallback"), "{out}");
    let result = fs::read_to_string(&path).unwrap();
    assert!(result.contains("let x = 2;"), "{result}");
}

#[test]
fn patch_file_fallback_errors_when_ambiguous() {
    let file = temp_file("fn a() {}\nfn a() {}\n");
    let path = file.path();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path_str(path),
            "old_str": "fn a() {}",
            "new_str": "fn b() {}",
        })
        .to_string(),
    );
    assert!(out.starts_with("Error"), "{out}");
}

#[test]
fn patch_file_fallback_rejects_whitespace_only_old_str() {
    let file = temp_file("a\n\nb\n");
    let path = file.path();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path_str(path),
            "old_str": "   ",
            "new_str": "x",
        })
        .to_string(),
    );
    assert!(out.starts_with("Error"), "{out}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "a\n\nb\n");
}

#[test]
fn patch_file_fallback_preserves_trailing_newline_in_new_str() {
    let file = temp_file("fn foo() {   \n}\n");
    let path = file.path();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path_str(path),
            "old_str": "fn foo() {\n}",
            "new_str": "fn foo() {\n    42\n}\n",
        })
        .to_string(),
    );
    assert!(out.contains("Patched"), "{out}");
    let result = fs::read_to_string(&path).unwrap();
    assert!(result.contains("42"), "{result}");
    assert!(
        result.ends_with('\n'),
        "missing trailing newline: {result:?}"
    );
    assert!(
        !result.ends_with("\n\n"),
        "double trailing newline: {result:?}"
    );
}

#[test]
fn patch_file_fallback_no_spurious_blank_line_with_suffix() {
    let file = temp_file("line1\nAAA  \nBBB\nline4\n");
    let path = file.path();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path_str(path),
            "old_str": "AAA\nBBB",
            "new_str": "CCC\nDDD\n",
        })
        .to_string(),
    );
    assert!(out.contains("line-by-line fallback"), "{out}");
    let result = fs::read_to_string(&path).unwrap();
    assert_eq!(
        result, "line1\nCCC\nDDD\nline4\n",
        "no spurious blank line between new_str and suffix"
    );
}

#[test]
fn patch_file_anchor_fallback_tolerates_wrong_interior_lines() {
    let file = temp_file("fn foo() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n}\n");
    let path = file.path();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path_str(path),
            "old_str": "fn foo() {\n    let a = 1;\n    let b = WRONG;\n    let c = 3;\n}",
            "new_str": "fn foo() {\n    42\n}",
        })
        .to_string(),
    );
    assert!(out.contains("Patched"), "{out}");
    assert!(out.contains("anchor"), "{out}");
    let result = fs::read_to_string(&path).unwrap();
    assert!(result.contains("42"), "{result}");
}

#[test]
fn patch_file_anchor_fallback_rejects_ambiguous_anchors() {
    let file = temp_file(
        "fn foo() {\n    let a = 1;\n    x\n    let z = 9;\n}\nfn foo() {\n    let a = 1;\n    y\n    let z = 9;\n}\n",
    );
    let path = file.path();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path_str(path),
            "old_str": "fn foo() {\n    let a = 1;\n    WRONG\n    let z = 9;\n}",
            "new_str": "fn bar() {}",
        })
        .to_string(),
    );
    assert!(out.starts_with("Error"), "{out}");
}

#[test]
fn patch_file_via_vm_call_tool() {
    let file = temp_file("hello world");
    let path = file.path();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path_str(path),
            "old_str": "hello",
            "new_str": "goodbye"
        })
        .to_string(),
    );
    assert!(out.contains("Patched"), "{out}");
}
