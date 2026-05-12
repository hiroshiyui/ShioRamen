// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::OnceLock;

fn static_regex(pattern: &'static str) -> regex::Regex {
    regex::Regex::new(pattern).expect("valid static web regex")
}

// ── Shared HTTP client ────────────────────────────────────────────────────────

/// Return the process-wide blocking HTTP client, or an error string if the
/// TLS backend failed to initialise. The client is constructed once and
/// reused; per-request timeouts are set by each call site via
/// `client.get(url).timeout(...)`.
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

/// Decode a percent-encoded URL string (e.g. `%2F` -> `/`, `+` -> space).
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

    let re_script = RE_SCRIPT.get_or_init(|| static_regex(r"(?si)<script[^>]*>.*?</script>"));
    let re_style = RE_STYLE.get_or_init(|| static_regex(r"(?si)<style[^>]*>.*?</style>"));
    let re_block = RE_BLOCK.get_or_init(|| {
        static_regex(
            r"(?i)</?(?:p|div|h[1-6]|li|tr|br|hr|blockquote|pre|article|section|header|footer|nav|main)[^>]*>",
        )
    });
    let re_tag = RE_TAG.get_or_init(|| static_regex(r"<[^>]+>"));
    let re_spaces = RE_SPACES.get_or_init(|| static_regex(r"[ \t]+"));
    let re_newlines = RE_NEWLINES.get_or_init(|| static_regex(r"\n{3,}"));

    // 1. Drop script / style blocks (content included).
    let s = re_script.replace_all(html, " ");
    let s = re_style.replace_all(&s, " ");

    // 2. Block-level tags -> newline so paragraphs break visually.
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

    // 5. Collapse runs of whitespace; preserve paragraph breaks (2+ newlines -> blank line).
    let s = re_spaces.replace_all(&s, " ");
    let s = re_newlines.replace_all(&s, "\n\n");

    s.trim().to_string()
}

/// Return `true` if the given IPv4 octets belong to a private, loopback,
/// link-local, or unspecified range.
fn is_private_ipv4(o: [u8; 4]) -> bool {
    o[0] == 127                                 // 127.0.0.0/8 loopback
        || o[0] == 10                            // 10.0.0.0/8
        || (o[0] == 172 && (16..=31).contains(&o[1])) // 172.16.0.0/12
        || (o[0] == 192 && o[1] == 168)         // 192.168.0.0/16
        || (o[0] == 169 && o[1] == 254)         // 169.254.0.0/16 IMDS / link-local
        || o == [0, 0, 0, 0] // unspecified
}

/// Return `true` if the URL's host is localhost, a loopback address, or a
/// private/link-local IP range -- any destination that should not be reachable
/// from an SSRF attack.
pub(crate) fn is_private_host(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return true;
    };
    let Some(host) = parsed.host_str() else {
        return true;
    };
    let host_lc = host.trim_matches(['[', ']']).to_ascii_lowercase();

    // Textual localhost / unspecified.
    if matches!(host_lc.as_str(), "localhost" | "::1" | "::" | "0.0.0.0") {
        return true;
    }
    // .local mDNS names resolve to the LAN.
    if host_lc.ends_with(".local") {
        return true;
    }

    // IPv4 private / loopback / link-local ranges.
    if let Ok(ipv4) = host_lc.parse::<std::net::Ipv4Addr>() {
        return is_private_ipv4(ipv4.octets());
    }

    // IPv6: loopback, unspecified, unique-local (fc00::/7), link-local (fe80::/10),
    // and IPv4-mapped addresses (::ffff:x.x.x.x) -- re-check the embedded IPv4.
    if let Ok(ipv6) = host_lc.parse::<std::net::Ipv6Addr>() {
        if ipv6.is_loopback() || ipv6.is_unspecified() {
            return true;
        }
        let segs = ipv6.segments();
        if (segs[0] & 0xfe00) == 0xfc00 || (segs[0] & 0xffc0) == 0xfe80 {
            return true;
        }
        if let Some(ipv4) = ipv6.to_ipv4_mapped() {
            return is_private_ipv4(ipv4.octets());
        }
        return false;
    }

    false
}

/// Resolve the URL's hostname via DNS and check whether *any* resolved IP
/// falls into a private range. Returns `true` (blocked) if resolution
/// yields at least one private IP, or if the hostname cannot be resolved
/// (fail-closed).
pub(crate) fn resolves_to_private(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return true;
    };
    let Some(host) = parsed.host_str() else {
        return true;
    };
    let Some(port) = parsed.port_or_known_default() else {
        return true;
    };
    use std::net::ToSocketAddrs;
    let Ok(addrs) = (host, port).to_socket_addrs() else {
        // Resolution failed -- fail-closed: block the request.
        return true;
    };
    for addr in addrs {
        match addr.ip() {
            std::net::IpAddr::V4(ip) => {
                if is_private_ipv4(ip.octets()) {
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
                if let Some(ipv4) = ip.to_ipv4_mapped()
                    && is_private_ipv4(ipv4.octets())
                {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // 169.254.169.254 is the AWS/GCP IMDS endpoint -- must be blocked.
        assert!(is_private_host("http://169.254.169.254/latest/meta-data/"));
    }

    #[test]
    fn is_private_host_ipv6_loopback() {
        assert!(is_private_host("http://[::1]/"));
        assert!(is_private_host("http://[::1]:9000/"));
    }

    #[test]
    fn is_private_host_ipv6_unique_local() {
        // fc00::/7 -- unique local addresses (fd00:: is in range, fc00:: is too)
        assert!(is_private_host("http://[fd12:3456:789a::1]/"));
        assert!(is_private_host("http://[fc00::1]/path"));
    }

    #[test]
    fn is_private_host_ipv6_link_local() {
        // fe80::/10 -- link-local addresses
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
    fn is_private_host_handles_userinfo_with_private_host() {
        assert!(is_private_host("http://user:pass@192.168.0.1/admin"));
        assert!(is_private_host("http://example.com@127.0.0.1/"));
    }

    #[test]
    fn is_private_host_invalid_url_fails_closed() {
        assert!(is_private_host("not a url"));
        assert!(is_private_host("http://"));
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

    #[test]
    fn resolves_to_private_invalid_url_fails_closed() {
        assert!(resolves_to_private("not a url"));
        assert!(resolves_to_private("http://"));
    }
}
