// SPDX-License-Identifier: GPL-3.0-or-later

/// Expand a skill prompt template with the given args string.
/// `{args}` is replaced by `args`; if the placeholder is absent and `args` is
/// non-empty, they are appended after the prompt. The result is trimmed.
pub(super) fn expand_skill_prompt(prompt: &str, args: &str) -> String {
    if prompt.contains("{args}") {
        prompt.replace("{args}", args).trim().to_string()
    } else if !args.is_empty() {
        format!("{} {}", prompt.trim_end(), args)
    } else {
        prompt.to_string()
    }
}
