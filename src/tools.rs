// SPDX-License-Identifier: GPL-3.0-or-later
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;

use crate::client::{FunctionSpec, ToolCallItem, ToolDef};
use crate::ruby::vm::ShioVm;

// ── Shared HTTP client ────────────────────────────────────────────────────────

/// Return the process-wide blocking HTTP client, or an error string if the
/// TLS backend failed to initialise.  The client is constructed once and
/// reused; per-request timeouts are set by each call site via
/// `client.get(url).timeout(…)`.
pub(crate) fn http_client() -> Result<&'static reqwest::blocking::Client, String> {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c);
    }
    let built = reqwest::blocking::Client::builder()
        .user_agent("ShioRamen/0.1 (local AI assistant)")
        // Disable automatic redirects to prevent SSRF bypass via 302 to private hosts.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("Error: failed to initialise HTTP client: {e}"))?;
    // get_or_init guarantees only one value is stored even under contention;
    // if another thread won the race our `built` is simply dropped.
    Ok(CLIENT.get_or_init(|| built))
}

// ── Executor ─────────────────────────────────────────────────────────────────

/// Fallback cap used when no context size is known.
/// At startup this is overridden to `ctx_size * 4 * 75 / 100` so the
/// limit automatically scales with the configured context window.
pub const DEFAULT_MAX_TOOL_RESULT_CHARS: usize = 24_000;

#[derive(Clone)]
pub struct ToolExecutor {
    pub confirm_writes: bool,
    pub confirm_shell: bool,
    /// LSP server overrides from `[lsp.servers]` in `shio.toml`.
    pub lsp: std::collections::HashMap<String, String>,
    /// Maximum characters returned from a single tool call before truncation.
    /// Computed from `ctx_size` at startup so the cap scales with the context window.
    pub max_tool_result_chars: usize,
    /// If non-empty, only commands whose first token matches are allowed.
    pub shell_allowlist: Vec<String>,
    /// Commands whose first token matches are rejected.
    pub shell_denylist: Vec<String>,
    /// mRuby VM for Ruby-hosted tool handlers (Phase B+).
    pub(crate) vm: Arc<Mutex<ShioVm>>,
}

impl Default for ToolExecutor {
    fn default() -> Self {
        Self {
            confirm_writes: false,
            confirm_shell: false,
            lsp: std::collections::HashMap::new(),
            max_tool_result_chars: DEFAULT_MAX_TOOL_RESULT_CHARS,
            shell_allowlist: Vec::new(),
            shell_denylist: Vec::new(),
            vm: Arc::new(Mutex::new(ShioVm::new().expect("ShioVm init failed"))),
        }
    }
}

impl ToolExecutor {
    /// Execute a tool call without producing any terminal output.
    /// Confirmation must be handled externally before calling this.
    /// Used by the TUI agent loop where stderr would corrupt the display.
    pub fn execute_quiet(&self, call: &ToolCallItem) -> String {
        let mut args: Value = match serde_json::from_str(&call.function.arguments) {
            Ok(v) => v,
            Err(e) => return format!("Error parsing arguments: {e}"),
        };
        // Some local models wrap all arguments under the function name, e.g.
        // {"patch_file": {"path": "…"}} instead of {"path": "…"}.  Unwrap one
        // level when that pattern is detected.
        if let Value::Object(ref map) = args.clone()
            && map.len() == 1
            && let Some(inner) = map.get(call.function.name.as_str())
            && inner.is_object()
        {
            args = inner.clone();
        }
        // Push config into thread-locals for native methods.
        if let Ok(json) = serde_json::to_string(&self.lsp) {
            crate::ruby::native::set_lsp_config_json(&json);
        }
        crate::ruby::native::set_shell_policy(&self.shell_allowlist, &self.shell_denylist);
        let args_json = args.to_string();
        match self.vm.lock() {
            Ok(mut guard) => guard.call_tool(call.function.name.as_str(), &args_json),
            Err(_) => "Error: VM mutex poisoned".to_string(),
        }
    }

