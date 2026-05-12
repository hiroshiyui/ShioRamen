// SPDX-License-Identifier: GPL-3.0-or-later

/// Extract the first token (command name) from each segment of a shell command.
///
/// Splits on `&&`, `||`, `;`, and `|` to find independent commands, then takes
/// the first whitespace-delimited word of each. Also catches `$(...)` and
/// backtick subshells by treating `$` and `` ` `` as segment separators.
///
/// This is deliberately conservative: it may flag commands that wouldn't
/// actually run, but it won't miss obvious ones.
pub(crate) fn shell_command_tokens(cmd: &str) -> Vec<String> {
    // Split on shell metacharacters that introduce new commands.
    let segments: Vec<&str> = cmd.split([';', '|', '&', '`', '$']).collect();
    segments
        .iter()
        .filter_map(|seg| {
            let trimmed = seg.trim().trim_start_matches('(').trim();
            let first = trimmed.split_whitespace().next()?;
            // Strip leading env-var assignments like `FOO=bar cmd`.
            if first.contains('=') && !first.starts_with('=') {
                trimmed
                    .split_whitespace()
                    .find(|w| !w.contains('='))
                    .map(|s| s.to_string())
            } else {
                Some(first.to_string())
            }
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Check a shell command against the allowlist/denylist.
/// Returns `Ok(())` if allowed, or `Err(message)` if denied.
pub(crate) fn check_shell_policy(
    cmd: &str,
    allowlist: &[String],
    denylist: &[String],
) -> Result<(), String> {
    if allowlist.is_empty() && denylist.is_empty() {
        return Ok(());
    }
    let tokens = shell_command_tokens(cmd);
    if !allowlist.is_empty() {
        for tok in &tokens {
            if !allowlist.iter().any(|a| a == tok) {
                return Err(format!(
                    "command '{tok}' is not in the shell allowlist — \
                     see [tools].shell_allowlist in shio.toml"
                ));
            }
        }
    }
    if !denylist.is_empty() {
        for tok in &tokens {
            if denylist.iter().any(|d| d == tok) {
                return Err(format!(
                    "command '{tok}' is on the shell denylist — \
                     see [tools].shell_denylist in shio.toml"
                ));
            }
        }
    }
    Ok(())
}
