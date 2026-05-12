// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt::Write as _;

use super::{ToolCallFunction, ToolCallItem};

/// Extract tool calls that a local model embedded in the content field instead
/// of in the structured `tool_calls` field.
///
/// Some templates (e.g. peg-gemma4) wrap JSON payloads in `<tool_call>` /
/// `</tool_call>` (or `<|tool_call|>` / `<|/tool_call|>`) markers and replace
/// the `"` character with the special token `<|"|>`. This function normalises
/// those tokens and parses every embedded call it finds.
///
/// Expected payload shape: `{"name": "fn_name", "arguments": { ... }}`
/// Also accepts `"parameters"` in place of `"arguments"` (used by some models).
pub(super) fn extract_embedded_tool_calls(text: &str) -> Vec<ToolCallItem> {
    // Normalise the special quote token so JSON parsing works.
    let normalised = text.replace("<|\"|>", "\"").replace("<|\"|\u{3e}", "\"");

    // Collect all blocks between supported delimiters.
    let mut raw_blocks: Vec<&str> = Vec::new();
    for (open, close) in [
        ("<tool_call>", "</tool_call>"),
        ("<|tool_call|>", "<|/tool_call|>"),
    ] {
        let mut haystack = normalised.as_str();
        while let Some(start) = haystack.find(open) {
            let after_open = &haystack[start + open.len()..];
            if let Some(end) = after_open.find(close) {
                raw_blocks.push(&after_open[..end]);
                haystack = &after_open[end + close.len()..];
            } else {
                break;
            }
        }
    }

    let mut calls = Vec::new();
    for (i, block) in raw_blocks.iter().enumerate() {
        let trimmed = block.trim();

        // Strategy 1: JSON format: {"name": "func", "arguments": { ... }}
        if let Some(item) = try_parse_json_tool_call(i, trimmed) {
            calls.push(item);
            continue;
        }

        // Strategy 2: Python function-call format used by peg-gemma4.
        if let Some(item) = try_parse_funcall_tool_call(i, trimmed) {
            calls.push(item);
            continue;
        }

        eprintln!("[shio] embedded tool call block {i}: could not parse as JSON or function call");
    }
    calls
}