    /// Returns tool definitions sourced from the Ruby VM.
    pub fn tool_defs(&self) -> Vec<ToolDef> {
        match self.vm.lock() {
            Ok(mut guard) => match guard.tool_schemas() {
                Ok(schemas) => schemas
                    .into_iter()
                    .map(|(name, desc, params)| ToolDef {
                        kind: "function",
                        function: FunctionSpec {
                            name,
                            description: desc,
                            parameters: params,
                        },
                    })
                    .collect(),
                Err(_) => vec![],
            },
            Err(_) => vec![],
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Decode a percent-encoded URL string (e.g. `%2F` → `/`, `+` → space).
/// Handles multi-byte UTF-8 sequences correctly.
pub(crate) fn percent_decode(s: &str) -> String {
    let input = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%'
            && i + 2 < input.len()
            && input[i + 1].is_ascii_hexdigit()
            && input[i + 2].is_ascii_hexdigit()
        {
            let hi = (input[i + 1] as char).to_digit(16).unwrap() as u8;
            let lo = (input[i + 2] as char).to_digit(16).unwrap() as u8;
            out.push(hi << 4 | lo);
            i += 3;
        } else if input[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Extract the first token (command name) from each segment of a shell command.
///
/// Splits on `&&`, `||`, `;`, and `|` to find independent commands, then takes
/// the first whitespace-delimited word of each.  Also catches `$(…)` and
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

/// Strip HTML markup from a page, returning readable plain text.
///
/// Strategy:
/// 1. Remove `<script>` and `<style>` blocks with their content entirely.
/// 2. Replace block-level tags with newlines so paragraphs stay separate.
/// 3. Strip all remaining tags.
/// 4. Decode common HTML entities.
/// 5. Collapse runs of whitespace.
///
/// All regexes are compiled once and reused across calls via `OnceLock`.
pub(crate) fn strip_html(html: &str) -> String {
    static RE_SCRIPT: OnceLock<regex::Regex> = OnceLock::new();
    static RE_STYLE: OnceLock<regex::Regex> = OnceLock::new();
    static RE_BLOCK: OnceLock<regex::Regex> = OnceLock::new();
    static RE_TAG: OnceLock<regex::Regex> = OnceLock::new();
    static RE_SPACES: OnceLock<regex::Regex> = OnceLock::new();
    static RE_NEWLINES: OnceLock<regex::Regex> = OnceLock::new();

    let re_script =
        RE_SCRIPT.get_or_init(|| regex::Regex::new(r"(?si)<script[^>]*>.*?</script>").unwrap());
    let re_style =
        RE_STYLE.get_or_init(|| regex::Regex::new(r"(?si)<style[^>]*>.*?</style>").unwrap());
    let re_block = RE_BLOCK.get_or_init(|| {
        regex::Regex::new(
            r"(?i)</?(?:p|div|h[1-6]|li|tr|br|hr|blockquote|pre|article|section|header|footer|nav|main)[^>]*>",
        )
        .unwrap()
    });
    let re_tag = RE_TAG.get_or_init(|| regex::Regex::new(r"<[^>]+>").unwrap());
    let re_spaces = RE_SPACES.get_or_init(|| regex::Regex::new(r"[ \t]+").unwrap());
    let re_newlines = RE_NEWLINES.get_or_init(|| regex::Regex::new(r"\n{3,}").unwrap());

    // 1. Drop script / style blocks (content included).
    let s = re_script.replace_all(html, " ");
    let s = re_style.replace_all(&s, " ");

    // 2. Block-level tags → newline so paragraphs break visually.
    let s = re_block.replace_all(&s, "\n");

    // 3. Strip all remaining tags.
    let s = re_tag.replace_all(&s, "");

    // 4. Decode common HTML entities.
    let s = s
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…")
        .replace("&laquo;", "«")
        .replace("&raquo;", "»");

    // 5. Collapse runs of whitespace; preserve paragraph breaks (2+ newlines → blank line).
    let s = re_spaces.replace_all(&s, " ");
    let s = re_newlines.replace_all(&s, "\n\n");

    s.trim().to_string()
}

/// Return `true` if the URL's host is localhost, a loopback address, or a
/// private/link-local IP range — any destination that should not be reachable
/// from an SSRF attack.
pub(crate) fn is_private_host(url: &str) -> bool {
    // Strip scheme.
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    // Strip path, query, fragment — everything after the first '/', '?', '#'.
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);

    // Strip userinfo (e.g. "user:pass@host" → "host").  Without this an
    // attacker could bypass the private-host check with a URL like
    // http://x@192.168.1.1/ whose authority parses as host "x@192.168.1.1",
    // which fails the IPv4 parse and slips through.
    let host_with_port = authority.rsplit('@').next().unwrap_or(authority);

    // Strip port.  IPv6 addresses are enclosed in brackets: [::1]:8080.
    let host = if host_with_port.starts_with('[') {
        host_with_port
            .trim_start_matches('[')
            .split(']')
            .next()
            .unwrap_or(host_with_port)
    } else {
        // For plain hostnames / IPv4 we can't just split on ':' because colons
        // appear in IPv6 too — but we've already handled the bracket form above,
        // so here it's safe to strip the port with rsplit_once.
        host_with_port
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(host_with_port)
    };

    let host_lc = host.to_ascii_lowercase();

    // Textual localhost / unspecified.
    if matches!(host_lc.as_str(), "localhost" | "::1" | "::" | "0.0.0.0") {
        return true;
    }
    // .local mDNS names resolve to the LAN.
    if host_lc.ends_with(".local") {
        return true;
    }

    // IPv4 private / loopback / link-local ranges.
    if let Ok(ipv4) = host.parse::<std::net::Ipv4Addr>() {
        let o = ipv4.octets();
        return o[0] == 127                                    // 127.0.0.0/8 loopback
            || o[0] == 10                                     // 10.0.0.0/8
            || (o[0] == 172 && (16..=31).contains(&o[1]))    // 172.16.0.0/12
            || (o[0] == 192 && o[1] == 168)                  // 192.168.0.0/16
            || (o[0] == 169 && o[1] == 254); // 169.254.0.0/16 IMDS / link-local
    }

    // IPv6: loopback, unspecified, unique-local (fc00::/7), link-local (fe80::/10),
    // and IPv4-mapped addresses (::ffff:x.x.x.x) — re-check the embedded IPv4.
    if let Ok(ipv6) = host.parse::<std::net::Ipv6Addr>() {
        if ipv6.is_loopback() || ipv6.is_unspecified() {
            return true;
        }
        let segs = ipv6.segments();
        if (segs[0] & 0xfe00) == 0xfc00 || (segs[0] & 0xffc0) == 0xfe80 {
            return true;
        }
        // IPv4-mapped IPv6 (::ffff:x.x.x.x) — extract the IPv4 and re-check.
        if let Some(ipv4) = ipv6.to_ipv4_mapped() {
            let o = ipv4.octets();
            return o[0] == 127
                || o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 169 && o[1] == 254)
                || o == [0, 0, 0, 0];
        }
        return false;
    }

    false
}

/// Resolve the URL's hostname via DNS and check whether *any* resolved IP
/// falls into a private range.  Returns `true` (blocked) if resolution
/// yields at least one private IP, or if the hostname cannot be resolved
/// (fail-closed).
pub(crate) fn resolves_to_private(url: &str) -> bool {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    let host_with_port = authority.rsplit('@').next().unwrap_or(authority);

    // Ensure we have host:port for ToSocketAddrs.
    let addr_str = if host_with_port.contains(':') && !host_with_port.starts_with('[') {
        // Already has a port or is bare IPv6 — try as-is, then with default port.
        host_with_port.to_string()
    } else if host_with_port.starts_with('[') {
        // Bracketed IPv6 — may or may not have port.
        if host_with_port.contains("]:") {
            host_with_port.to_string()
        } else {
            format!("{}:80", host_with_port)
        }
    } else {
        format!("{host_with_port}:80")
    };

    use std::net::ToSocketAddrs;
    let Ok(addrs) = addr_str.to_socket_addrs() else {
        // Resolution failed — fail-closed: block the request.
        return true;
    };
    for addr in addrs {
        match addr.ip() {
            std::net::IpAddr::V4(ip) => {
                let o = ip.octets();
                if o[0] == 127
                    || o[0] == 10
                    || (o[0] == 172 && (16..=31).contains(&o[1]))
                    || (o[0] == 192 && o[1] == 168)
                    || (o[0] == 169 && o[1] == 254)
                    || o == [0, 0, 0, 0]
                {
                    return true;
                }
            }
            std::net::IpAddr::V6(ip) => {
                if ip.is_loopback() || ip.is_unspecified() {
                    return true;
                }
                let segs = ip.segments();
                if (segs[0] & 0xfe00) == 0xfc00 || (segs[0] & 0xffc0) == 0xfe80 {
                    return true;
                }
                if let Some(ipv4) = ip.to_ipv4_mapped() {
                    let o = ipv4.octets();
                    if o[0] == 127
                        || o[0] == 10
                        || (o[0] == 172 && (16..=31).contains(&o[1]))
                        || (o[0] == 192 && o[1] == 168)
                        || (o[0] == 169 && o[1] == 254)
                        || o == [0, 0, 0, 0]
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn executor(confirm_writes: bool, confirm_shell: bool) -> ToolExecutor {
        ToolExecutor {
            confirm_writes,
            confirm_shell,
            ..Default::default()
        }
    }

    // ── read_file ─────────────────────────────────────────────────────────────

    #[test]
    fn read_file_returns_content() {
        let path = std::env::temp_dir().join("shio_tool_read.txt");
        fs::write(&path, "hello tool").unwrap();
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "read_file",
            &serde_json::json!({ "path": path.to_str().unwrap() }).to_string(),
        );
        assert_eq!(result, "hello tool");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn read_file_missing_returns_error() {
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "read_file",
            &serde_json::json!({ "path": "/nonexistent/shio_missing.txt" }).to_string(),
        );
        assert!(result.starts_with("Error"), "{result}");
    }

    // ── write_file ────────────────────────────────────────────────────────────

    #[test]
    fn write_file_creates_file() {
        let path = std::env::temp_dir().join("shio_tool_write.txt");
        let _ = fs::remove_file(&path);
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "write_file",
            &serde_json::json!({ "path": path.to_str().unwrap(), "content": "written" })
                .to_string(),
        );
        assert!(
            result.contains("written") || result.contains("bytes"),
            "{result}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "written");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn write_file_preserves_pipe_table_content() {
        // write_file must NOT strip "  N | text" patterns — users may write
        // loose documents with pipe-separated tables or similar structure.
        let path = std::env::temp_dir().join("shio_write_preserve.txt");
        let _ = fs::remove_file(&path);
        let ex = executor(false, false);
        ex.vm.lock().unwrap().call_tool(
            "write_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "content": "  3 | value\n| col | col |"
            })
            .to_string(),
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "  3 | value\n| col | col |"
        );
        let _ = fs::remove_file(&path);
    }

    // ── insert_after_line ─────────────────────────────────────────────────────

    #[test]
    fn insert_after_line_inserts_in_middle() {
        let path = std::env::temp_dir().join("shio_insert_mid.txt");
        fs::write(&path, "line1\nline2\nline3\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "insert_after_line",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "line": 2,
                "content": "inserted"
            })
            .to_string(),
        );
        assert!(out.contains("Inserted"), "{out}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "line1\nline2\ninserted\nline3\n"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn insert_after_line_at_end_appends() {
        let path = std::env::temp_dir().join("shio_insert_end.txt");
        fs::write(&path, "line1\nline2\n").unwrap();
        let ex = executor(false, false);
        ex.vm.lock().unwrap().call_tool(
            "insert_after_line",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "line": 2,
                "content": "appended"
            })
            .to_string(),
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "line1\nline2\nappended\n"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn insert_after_line_after_line_zero_prepends() {
        let path = std::env::temp_dir().join("shio_insert_zero.txt");
        fs::write(&path, "line1\nline2\n").unwrap();
        let ex = executor(false, false);
        ex.vm.lock().unwrap().call_tool(
            "insert_after_line",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "line": 0,
                "content": "prepended"
            })
            .to_string(),
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "prepended\nline1\nline2\n"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn insert_after_line_out_of_range_returns_error() {
        let path = std::env::temp_dir().join("shio_insert_oor.txt");
        fs::write(&path, "line1\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "insert_after_line",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "line": 99,
                "content": "x"
            })
            .to_string(),
        );
        assert!(out.starts_with("Error"), "{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn insert_after_line_accepts_string_line_number() {
        // Local models sometimes stringify numbers; must tolerate "42" as well as 42.
        let path = std::env::temp_dir().join("shio_insert_strnum.txt");
        fs::write(&path, "line1\nline2\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "insert_after_line",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "line": "1",
                "content": "inserted"
            })
            .to_string(),
        );
        assert!(!out.starts_with("Error"), "{out}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "line1\ninserted\nline2\n"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn insert_after_line_preserves_pipe_table_content() {
        // insert_after_line must NOT strip "  N | text" — same reason as write_file.
        let path = std::env::temp_dir().join("shio_insert_preserve.txt");
        fs::write(&path, "line1\nline2\n").unwrap();
        let ex = executor(false, false);
        ex.vm.lock().unwrap().call_tool(
            "insert_after_line",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "line": 1,
                "content": "  3 | value"
            })
            .to_string(),
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "line1\n  3 | value\nline2\n"
        );
        let _ = fs::remove_file(&path);
    }

    // ── list_directory ────────────────────────────────────────────────────────

    #[test]
    fn list_directory_shows_entries() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "list_directory",
            &serde_json::json!({ "path": "src" }).to_string(),
        );
        assert!(out.contains("main.rs"), "{out}");
    }

    // ── run_shell ─────────────────────────────────────────────────────────────

    #[test]
    fn run_shell_captures_stdout() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "run_shell",
            &serde_json::json!({ "command": "echo hello" }).to_string(),
        );
        assert!(out.contains("hello"), "{out}");
    }

    #[test]
    fn run_shell_includes_exit_code_on_failure() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "run_shell",
            &serde_json::json!({ "command": "exit 1" }).to_string(),
        );
        assert!(out.contains("exit code"), "{out}");
    }

    // ── shell_command_tokens ───────────────────────────────────────────────

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

    // ── check_shell_policy ───────────────────────────────────────────────────

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
        // Allowed by allowlist but blocked by denylist.
        let allow = vec!["git".to_string(), "rm".to_string()];
        let deny = vec!["rm".to_string()];
        let r = check_shell_policy("rm -rf /", &allow, &deny);
        assert!(r.is_err());
    }

    #[test]
    fn run_shell_blocked_by_denylist() {
        let mut ex = executor(false, false);
        ex.shell_denylist = vec!["rm".to_string()];
        // The denylist is checked via the thread-local set by execute_quiet,
        // so we need to go through execute_quiet (not vm.call_tool directly).
        let call = ToolCallItem {
            id: "1".to_string(),
            kind: "function".to_string(),
            function: crate::client::ToolCallFunction {
                name: "run_shell".to_string(),
                arguments: serde_json::json!({ "command": "rm -rf /tmp/junk" }).to_string(),
            },
        };
        let out = ex.execute_quiet(&call);
        assert!(out.contains("denylist"), "{out}");
    }

    // ── search_files ─────────────────────────────────────────────────────────

    #[test]
    fn search_files_finds_rust_sources() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "search_files",
            &serde_json::json!({ "pattern": "src/*.rs" }).to_string(),
        );
        assert!(out.contains("main.rs"), "{out}");
    }

    // ── grep_files ────────────────────────────────────────────────────────────

    #[test]
    fn grep_files_finds_pattern() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "grep_files",
            &serde_json::json!({ "pattern": "fn main", "path": "src/main.rs" }).to_string(),
        );
        assert!(out.contains("fn main"), "{out}");
    }

    // ── read_file_range ───────────────────────────────────────────────────────

    #[test]
    fn read_file_range_returns_numbered_lines() {
        let path = std::env::temp_dir().join("shio_range.txt");
        fs::write(&path, "line1\nline2\nline3\nline4\nline5\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "read_file_range",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "start_line": 2,
                "end_line": 4
            })
            .to_string(),
        );
        assert!(out.contains("line2"), "{out}");
        assert!(out.contains("line4"), "{out}");
        assert!(!out.contains("line1"), "{out}");
        assert!(!out.contains("line5"), "{out}");
        // Header must report the range so the model knows where it is.
        assert!(out.contains("Lines 2"), "{out}");
        assert!(out.contains("Lines 2") && out.contains('4'), "{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn read_file_range_without_end_reads_to_eof() {
        let path = std::env::temp_dir().join("shio_range2.txt");
        fs::write(&path, "a\nb\nc\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "read_file_range",
            &serde_json::json!({ "path": path.to_str().unwrap(), "start_line": 2 }).to_string(),
        );
        assert!(out.contains('b'), "{out}");
        assert!(out.contains('c'), "{out}");
        // "a\n" would be the first line content; the header may contain path chars
        assert!(!out.contains("\na\n") && !out.ends_with("\na"), "{out}");
        let _ = fs::remove_file(&path);
    }

    // ── patch_file ────────────────────────────────────────────────────────────

    #[test]
    fn patch_file_replaces_exact_match() {
        let path = std::env::temp_dir().join("shio_patch.txt");
        fs::write(&path, "hello world\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "hello",
                "new_str": "goodbye"
            })
            .to_string(),
        );
        assert!(out.contains("Patched"), "{out}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "goodbye world\n");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_errors_when_old_str_not_found() {
        let path = std::env::temp_dir().join("shio_patch2.txt");
        fs::write(&path, "hello world\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "nonexistent",
                "new_str": "x"
            })
            .to_string(),
        );
        assert!(out.starts_with("Error"), "{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_errors_when_old_str_ambiguous() {
        let path = std::env::temp_dir().join("shio_patch3.txt");
        fs::write(&path, "a a a\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "a",
                "new_str": "b"
            })
            .to_string(),
        );
        assert!(out.starts_with("Error"), "{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_fallback_tolerates_trailing_whitespace() {
        let path = std::env::temp_dir().join("shio_patch_fallback.txt");
        // File has trailing spaces on the second line.
        fs::write(&path, "fn foo() {\n    let x = 1;   \n}\n").unwrap();
        let ex = executor(false, false);
        // old_str has no trailing spaces — exact match would fail.
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "fn foo() {\n    let x = 1;\n}",
                "new_str": "fn foo() {\n    let x = 2;\n}",
            })
            .to_string(),
        );
        assert!(out.contains("Patched"), "{out}");
        assert!(out.contains("fallback"), "{out}");
        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("let x = 2;"), "{result}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_fallback_errors_when_ambiguous() {
        let path = std::env::temp_dir().join("shio_patch_fallback_ambig.txt");
        // Two identical blocks (old_str matches both via line-by-line).
        fs::write(&path, "fn a() {}\nfn a() {}\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "fn a() {}",
                "new_str": "fn b() {}",
            })
            .to_string(),
        );
        assert!(out.starts_with("Error"), "{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_fallback_rejects_whitespace_only_old_str() {
        // A whitespace-only old_str would match every blank line — must be rejected.
        let path = std::env::temp_dir().join("shio_patch_ws_guard.txt");
        fs::write(&path, "a\n\nb\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "   ",
                "new_str": "x",
            })
            .to_string(),
        );
        assert!(out.starts_with("Error"), "{out}");
        // File must be unchanged.
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\n\nb\n");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_fallback_preserves_trailing_newline_in_new_str() {
        // new_str ending with '\n' must be preserved verbatim (not dropped by split).
        let path = std::env::temp_dir().join("shio_patch_trail_nl.txt");
        fs::write(&path, "fn foo() {   \n}\n").unwrap(); // trailing space triggers fallback
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "fn foo() {\n}",
                "new_str": "fn foo() {\n    42\n}\n",
            })
            .to_string(),
        );
        assert!(out.contains("Patched"), "{out}");
        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("42"), "{result}");
        // File should end with exactly one newline.
        assert!(
            result.ends_with('\n'),
            "missing trailing newline: {result:?}"
        );
        assert!(
            !result.ends_with("\n\n"),
            "double trailing newline: {result:?}"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_anchor_fallback_tolerates_wrong_interior_lines() {
        // Simulate a model that reproduced 5 lines but got the middle one wrong.
        let path = std::env::temp_dir().join("shio_patch_anchor.txt");
        fs::write(
            &path,
            "fn foo() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n}\n",
        )
        .unwrap();
        let ex = executor(false, false);
        // Middle line differs from the file — exact and line-by-line fallbacks both fail.
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "fn foo() {\n    let a = 1;\n    let b = WRONG;\n    let c = 3;\n}",
                "new_str": "fn foo() {\n    42\n}",
            })
            .to_string(),
        );
        assert!(out.contains("Patched"), "{out}");
        assert!(out.contains("anchor"), "{out}");
        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("42"), "{result}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_anchor_fallback_rejects_ambiguous_anchors() {
        // Two blocks with the same first two and last two lines — anchor must refuse.
        let path = std::env::temp_dir().join("shio_patch_anchor_ambig.txt");
        fs::write(
            &path,
            "fn foo() {\n    let a = 1;\n    x\n    let z = 9;\n}\nfn foo() {\n    let a = 1;\n    y\n    let z = 9;\n}\n",
        )
        .unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "fn foo() {\n    let a = 1;\n    WRONG\n    let z = 9;\n}",
                "new_str": "fn bar() {}",
            })
            .to_string(),
        );
        assert!(out.starts_with("Error"), "{out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn patch_file_via_vm_call_tool() {
        // Verify patch_file is reachable through the Ruby VM (smoke test for registration).
        let path = std::env::temp_dir().join("shio_patch_vm.txt");
        fs::write(&path, "hello world").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "patch_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_str": "hello",
                "new_str": "goodbye"
            })
            .to_string(),
        );
        assert!(out.contains("Patched"), "{out}");
        let _ = fs::remove_file(&path);
    }

    // ── value_to_ruby string interpolation safety ──────────────────────────

    #[test]
    fn ruby_string_interpolation_is_escaped() {
        // Verify that Ruby #{} interpolation in tool args is escaped, not evaluated.
        let path = std::env::temp_dir().join("shio_interp_test.txt");
        let _ = fs::remove_file(&path);
        let ex = executor(false, false);
        // If #{} were NOT escaped, mRuby would try to evaluate `1+1` and write "2".
        let content = "before #{1+1} after";
        ex.vm.lock().unwrap().call_tool(
            "write_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "content": content
            })
            .to_string(),
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
        let _ = fs::remove_file(&path);
    }

    // ── delete_file ───────────────────────────────────────────────────────────

    #[test]
    fn delete_file_removes_existing_file() {
        let path = std::env::temp_dir().join("shio_delete.txt");
        fs::write(&path, "bye").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "delete_file",
            &serde_json::json!({ "path": path.to_str().unwrap() }).to_string(),
        );
        assert!(out.contains("Deleted"), "{out}");
        assert!(!path.exists());
    }

    #[test]
    fn delete_file_errors_on_missing_file() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "delete_file",
            &serde_json::json!({ "path": "/nonexistent/shio_gone.txt" }).to_string(),
        );
        assert!(out.starts_with("Error"), "{out}");
    }

    // ── move_file ─────────────────────────────────────────────────────────────

    #[test]
    fn move_file_renames_file() {
        let src = std::env::temp_dir().join("shio_move_src.txt");
        let dst = std::env::temp_dir().join("shio_move_dst.txt");
        let _ = fs::remove_file(&dst);
        fs::write(&src, "content").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "move_file",
            &serde_json::json!({
                "src": src.to_str().unwrap(),
                "dst": dst.to_str().unwrap()
            })
            .to_string(),
        );
        assert!(out.contains("Moved") || out.contains("→"), "{out}");
        assert!(!src.exists());
        assert!(dst.exists());
        let _ = fs::remove_file(&dst);
    }

    // ── lsp ───────────────────────────────────────────────────────────────────

    #[test]
    fn lsp_query_unsupported_extension_returns_error_message() {
        // .xyz is not a known language — exercises lsp::query through Ruby without
        // needing a real LSP server.
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "lsp",
            &serde_json::json!({
                "operation": "hover",
                "file": "test.xyz",
                "line": 1,
                "column": 1
            })
            .to_string(),
        );
        assert!(
            result.contains("No LSP server found") || result.contains("Error"),
            "expected error message, got: {result}"
        );
    }

    #[test]
    fn lsp_query_missing_file_argument() {
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "lsp",
            &serde_json::json!({ "operation": "hover" }).to_string(),
        );
        assert!(result.contains("missing 'file'"), "got: {result}");
    }

    #[test]
    fn lsp_query_dispatched_via_execute_quiet() {
        // Verify the lsp tool is reachable through the Ruby VM.
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "lsp",
            &serde_json::json!({"operation": "hover", "file": "test.xyz", "line": 1}).to_string(),
        );
        assert!(!result.starts_with("Error: unknown tool:"), "got: {result}");
    }

    // ── strip_html ────────────────────────────────────────────────────────────

    #[test]
    fn strip_html_removes_tags_and_decodes_entities() {
        let html = "<p>Hello &amp; <b>world</b>!&nbsp;It&#39;s fine.</p>";
        let out = strip_html(html);
        assert!(out.contains("Hello & world!"), "{out}");
        assert!(out.contains("It's fine."), "{out}");
        assert!(!out.contains('<'), "{out}");
    }

    #[test]
    fn strip_html_drops_script_and_style_content() {
        let html = "<style>body{color:red}</style><script>alert(1)</script><p>visible</p>";
        let out = strip_html(html);
        assert!(out.contains("visible"), "{out}");
        assert!(!out.contains("color:red"), "{out}");
        assert!(!out.contains("alert"), "{out}");
    }

    // ── fetch_url (scheme guard, no network) ─────────────────────────────────

    #[test]
    fn fetch_url_rejects_non_http_schemes() {
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "fetch_url",
            &serde_json::json!({ "url": "file:///etc/passwd" }).to_string(),
        );
        assert!(result.starts_with("Error"), "{result}");
        assert!(result.contains("http"), "{result}");
    }

    #[test]
    fn fetch_url_requires_url_argument() {
        let ex = executor(false, false);
        let result = ex
            .vm
            .lock()
            .unwrap()
            .call_tool("fetch_url", &serde_json::json!({}).to_string());
        assert!(result.starts_with("Error"), "{result}");
    }

    // ── create_directory ──────────────────────────────────────────────────────

    #[test]
    fn create_directory_creates_nested_dirs() {
        let dir = std::env::temp_dir().join("shio_test_mkdir/a/b/c");
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "create_directory",
            &serde_json::json!({ "path": dir.to_str().unwrap() }).to_string(),
        );
        assert!(result.contains("Created"), "{result}");
        assert!(dir.is_dir());
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("shio_test_mkdir"));
    }

    #[test]
    fn create_directory_is_idempotent() {
        let dir = std::env::temp_dir().join("shio_test_mkdir_exist");
        std::fs::create_dir_all(&dir).unwrap();
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "create_directory",
            &serde_json::json!({ "path": dir.to_str().unwrap() }).to_string(),
        );
        assert!(result.contains("Created"), "{result}");
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn create_directory_requires_path() {
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool("create_directory", "{}");
        assert!(result.starts_with("Error"), "{result}");
    }

    // ── get_working_directory ─────────────────────────────────────────────────

    #[test]
    fn get_working_directory_returns_nonempty_path() {
        let ex = executor(false, false);
        let result = ex
            .vm
            .lock()
            .unwrap()
            .call_tool("get_working_directory", "{}");
        assert!(!result.is_empty());
        assert!(!result.starts_with("Error"), "{result}");
    }

    // ── web_search ────────────────────────────────────────────────────────────

    #[test]
    fn web_search_requires_query() {
        let ex = executor(false, false);
        let result = ex
            .vm
            .lock()
            .unwrap()
            .call_tool("web_search", &serde_json::json!({}).to_string());
        assert!(result.starts_with("Error"), "{result}");
    }

    // ── save_memory ───────────────────────────────────────────────────────────

    #[test]
    fn save_memory_appends_to_file() {
        let path = std::env::temp_dir().join("shio_test_memory.md");
        let _ = fs::remove_file(&path);
        let path_str = path.to_str().unwrap();
        let ex = executor(false, false);

        let result = ex.vm.lock().unwrap().call_tool(
            "save_memory",
            &serde_json::json!({ "memory": "prefer snake_case", "file": path_str }).to_string(),
        );
        assert!(result.contains("Saved"), "{result}");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("prefer snake_case"), "{content}");

        // Duplicate is skipped.
        let result2 = ex.vm.lock().unwrap().call_tool(
            "save_memory",
            &serde_json::json!({ "memory": "prefer snake_case", "file": path_str }).to_string(),
        );
        assert!(result2.contains("skipped"), "{result2}");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_memory_requires_memory_arg() {
        let ex = executor(false, false);
        let result = ex
            .vm
            .lock()
            .unwrap()
            .call_tool("save_memory", &serde_json::json!({}).to_string());
        assert!(result.starts_with("Error"), "{result}");
    }

    // ── read_many_files ───────────────────────────────────────────────────────

    #[test]
    fn read_many_files_returns_all_contents() {
        let a = std::env::temp_dir().join("shio_rmf_a.txt");
        let b = std::env::temp_dir().join("shio_rmf_b.txt");
        fs::write(&a, "content_a").unwrap();
        fs::write(&b, "content_b").unwrap();
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "read_many_files",
            &serde_json::json!({
                "paths": [a.to_str().unwrap(), b.to_str().unwrap()]
            })
            .to_string(),
        );
        assert!(result.contains("content_a"), "{result}");
        assert!(result.contains("content_b"), "{result}");
        let _ = fs::remove_file(&a);
        let _ = fs::remove_file(&b);
    }

    #[test]
    fn read_many_files_reports_missing_file_inline() {
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "read_many_files",
            &serde_json::json!({
                "paths": ["/nonexistent/shio_rmf_missing.txt"]
            })
            .to_string(),
        );
        assert!(result.contains("Error"), "{result}");
    }

    #[test]
    fn read_many_files_requires_paths() {
        let ex = executor(false, false);
        let result = ex
            .vm
            .lock()
            .unwrap()
            .call_tool("read_many_files", &serde_json::json!({}).to_string());
        assert!(result.starts_with("Error"), "{result}");
    }

    // ── write_todos ───────────────────────────────────────────────────────────

    #[test]
    fn write_todos_creates_file_with_checkboxes() {
        let path = std::env::temp_dir().join("shio_todos_test.md");
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "write_todos",
            &serde_json::json!({
                "todos": [
                    { "task": "first task", "status": "completed" },
                    { "task": "second task", "status": "in_progress" },
                    { "task": "third task" }
                ],
                "file": path.to_str().unwrap()
            })
            .to_string(),
        );
        assert!(result.contains("3"), "{result}");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("[x] first task"), "{content}");
        assert!(content.contains("[-] second task"), "{content}");
        assert!(content.contains("[ ] third task"), "{content}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn write_todos_requires_todos() {
        let ex = executor(false, false);
        let result = ex
            .vm
            .lock()
            .unwrap()
            .call_tool("write_todos", &serde_json::json!({}).to_string());
        assert!(result.starts_with("Error"), "{result}");
    }

    // ── percent_decode ────────────────────────────────────────────────────────

    #[test]
    fn percent_decode_handles_basic_encoding() {
        assert_eq!(percent_decode("hello+world"), "hello world");
        assert_eq!(percent_decode("foo%2Fbar"), "foo/bar");
        assert_eq!(percent_decode("a%20b"), "a b");
    }

    #[test]
    fn percent_decode_handles_utf8_sequences() {
        // "é" encodes as %C3%A9 in UTF-8
        assert_eq!(percent_decode("%C3%A9"), "é");
    }

    // ── is_private_host ───────────────────────────────────────────────────────

    #[test]
    fn is_private_host_localhost() {
        assert!(is_private_host("http://localhost/path"));
        assert!(is_private_host("https://localhost:8080/x"));
    }

    #[test]
    fn is_private_host_loopback_ipv4() {
        assert!(is_private_host("http://127.0.0.1/"));
        assert!(is_private_host("http://127.1.2.3/"));
    }

    #[test]
    fn is_private_host_private_ipv4_ranges() {
        assert!(is_private_host("http://10.0.0.1/"));
        assert!(is_private_host("http://10.255.255.255/"));
        assert!(is_private_host("http://172.16.0.1/"));
        assert!(is_private_host("http://172.31.255.255/"));
        assert!(is_private_host("http://192.168.1.100/"));
    }

    #[test]
    fn is_private_host_link_local_imds() {
        // 169.254.169.254 is the AWS/GCP IMDS endpoint — must be blocked.
        assert!(is_private_host("http://169.254.169.254/latest/meta-data/"));
    }

    #[test]
    fn is_private_host_ipv6_loopback() {
        assert!(is_private_host("http://[::1]/"));
        assert!(is_private_host("http://[::1]:9000/"));
    }

    #[test]
    fn is_private_host_ipv6_unique_local() {
        // fc00::/7 — unique local addresses (fd00:: is in range, fc00:: is too)
        assert!(is_private_host("http://[fd12:3456:789a::1]/"));
        assert!(is_private_host("http://[fc00::1]/path"));
    }

    #[test]
    fn is_private_host_ipv6_link_local() {
        // fe80::/10 — link-local addresses
        assert!(is_private_host("http://[fe80::1]/"));
        assert!(is_private_host("http://[fe80::dead:beef]:8080/api"));
    }

    #[test]
    fn is_private_host_ipv6_public_returns_false() {
        // 2001:db8::/32 is documentation range (publicly routable prefix)
        assert!(!is_private_host("https://[2001:db8::1]/"));
        assert!(!is_private_host("https://[2606:4700:4700::1111]/dns")); // Cloudflare
    }

    #[test]
    fn is_private_host_mdns_local() {
        assert!(is_private_host("http://mydevice.local/api"));
    }

    #[test]
    fn is_private_host_public_address_returns_false() {
        assert!(!is_private_host("https://example.com/"));
        assert!(!is_private_host("https://8.8.8.8/dns"));
        assert!(!is_private_host("https://172.32.0.1/")); // just outside 172.16-31
    }

    #[test]
    fn is_private_host_strips_port_correctly() {
        assert!(is_private_host("http://192.168.0.1:3000/api"));
        assert!(!is_private_host("http://93.184.216.34:443/"));
    }

    #[test]
    fn is_private_host_ipv4_mapped_ipv6_loopback() {
        assert!(is_private_host("http://[::ffff:127.0.0.1]/"));
        assert!(is_private_host("http://[::ffff:127.0.0.1]:8080/"));
    }

    #[test]
    fn is_private_host_ipv4_mapped_ipv6_private() {
        assert!(is_private_host("http://[::ffff:10.0.0.1]/"));
        assert!(is_private_host("http://[::ffff:192.168.1.1]/"));
        assert!(is_private_host("http://[::ffff:169.254.169.254]/"));
    }

    #[test]
    fn is_private_host_ipv4_mapped_ipv6_public_returns_false() {
        assert!(!is_private_host("http://[::ffff:93.184.216.34]/"));
    }

    #[test]
    fn is_private_host_ipv6_unspecified() {
        assert!(is_private_host("http://[::]/"));
        assert!(is_private_host("http://[::]:8080/"));
    }

    #[test]
    fn resolves_to_private_blocks_localhost() {
        // "localhost" should resolve to 127.0.0.1 or ::1 on all platforms.
        assert!(resolves_to_private("http://localhost/"));
        assert!(resolves_to_private("http://localhost:8080/path"));
    }

    // ── write_file — parent directory auto-creation ───────────────────────────

    #[test]
    fn write_file_creates_parent_dirs() {
        let dir = std::env::temp_dir().join("shio_write_nested/a/b");
        let path = dir.join("out.txt");
        let ex = executor(false, false);
        let result = ex.vm.lock().unwrap().call_tool(
            "write_file",
            &serde_json::json!({ "path": path.to_str().unwrap(), "content": "nested" }).to_string(),
        );
        assert!(
            result.contains("bytes") || result.contains("nested"),
            "{result}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "nested");
        let _ = fs::remove_dir_all(std::env::temp_dir().join("shio_write_nested"));
    }

    // ── run_shell — missing command argument ──────────────────────────────────

    #[test]
    fn run_shell_missing_command_returns_error() {
        let ex = executor(false, false);
        let out = ex
            .vm
            .lock()
            .unwrap()
            .call_tool("run_shell", &serde_json::json!({}).to_string());
        assert!(
            out.starts_with("Error"),
            "expected error for missing command, got: {out}"
        );
    }

    // ── grep_files — case-insensitive and invalid regex ───────────────────────

    #[test]
    fn grep_files_case_insensitive_flag() {
        let path = std::env::temp_dir().join("shio_grep_ci.txt");
        fs::write(&path, "Hello World\nlower case\n").unwrap();
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "grep_files",
            &serde_json::json!({
                "pattern": "hello",
                "path": path.to_str().unwrap(),
                "case_insensitive": true
            })
            .to_string(),
        );
        assert!(out.contains("Hello World"), "got: {out}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn grep_files_invalid_regex_returns_error() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "grep_files",
            &serde_json::json!({ "pattern": "[invalid(regex", "path": "src" }).to_string(),
        );
        assert!(
            out.contains("Invalid regex") || out.starts_with("Error"),
            "got: {out}"
        );
    }

    // ── append_file ───────────────────────────────────────────────────────────

    #[test]
    fn append_file_creates_new_file() {
        let path = std::env::temp_dir().join("shio_append_new.txt");
        let _ = fs::remove_file(&path);
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "append_file",
            &serde_json::json!({ "path": path.to_str().unwrap(), "content": "hello" }).to_string(),
        );
        assert!(out.contains("Appended"), "{out}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn append_file_preserves_existing_content() {
        let path = std::env::temp_dir().join("shio_append_existing.txt");
        fs::write(&path, "line1\n").unwrap();
        let ex = executor(false, false);
        ex.vm.lock().unwrap().call_tool(
            "append_file",
            &serde_json::json!({ "path": path.to_str().unwrap(), "content": "line2\n" })
                .to_string(),
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "line1\nline2\n");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn append_file_creates_parent_directories() {
        let dir = std::env::temp_dir().join("shio_append_dir_test");
        let path = dir.join("sub").join("file.txt");
        let _ = fs::remove_dir_all(&dir);
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "append_file",
            &serde_json::json!({ "path": path.to_str().unwrap(), "content": "data" }).to_string(),
        );
        assert!(out.contains("Appended"), "{out}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "data");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_file_missing_path_returns_error() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "append_file",
            &serde_json::json!({ "content": "data" }).to_string(),
        );
        assert_eq!(out, "Error: missing 'path' argument");
    }

    #[test]
    fn append_file_missing_content_returns_error() {
        let ex = executor(false, false);
        let out = ex.vm.lock().unwrap().call_tool(
            "append_file",
            &serde_json::json!({ "path": "/tmp/shio_whatever.txt" }).to_string(),
        );
        assert_eq!(out, "Error: missing 'content' argument");
    }

    #[test]
    fn append_file_preserves_pipe_table_content() {
        // append_file must NOT strip "  N | text" — same reason as write_file.
        let path = std::env::temp_dir().join("shio_append_preserve.txt");
        let _ = fs::remove_file(&path);
        let ex = executor(false, false);
        ex.vm.lock().unwrap().call_tool(
            "append_file",
            &serde_json::json!({
                "path": path.to_str().unwrap(),
                "content": "  3 | value"
            })
            .to_string(),
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "  3 | value");
        let _ = fs::remove_file(&path);
    }

    // ── execute_quiet: argument-unwrap edge cases ─────────────────────────────

    #[test]
    fn execute_quiet_does_not_unwrap_non_matching_single_key() {
        // A single-key object whose key does NOT match the function name must
        // not be unwrapped — it should go to dispatch as-is (and produce an
        // error about the missing argument, not a panic or wrong behaviour).
        use crate::client::{ToolCallFunction, ToolCallItem};
        let ex = executor(false, false);
        let call = ToolCallItem {
            id: "x".into(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: "get_working_directory".into(),
                // Single key but named "other", not "get_working_directory".
                arguments: serde_json::json!({ "other": {} }).to_string(),
            },
        };
        // get_working_directory ignores args entirely, so it must still succeed.
        let out = ex.execute_quiet(&call);
        assert!(!out.starts_with("Error"), "{out}");
    }

    #[test]
    fn execute_quiet_invalid_json_returns_error() {
        use crate::client::{ToolCallFunction, ToolCallItem};
        let ex = executor(false, false);
        let call = ToolCallItem {
            id: "x".into(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: "read_file".into(),
                arguments: "not json at all".into(),
            },
        };
        let out = ex.execute_quiet(&call);
        assert!(out.starts_with("Error parsing arguments"), "{out}");
    }
}
