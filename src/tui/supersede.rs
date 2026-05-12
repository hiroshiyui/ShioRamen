// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;

use crate::client::{Message, MessageContent};

use super::context_budget::msg_size;

// All shio-emitted stubs start with a zero-width-space (`\u{200B}`). The model
// sees the human-readable text as if the ZWSP weren't there; we use it as an
// unambiguous machine-detectable sentinel so heuristics (`is_supersede_stub`)
// can never collide with real tool output.
pub(super) const SUPERSEDE_STUB_SENTINEL: char = '\u{200B}';

/// Read-shaped tools whose later call obsoletes an earlier call on the same
/// `key` argument. Third tuple field is the value used when the tool is called
/// without an explicit value for the key (matches the Ruby tool's default).
const SUPERSEDE_DISPATCH: &[(&str, &str, Option<&str>)] = &[
    ("read_file", "path", None),
    ("list_directory", "path", Some(".")),
    ("fetch_url", "url", None),
];

pub(super) fn supersede_spec_for(
    tool_name: &str,
) -> Option<&'static (&'static str, &'static str, Option<&'static str>)> {
    SUPERSEDE_DISPATCH.iter().find(|(n, _, _)| *n == tool_name)
}

pub(super) fn is_supersede_stub(body: &str) -> bool {
    body.starts_with(SUPERSEDE_STUB_SENTINEL)
}

/// Replace the body of any earlier `tool_name` tool_result whose argument
/// `key_name` equals `key_value` with a short stub. Keeps the message in
/// place (chat templates require every `role:"tool"` message to pair with a
/// tool_call in the preceding assistant message) but drops the bytes so
/// context doesn't grow per call. Used for read-shaped tools where a later
/// call obsoletes earlier ones on the same key (read_file -> path,
/// list_directory -> path, fetch_url -> url).
pub(super) fn supersede_prior_tool_for_key(
    msgs: &mut [Message],
    tool_name: &str,
    key_name: &str,
    key_value: &str,
    current_id: &str,
) {
    // If the caller's tool has a registered default for this key (e.g.
    // list_directory's `path` defaults to "."), reuse it when scanning prior
    // calls so implicit-arg invocations still supersede each other.
    let default_for_key: Option<&str> = SUPERSEDE_DISPATCH
        .iter()
        .find(|(n, k, _)| *n == tool_name && *k == key_name)
        .and_then(|(_, _, d)| *d);
    // Map tool_call id -> (function name, value of `key_name` in args).
    let mut owner: HashMap<String, (String, String)> = HashMap::new();
    for m in msgs.iter() {
        if m.role == "assistant"
            && let Some(calls) = &m.tool_calls
        {
            for c in calls {
                let v: serde_json::Value =
                    serde_json::from_str(&c.function.arguments).unwrap_or_default();
                let resolved = v.get(key_name).and_then(|x| x.as_str()).or_else(|| {
                    // Only apply the default for the matching tool name.
                    if c.function.name == tool_name {
                        default_for_key
                    } else {
                        None
                    }
                });
                if let Some(k) = resolved {
                    owner.insert(c.id.clone(), (c.function.name.clone(), k.to_string()));
                }
            }
        }
    }
    for m in msgs.iter_mut() {
        if m.role != "tool" {
            continue;
        }
        let Some(id) = &m.tool_call_id else { continue };
        if id == current_id {
            continue;
        }
        if let Some((name, k)) = owner.get(id)
            && name == tool_name
            && k == key_value
        {
            m.content = Some(MessageContent::Text(format!(
                "{SUPERSEDE_STUB_SENTINEL}[earlier {tool_name} result for {key_value} — superseded by a later call]"
            )));
        }
    }
}

/// Stub the *oldest* tool_result messages in the slice `msgs[turn_start..]`
/// until total estimated size is at or below `budget`, or only one
/// tool_result remains. Stubs (rather than removing) so chat-template
/// `tool_call_id` pairing stays valid. Used inside the agent loop when the
/// current-turn history alone exceeds budget; `trim_to_budget_before` only
/// touches pre-turn history.
///
/// Returns the number of messages that were stubbed.
pub(super) fn stub_oldest_tool_results_in_turn(
    msgs: &mut [Message],
    turn_start: usize,
    budget: usize,
) -> usize {
    if turn_start >= msgs.len() {
        return 0;
    }
    let total: usize = msgs.iter().map(msg_size).sum();
    if total <= budget {
        return 0;
    }
    // Indices of tool_result messages in this turn that haven't already been
    // stubbed. Stubs are identified by the SUPERSEDE_STUB_SENTINEL prefix, an
    // unambiguous marker that real tool output can't accidentally produce.
    let candidates: Vec<usize> = msgs
        .iter()
        .enumerate()
        .skip(turn_start)
        .filter(|(_, m)| m.role == "tool" && !m.text_content().is_some_and(is_supersede_stub))
        .map(|(i, _)| i)
        .collect();
    // Keep at least one current-turn tool_result un-stubbed (the most recent).
    if candidates.len() <= 1 {
        return 0;
    }
    let mut stubbed = 0usize;
    let cutoff = candidates.len() - 1; // skip the last (newest)
    let stub_body = "\u{200B}[earlier tool result dropped to free context]";
    let stub_size_estimate = msg_size(&Message::tool_result("placeholder", stub_body));
    let mut running_total = total;
    for &idx in &candidates[..cutoff] {
        if running_total <= budget {
            break;
        }
        let old_size = msg_size(&msgs[idx]);
        let m = &mut msgs[idx];
        m.content = Some(MessageContent::Text(stub_body.to_string()));
        running_total = running_total
            .saturating_sub(old_size)
            .saturating_add(stub_size_estimate);
        stubbed += 1;
    }
    stubbed
}
