// SPDX-License-Identifier: GPL-3.0-or-later

use crate::client::{ContentPart, Message, MessageContent, ToolDef};

/// Estimate the percentage of the context window currently consumed by messages.
/// Returns `None` when `ctx_size` is unknown (0).
pub(super) fn context_used_pct(msgs: &[Message], tools: &[ToolDef], ctx_size: u32) -> Option<u32> {
    if ctx_size == 0 {
        return None;
    }
    let tools_overhead = serde_json::to_string(tools).map_or(0, |s| s.len());
    let total: usize = msgs.iter().map(msg_size).sum::<usize>() + tools_overhead;
    let capacity = ctx_size as usize * 4; // ~4 bytes per token estimate
    Some(((total * 100) / capacity).min(100) as u32)
}

/// For text-only messages this is the serialized JSON length.  For multimodal
/// messages that contain `image_url` parts, the base64 data URL is *not*
/// counted as tokens — llama-server decodes the image and feeds it through
/// the multimodal projector, which produces a fixed number of embedding
/// tokens (~256-576) regardless of pixel dimensions or file size.  We
/// estimate each image at 576 tokens × 4 bytes = 2 304 bytes.
pub(super) fn msg_size(m: &Message) -> usize {
    const IMAGE_TOKEN_ESTIMATE: usize = 576 * 4; // bytes

    match &m.content {
        Some(MessageContent::Parts(parts)) => {
            let mut size = 0;
            for part in parts {
                match part {
                    ContentPart::ImageUrl { .. } => size += IMAGE_TOKEN_ESTIMATE,
                    ContentPart::Text { text } => size += text.len(),
                }
            }
            // Add overhead for role, tool_calls, etc.
            size += m.role.len() + 32;
            if let Some(tc) = &m.tool_calls {
                size += serde_json::to_string(tc).map_or(0, |s| s.len());
            }
            size
        }
        _ => serde_json::to_string(m).map_or(0, |s| s.len()),
    }
}

/// Drop the oldest non-system messages from `msgs` until the total content
/// length in bytes fits within `budget`.  The system prompt (index 0) is
/// always kept.  Returns the number of messages removed.
pub(super) fn trim_to_budget(msgs: &mut Vec<Message>, budget: usize) -> usize {
    trim_to_budget_before(msgs, budget, msgs.len())
}

/// Like [`trim_to_budget`] but only removes messages before index
/// `protected_from`.  Messages at or after that index are never touched,
/// preserving the current agent turn's tool results.
/// Returns the number of messages removed.
pub(super) fn trim_to_budget_before(
    msgs: &mut Vec<Message>,
    budget: usize,
    mut protected_from: usize,
) -> usize {
    let mut dropped = 0;
    loop {
        let total: usize = msgs.iter().map(msg_size).sum();
        // Stop if within budget or no pre-turn messages left to drop (index 0 is
        // always the system prompt; earliest droppable index is 1).
        if total <= budget || protected_from <= 1 {
            break;
        }
        msgs.remove(1);
        protected_from -= 1;
        dropped += 1;
    }
    dropped
}