/// Try to parse a `<tool_call>` block as JSON:
/// `{"name": "func", "arguments"|"parameters": { ... }}`
fn try_parse_json_tool_call(index: usize, block: &str) -> Option<ToolCallItem> {
    let v = match serde_json::from_str::<serde_json::Value>(block) {
        Ok(v) => v,
        Err(_) => {
            let repaired = escape_control_chars_in_json_strings(block);
            serde_json::from_str::<serde_json::Value>(&repaired).ok()?
        }
    };
    let name = v["name"].as_str()?;
    let args = ["arguments", "parameters"]
        .iter()
        .find_map(|k| {
            let val = &v[k];
            if val.is_object() || val.is_array() {
                Some(val.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "{}".to_string());
    Some(ToolCallItem {
        id: format!("embedded_{index}"),
        kind: "function".to_string(),
        function: ToolCallFunction {
            name: name.to_string(),
            arguments: args,
        },
    })
}

/// Try to parse a `<tool_call>` block as a Python-style function call:
/// `func_name(key="val", key2="val2")`
///
/// This is the format used by Gemma 4 models via the peg-gemma4 chat format.
fn try_parse_funcall_tool_call(index: usize, block: &str) -> Option<ToolCallItem> {
    // Find function name: everything before the first '('.
    let paren = block.find('(')?;
    let name = block[..paren].trim();
    if name.is_empty() || name.contains('{') {
        return None;
    }

    // Everything between the outer parentheses.
    let rest = &block[paren + 1..];
    let inner = rest.rsplit_once(')')?.0;

    // Parse keyword arguments: key=value, key2=value2, ...
    let args = parse_kwargs(inner);

    let args_json = serde_json::to_string(&args).ok()?;
    Some(ToolCallItem {
        id: format!("embedded_{index}"),
        kind: "function".to_string(),
        function: ToolCallFunction {
            name: name.to_string(),
            arguments: args_json,
        },
    })
}

/// Parse Python-style keyword arguments: `key="value", key2="value2"`.
/// Returns a JSON object. Handles quoted strings with escapes and bare
/// newlines inside values.
pub(super) fn parse_kwargs(s: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    let mut pos = 0;
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();

    while pos < len {
        // Skip whitespace and commas.
        while pos < len && (chars[pos].is_whitespace() || chars[pos] == ',') {
            pos += 1;
        }
        if pos >= len {
            break;
        }

        // Read key (identifier before '=').
        let key_start = pos;
        while pos < len && chars[pos] != '=' && !chars[pos].is_whitespace() {
            pos += 1;
        }
        let key: String = chars[key_start..pos].iter().collect();
        if key.is_empty() {
            break;
        }

        // Skip whitespace and '='.
        while pos < len && (chars[pos].is_whitespace() || chars[pos] == '=') {
            pos += 1;
        }
        if pos >= len {
            break;
        }

        // Read value.
        let value = if chars[pos] == '"' || chars[pos] == '\'' {
            // Quoted string: scan to matching close quote, respecting escapes.
            let quote = chars[pos];
            pos += 1;
            let mut val = String::new();
            while pos < len && chars[pos] != quote {
                if chars[pos] == '\\' && pos + 1 < len {
                    let esc = chars[pos + 1];
                    match esc {
                        'n' => val.push('\n'),
                        'r' => val.push('\r'),
                        't' => val.push('\t'),
                        '\\' => val.push('\\'),
                        c if c == quote => val.push(c),
                        _ => {
                            val.push('\\');
                            val.push(esc);
                        }
                    }
                    pos += 2;
                } else {
                    val.push(chars[pos]);
                    pos += 1;
                }
            }
            if pos < len {
                pos += 1;
            }
            serde_json::Value::String(val)
        } else {
            // Unquoted value: read until comma or end.
            let val_start = pos;
            while pos < len && chars[pos] != ',' && chars[pos] != ')' {
                pos += 1;
            }
            let raw: String = chars[val_start..pos].iter().collect();
            let raw = raw.trim();
            if let Ok(n) = raw.parse::<i64>() {
                serde_json::Value::Number(n.into())
            } else if let Ok(n) = raw.parse::<f64>() {
                serde_json::json!(n)
            } else if raw == "true" {
                serde_json::Value::Bool(true)
            } else if raw == "false" {
                serde_json::Value::Bool(false)
            } else if raw == "null" || raw == "None" {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(raw.to_string())
            }
        };

        map.insert(key, value);
    }
    map
}

/// Escape bare control characters (newlines, tabs, etc.) that appear inside
/// JSON string values. Structural whitespace outside strings is left alone.
/// This fixes a common issue with local models that emit literal newlines
/// inside the `"content"` argument of tool calls.
pub(super) fn escape_control_chars_in_json_strings(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 64);
    let mut in_string = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if in_string {
            match c {
                '"' => {
                    in_string = false;
                    out.push(c);
                }
                '\\' => {
                    // Preserve existing escape sequences.
                    out.push(c);
                    if let Some(esc) = chars.next() {
                        out.push(esc);
                    }
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c.is_control() => {
                    write!(out, "\\u{:04x}", c as u32).ok();
                }
                _ => out.push(c),
            }
        } else {
            if c == '"' {
                in_string = true;
            }
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_embedded_tool_calls_parses_tool_call_markers() {
        let text = r#"<tool_call>{"name":"write_file","arguments":{"path":"a.txt","content":"hi"}}</tool_call>"#;
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "write_file");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "a.txt");
    }

    #[test]
    fn extract_embedded_tool_calls_normalises_special_quote_token() {
        let text = "<tool_call>{<|\"|>name<|\"|>:<|\"|>read_file<|\"|>,<|\"|>arguments<|\"|>:{<|\"|>path<|\"|>:<|\"|>story.md<|\"|>}}</tool_call>";
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
    }

    #[test]
    fn extract_embedded_tool_calls_accepts_parameters_key() {
        let text = r#"<tool_call>{"name":"read_file","parameters":{"path":"a.txt"}}</tool_call>"#;
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "a.txt");
    }

    #[test]
    fn extract_embedded_tool_calls_returns_empty_for_plain_text() {
        let calls = extract_embedded_tool_calls("Just a plain assistant reply.");
        assert!(calls.is_empty());
    }

    #[test]
    fn extract_embedded_tool_calls_handles_pipe_delimited_markers() {
        let text =
            r#"<|tool_call|>{"name":"list_directory","arguments":{"path":"."}}<|/tool_call|>"#;
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "list_directory");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], ".");
    }

    #[test]
    fn extract_embedded_tool_calls_extracts_multiple_calls() {
        let text = concat!(
            r#"<tool_call>{"name":"read_file","arguments":{"path":"a.txt"}}</tool_call>"#,
            r#"<tool_call>{"name":"read_file","arguments":{"path":"b.txt"}}</tool_call>"#,
        );
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 2);
        let args0: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        let args1: serde_json::Value = serde_json::from_str(&calls[1].function.arguments).unwrap();
        assert_eq!(args0["path"], "a.txt");
        assert_eq!(args1["path"], "b.txt");
    }

    #[test]
    fn extract_embedded_tool_calls_repairs_unescaped_newlines_in_content() {
        let text = "<tool_call>{\"name\":\"write_file\",\"arguments\":{\"path\":\"story.txt\",\"content\":\"line1\nline2\nline3\"}}</tool_call>";
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1, "should parse despite unescaped newlines");
        assert_eq!(calls[0].function.name, "write_file");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "story.txt");
        assert_eq!(args["content"], "line1\nline2\nline3");
    }

    #[test]
    fn escape_control_chars_in_json_strings_preserves_valid_json() {
        let input = r#"{"name":"read_file","arguments":{"path":"a.txt"}}"#;
        let output = escape_control_chars_in_json_strings(input);
        assert_eq!(input, output);
    }

    #[test]
    fn escape_control_chars_in_json_strings_fixes_bare_newlines() {
        let input = "{\"key\":\"hello\nworld\"}";
        let output = escape_control_chars_in_json_strings(input);
        assert_eq!(output, r#"{"key":"hello\nworld"}"#);
        let v: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(v["key"], "hello\nworld");
    }

    #[test]
    fn escape_control_chars_preserves_existing_escape_sequences() {
        let input = r#"{"key":"already\\escaped\n\"quote\""}"#;
        let output = escape_control_chars_in_json_strings(input);
        assert_eq!(input, output);
    }

    #[test]
    fn escape_control_chars_handles_tabs_and_carriage_returns() {
        let input = "{\"key\":\"col1\tcol2\r\n\"}";
        let output = escape_control_chars_in_json_strings(input);
        assert_eq!(output, r#"{"key":"col1\tcol2\r\n"}"#);
        let v: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(v["key"], "col1\tcol2\r\n");
    }

    #[test]
    fn escape_control_chars_leaves_structural_whitespace_alone() {
        let input = "{\n  \"a\": 1,\n  \"b\": 2\n}";
        let output = escape_control_chars_in_json_strings(input);
        assert_eq!(input, output);
        let v: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], 2);
    }

    #[test]
    fn extract_embedded_repairs_multiline_story_content() {
        let text = "<tool_call>{\"name\":\"write_file\",\"arguments\":{\"path\":\"story.txt\",\"content\":\"# Chapter 1\n\nShe opened the door.\nThe rain was heavy.\n\nThe end.\"}}</tool_call>";
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1, "repair should rescue the malformed JSON");
        assert_eq!(calls[0].function.name, "write_file");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "story.txt");
        let content = args["content"].as_str().unwrap();
        assert!(
            content.contains('\n'),
            "newlines must be preserved in content"
        );
        assert!(content.starts_with("# Chapter 1"));
        assert!(content.ends_with("The end."));
    }

    #[test]
    fn extract_embedded_repairs_pipe_delimited_with_bare_newlines() {
        let text = "<|tool_call|>{\"name\":\"write_file\",\"arguments\":{\"path\":\"a.txt\",\"content\":\"line1\nline2\"}}<|/tool_call|>";
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "write_file");
    }

    #[test]
    fn extract_embedded_skips_block_with_no_name_field() {
        let text = r#"<tool_call>{"arguments":{"path":"a.txt"}}</tool_call>"#;
        let calls = extract_embedded_tool_calls(text);
        assert!(calls.is_empty(), "block without 'name' should be skipped");
    }

    #[test]
    fn extract_embedded_skips_completely_unparseable_block() {
        let text = "<tool_call>not a function call and not json</tool_call>";
        let calls = extract_embedded_tool_calls(text);
        assert!(calls.is_empty());
    }

    #[test]
    fn extract_embedded_parses_funcall_format() {
        let text = r#"<tool_call>write_file(path="story.txt", content="hello world")</tool_call>"#;
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "write_file");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "story.txt");
        assert_eq!(args["content"], "hello world");
    }

    #[test]
    fn extract_embedded_parses_funcall_with_escaped_newlines() {
        let text =
            r#"<tool_call>write_file(path="a.txt", content="line1\nline2\nline3")</tool_call>"#;
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["content"], "line1\nline2\nline3");
    }

    #[test]
    fn extract_embedded_parses_funcall_with_hallucinated_name() {
        let text = r#"<tool_call>cloud_subprocess_filecontent(path="story.txt", content="chapter 1")</tool_call>"#;
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "cloud_subprocess_filecontent");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "story.txt");
        assert_eq!(args["content"], "chapter 1");
    }

    #[test]
    fn extract_embedded_parses_funcall_with_numeric_arg() {
        let text =
            r#"<tool_call>read_file_range(path="a.rs", start_line=10, end_line=20)</tool_call>"#;
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["start_line"], 10);
        assert_eq!(args["end_line"], 20);
    }

    #[test]
    fn extract_embedded_parses_funcall_pipe_delimiters() {
        let text = r#"<|tool_call|>read_file(path="main.rs")<|/tool_call|>"#;
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
    }

    #[test]
    fn parse_kwargs_handles_escaped_quotes_in_value() {
        let kwargs = r#"path="say \"hello\"", n=5"#;
        let map = parse_kwargs(kwargs);
        assert_eq!(map["path"].as_str().unwrap(), r#"say "hello""#);
        assert_eq!(map["n"].as_i64().unwrap(), 5);
    }

    #[test]
    fn parse_kwargs_handles_bare_newlines_in_value() {
        let kwargs = "path=\"a.txt\", content=\"line1\nline2\"";
        let map = parse_kwargs(kwargs);
        assert_eq!(map["content"].as_str().unwrap(), "line1\nline2");
    }
}
