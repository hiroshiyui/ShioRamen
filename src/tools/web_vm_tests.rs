// SPDX-License-Identifier: GPL-3.0-or-later
use super::test_support::{call_tool, executor};

#[test]
fn fetch_url_rejects_non_http_schemes() {
    let ex = executor(false, false);
    let result = call_tool(
        &ex,
        "fetch_url",
        serde_json::json!({ "url": "file:///etc/passwd" }),
    );
    assert!(result.starts_with("Error"), "{result}");
    assert!(result.contains("http"), "{result}");
}

#[test]
fn fetch_url_requires_url_argument() {
    let ex = executor(false, false);
    let result = call_tool(&ex, "fetch_url", serde_json::json!({}));
    assert!(result.starts_with("Error"), "{result}");
}

#[test]
fn web_search_requires_query() {
    let ex = executor(false, false);
    let result = call_tool(&ex, "web_search", serde_json::json!({}));
    assert!(result.starts_with("Error"), "{result}");
}
