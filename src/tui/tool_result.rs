// SPDX-License-Identifier: GPL-3.0-or-later

/// Cap a tool result at `limit` characters.
/// The truncation message instructs the model to use read_file_range
/// rather than leaving it confused about partial content.
pub(super) fn cap_tool_result(result: String, limit: usize) -> String {
    if result.len() <= limit {
        return result;
    }
    // Truncate at a char boundary.
    let cut = result
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i < limit)
        .last()
        .unwrap_or(limit);
    format!(
        "{}\n[Output truncated at {cut} chars. \
         Use read_file_range with explicit line numbers to read specific sections.]",
        &result[..cut]
    )
}

/// True iff the given tool result body is a chunked `read_file` response
/// that signals more content is available — i.e. the trailing hint produced
/// by `tools/builtin/read_file.rb` when the file has not yet been fully read.
pub(super) fn result_needs_chunk_nudge(tool_name: &str, body: &str) -> bool {
    tool_name == "read_file" && body.contains("call read_file again with cursor=")
}
