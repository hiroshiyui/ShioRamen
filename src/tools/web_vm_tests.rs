// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

fn executor(confirm_writes: bool, confirm_shell: bool) -> ToolExecutor {
    ToolExecutor {
        confirm_writes,
        confirm_shell,
        ..Default::default()
    }
}

#[test]
fn fetch_url_rejects_non_http_schemes() {
    let ex = executor(false, false);
    let result = ex.vm.lock().unwrap().call_tool(
        "fetch_url",
        &serde_json::json!({ "url": "file:///etc/passwd" }).to_string(),
    );
    assert!(result.starts_with("Error"), "{result}");
    assert!(result.contains("http"), "{result}");
}

#[test]
fn fetch_url_requires_url_argument() {
    let ex = executor(false, false);
    let result = ex
        .vm
        .lock()
        .unwrap()
        .call_tool("fetch_url", &serde_json::json!({}).to_string());
    assert!(result.starts_with("Error"), "{result}");
}

#[test]
fn web_search_requires_query() {
    let ex = executor(false, false);
    let result = ex
        .vm
        .lock()
        .unwrap()
        .call_tool("web_search", &serde_json::json!({}).to_string());
    assert!(result.starts_with("Error"), "{result}");
}
