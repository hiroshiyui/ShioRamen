// SPDX-License-Identifier: GPL-3.0-or-later
use serde_json::Value;

/// Infer the intended tool name from the argument keys when the model
/// hallucinates an unknown function name.  Returns `None` if no confident
/// match can be made.  The mapping is based on *required* argument
/// combinations that uniquely identify each tool.
pub(super) fn infer_tool_name_from_args(args: &Value) -> Option<&'static str> {
    let obj = args.as_object()?;
    let has = |k: &str| obj.contains_key(k);

    // Most-specific signatures first to avoid ambiguity.
    if has("path") && has("old_str") && has("new_str") {
        return Some("patch_file");
    }
    if has("path") && has("line") && has("content") {
        return Some("insert_after_line");
    }
    if has("path") && has("start_line") {
        return Some("read_file_range");
    }
    if has("path") && has("content") {
        // Both write_file and append_file have path+content.
        // Default to write_file - the far more common operation.
        return Some("write_file");
    }
    if has("src") && has("dst") {
        return Some("move_file");
    }
    if has("paths") {
        return Some("read_many_files");
    }
    if has("command") {
        return Some("run_shell");
    }
    if has("pattern") {
        return Some("search_files");
    }
    if has("url") {
        return Some("fetch_url");
    }
    if has("query") {
        return Some("web_search");
    }
    if has("todos") {
        return Some("write_todos");
    }
    if has("memory") {
        return Some("save_memory");
    }
    if has("operation") && has("file") {
        return Some("lsp");
    }
    // path-only is ambiguous (read_file, delete_file, list_directory, ...)
    // so we don't guess.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_write_file_from_path_and_content() {
        let args = serde_json::json!({"path": "story.txt", "content": "hello"});
        assert_eq!(infer_tool_name_from_args(&args), Some("write_file"));
    }

    #[test]
    fn infer_patch_file_from_path_old_str_new_str() {
        let args = serde_json::json!({"path": "a.rs", "old_str": "foo", "new_str": "bar"});
        assert_eq!(infer_tool_name_from_args(&args), Some("patch_file"));
    }

    #[test]
    fn infer_insert_after_line_from_path_line_content() {
        let args = serde_json::json!({"path": "a.rs", "line": 10, "content": "new"});
        assert_eq!(infer_tool_name_from_args(&args), Some("insert_after_line"));
    }

    #[test]
    fn infer_move_file_from_src_dst() {
        let args = serde_json::json!({"src": "a.txt", "dst": "b.txt"});
        assert_eq!(infer_tool_name_from_args(&args), Some("move_file"));
    }

    #[test]
    fn infer_run_shell_from_command() {
        let args = serde_json::json!({"command": "ls -la"});
        assert_eq!(infer_tool_name_from_args(&args), Some("run_shell"));
    }

    #[test]
    fn infer_search_files_from_pattern() {
        let args = serde_json::json!({"pattern": "TODO"});
        assert_eq!(infer_tool_name_from_args(&args), Some("search_files"));
    }

    #[test]
    fn infer_fetch_url_from_url() {
        let args = serde_json::json!({"url": "https://example.com"});
        assert_eq!(infer_tool_name_from_args(&args), Some("fetch_url"));
    }

    #[test]
    fn infer_read_file_range_from_path_start_line() {
        let args = serde_json::json!({"path": "a.rs", "start_line": 1});
        assert_eq!(infer_tool_name_from_args(&args), Some("read_file_range"));
    }

    #[test]
    fn infer_returns_none_for_path_only() {
        // path-only is ambiguous (read_file, delete_file, ...).
        let args = serde_json::json!({"path": "a.txt"});
        assert_eq!(infer_tool_name_from_args(&args), None);
    }

    #[test]
    fn infer_returns_none_for_empty_object() {
        let args = serde_json::json!({});
        assert_eq!(infer_tool_name_from_args(&args), None);
    }

    #[test]
    fn infer_returns_none_for_non_object() {
        let args = serde_json::json!("just a string");
        assert_eq!(infer_tool_name_from_args(&args), None);
    }
}
