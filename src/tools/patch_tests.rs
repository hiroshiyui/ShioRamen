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
fn patch_file_replaces_exact_match() {
    let path = std::env::temp_dir().join("shio_patch.txt");
    fs::write(&path, "hello world\n").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "hello",
            "new_str": "goodbye"
        })
        .to_string(),
    );
    assert!(out.contains("Patched"), "{out}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "goodbye world\n");
    let _ = fs::remove_file(&path);
}

#[test]
fn patch_file_errors_when_old_str_not_found() {
    let path = std::env::temp_dir().join("shio_patch2.txt");
    fs::write(&path, "hello world\n").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "nonexistent",
            "new_str": "x"
        })
        .to_string(),
    );
    assert!(out.starts_with("Error"), "{out}");
    let _ = fs::remove_file(&path);
}

#[test]
fn patch_file_errors_when_old_str_ambiguous() {
    let path = std::env::temp_dir().join("shio_patch3.txt");
    fs::write(&path, "a a a\n").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "a",
            "new_str": "b"
        })
        .to_string(),
    );
    assert!(out.starts_with("Error"), "{out}");
    let _ = fs::remove_file(&path);
}

#[test]
fn patch_file_fallback_tolerates_trailing_whitespace() {
    let path = std::env::temp_dir().join("shio_patch_fallback.txt");
    fs::write(&path, "fn foo() {\n    let x = 1;   \n}\n").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "fn foo() {\n    let x = 1;\n}",
            "new_str": "fn foo() {\n    let x = 2;\n}",
        })
        .to_string(),
    );
    assert!(out.contains("Patched"), "{out}");
    assert!(out.contains("fallback"), "{out}");
    let result = fs::read_to_string(&path).unwrap();
    assert!(result.contains("let x = 2;"), "{result}");
    let _ = fs::remove_file(&path);
}

#[test]
fn patch_file_fallback_errors_when_ambiguous() {
    let path = std::env::temp_dir().join("shio_patch_fallback_ambig.txt");
    fs::write(&path, "fn a() {}\nfn a() {}\n").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "fn a() {}",
            "new_str": "fn b() {}",
        })
        .to_string(),
    );
    assert!(out.starts_with("Error"), "{out}");
    let _ = fs::remove_file(&path);
}

#[test]
fn patch_file_fallback_rejects_whitespace_only_old_str() {
    let path = std::env::temp_dir().join("shio_patch_ws_guard.txt");
    fs::write(&path, "a\n\nb\n").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "   ",
            "new_str": "x",
        })
        .to_string(),
    );
    assert!(out.starts_with("Error"), "{out}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "a\n\nb\n");
    let _ = fs::remove_file(&path);
}

#[test]
fn patch_file_fallback_preserves_trailing_newline_in_new_str() {
    let path = std::env::temp_dir().join("shio_patch_trail_nl.txt");
    fs::write(&path, "fn foo() {   \n}\n").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
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
    let _ = fs::remove_file(&path);
}

#[test]
fn patch_file_fallback_no_spurious_blank_line_with_suffix() {
    let path = std::env::temp_dir().join("shio_patch_spurious_nl.txt");
    fs::write(&path, "line1\nAAA  \nBBB\nline4\n").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
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
    let _ = fs::remove_file(&path);
}

#[test]
fn patch_file_anchor_fallback_tolerates_wrong_interior_lines() {
    let path = std::env::temp_dir().join("shio_patch_anchor.txt");
    fs::write(
        &path,
        "fn foo() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n}\n",
    )
    .unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "fn foo() {\n    let a = 1;\n    let b = WRONG;\n    let c = 3;\n}",
            "new_str": "fn foo() {\n    42\n}",
        })
        .to_string(),
    );
    assert!(out.contains("Patched"), "{out}");
    assert!(out.contains("anchor"), "{out}");
    let result = fs::read_to_string(&path).unwrap();
    assert!(result.contains("42"), "{result}");
    let _ = fs::remove_file(&path);
}

#[test]
fn patch_file_anchor_fallback_rejects_ambiguous_anchors() {
    let path = std::env::temp_dir().join("shio_patch_anchor_ambig.txt");
    fs::write(
        &path,
        "fn foo() {\n    let a = 1;\n    x\n    let z = 9;\n}\nfn foo() {\n    let a = 1;\n    y\n    let z = 9;\n}\n",
    )
    .unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "fn foo() {\n    let a = 1;\n    WRONG\n    let z = 9;\n}",
            "new_str": "fn bar() {}",
        })
        .to_string(),
    );
    assert!(out.starts_with("Error"), "{out}");
    let _ = fs::remove_file(&path);
}

#[test]
fn patch_file_via_vm_call_tool() {
    let path = std::env::temp_dir().join("shio_patch_vm.txt");
    fs::write(&path, "hello world").unwrap();
    let ex = executor(false, false);
    let out = ex.vm.lock().unwrap().call_tool(
        "patch_file",
        &serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_str": "hello",
            "new_str": "goodbye"
        })
        .to_string(),
    );
    assert!(out.contains("Patched"), "{out}");
    let _ = fs::remove_file(&path);
}
