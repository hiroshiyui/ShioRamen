// SPDX-License-Identifier: GPL-3.0-or-later

use crate::client::ToolCallItem;

pub(super) fn fmt_call(call: &ToolCallItem) -> String {
    let args: serde_json::Value =
        serde_json::from_str(&call.function.arguments).unwrap_or_default();
    let name = &call.function.name;
    if let Some(map) = args.as_object() {
        let mut keys: Vec<&String> = map.keys().collect();
        keys.sort();
        let parts: Vec<String> = keys
            .iter()
            .filter_map(|k| {
                map[k.as_str()].as_str().map(|s| {
                    let s: String = s.chars().take(60).collect();
                    format!("{k}=\"{s}\"")
                })
            })
            .take(2)
            .collect();
        format!("{name}({})", parts.join(", "))
    } else {
        name.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ToolCallFunction;

    fn make_call(name: &str, args: &str) -> ToolCallItem {
        ToolCallItem {
            id: "test-id".to_string(),
            kind: "function".to_string(),
            function: ToolCallFunction {
                name: name.to_string(),
                arguments: args.to_string(),
            },
        }
    }

    #[test]
    fn fmt_call_shows_function_name_and_first_two_string_args() {
        let call = make_call("read_file", r#"{"path":"src/lib.rs"}"#);
        assert!(fmt_call(&call).starts_with("read_file("));
        assert!(fmt_call(&call).contains(r#"path="src/lib.rs""#));
    }

    #[test]
    fn fmt_call_truncates_long_arg_values_at_60_chars() {
        let long = "x".repeat(80);
        let args = format!(r#"{{"path":"{long}"}}"#);
        let out = fmt_call(&make_call("write_file", &args));
        let value_part = out.split('"').nth(3).unwrap_or("");
        assert!(value_part.len() <= 60, "value not truncated: {value_part}");
    }

    #[test]
    fn fmt_call_no_string_args_shows_just_name() {
        let call = make_call("set_timeout", r#"{"ms":500}"#);
        assert_eq!(fmt_call(&call), "set_timeout()");
    }

    #[test]
    fn fmt_call_empty_args_shows_just_name() {
        let call = make_call("list_tools", "{}");
        assert_eq!(fmt_call(&call), "list_tools()");
    }

    #[test]
    fn fmt_call_invalid_json_falls_back_to_name() {
        let call = make_call("broken_tool", "not json");
        assert_eq!(fmt_call(&call), "broken_tool");
    }
}
