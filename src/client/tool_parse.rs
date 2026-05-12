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
