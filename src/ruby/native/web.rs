// SPDX-License-Identifier: GPL-3.0-or-later
use super::context::{set_err, set_result};
use std::ffi::{CStr, c_char};
use std::io::Read;
use std::ptr;
use std::sync::OnceLock;

const BODY_LIMIT: usize = 2 * 1024 * 1024;

fn static_regex(pattern: &'static str) -> regex::Regex {
    regex::Regex::new(pattern).expect("valid static native web regex")
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_fetch_url(
    url: *const c_char,
    max_chars: std::ffi::c_int,
    error_out: *mut *const c_char,
) -> *const c_char {
    let url = match unsafe { CStr::from_ptr(url) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            unsafe { set_err(error_out, "url is not valid UTF-8") };
            return ptr::null();
        }
    };
    let max_chars = if max_chars <= 0 {
        8_000usize
    } else {
        max_chars as usize
    };

    let client = match crate::tools::http_client() {
        Ok(c) => c,
        Err(e) => {
            unsafe { set_err(error_out, &e) };
            return ptr::null();
        }
    };

    match fetch_url_text(client, url, max_chars) {
        Ok(result) => set_result(result),
        Err(e) => {
            unsafe { set_err(error_out, &e) };
            ptr::null()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_web_search(
    query: *const c_char,
    max_results: std::ffi::c_int,
    error_out: *mut *const c_char,
) -> *const c_char {
    let query = match unsafe { CStr::from_ptr(query) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            unsafe { set_err(error_out, "query is not valid UTF-8") };
            return ptr::null();
        }
    };
    let max_results = (if max_results <= 0 {
        5
    } else {
        max_results as usize
    })
    .min(20);

    let encoded = percent_encode_query(query);
    let search_url = format!("https://lite.duckduckgo.com/lite/?q={encoded}");

    let client = match crate::tools::http_client() {
        Ok(c) => c,
        Err(e) => {
            unsafe { set_err(error_out, &e) };
            return ptr::null();
        }
    };

    let body = match client
        .get(&search_url)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .and_then(|r| r.text())
    {
        Ok(b) => b,
        Err(e) => {
            unsafe { set_err(error_out, &format!("Error fetching search results: {e}")) };
            return ptr::null();
        }
    };

    let results = parse_search_results(&body, max_results);
    if results.is_empty() {
        return set_result(format!(
            "No results found for: {query}\n\
             (try rephrasing or use fetch_url with a known URL)"
        ));
    }

    let snippets = parse_search_snippets(&body, max_results);
    let mut out = format!("Web search results for \"{query}\":\n\n");
    for (i, (title, url)) in results.iter().enumerate() {
        out.push_str(&format!("{}. {title}\n   {url}\n", i + 1));
        if let Some(snippet) = snippets.get(i)
            && !snippet.is_empty()
        {
            out.push_str(&format!("   {snippet}\n"));
        }
        out.push('\n');
    }

    set_result(out.trim_end().to_string())
}

fn percent_encode_query(query: &str) -> String {
    let mut encoded = String::new();
    for c in query.chars() {
        if c == ' ' {
            encoded.push('+');
        } else if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            encoded.push(c);
        } else {
            for byte in c.encode_utf8(&mut [0u8; 4]).as_bytes() {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    encoded
}

fn parse_search_results(body: &str, max_results: usize) -> Vec<(String, String)> {
    static RE_RESULT: OnceLock<regex::Regex> = OnceLock::new();
    static RE_UDDG: OnceLock<regex::Regex> = OnceLock::new();

    let re_result = RE_RESULT
        .get_or_init(|| static_regex(r#"(?s)<a[^>]+href="[^"]*uddg=[^"]*"[^>]*>(.*?)</a>"#));
    let re_uddg = RE_UDDG.get_or_init(|| static_regex(r"uddg=([^&\s]+)"));

    re_result
        .captures_iter(body)
        .filter_map(|cap| {
            let full = cap.get(0)?.as_str();
            let title = crate::tools::strip_html(cap.get(1)?.as_str())
                .trim()
                .to_string();
            if title.is_empty() {
                return None;
            }
            let real_url = re_uddg
                .captures(full)
                .and_then(|c| c.get(1))
                .map(|m| crate::tools::percent_decode(m.as_str()))
                .unwrap_or_default();
            if real_url.is_empty() {
                return None;
            }
            Some((title, real_url))
        })
        .take(max_results)
        .collect()
}

fn parse_search_snippets(body: &str, max_results: usize) -> Vec<String> {
    static RE_SNIPPET: OnceLock<regex::Regex> = OnceLock::new();

    let re_snippet = RE_SNIPPET
        .get_or_init(|| static_regex(r#"(?s)<td[^>]*class="result-snippet"[^>]*>(.*?)</td>"#));
    re_snippet
        .captures_iter(body)
        .map(|cap| {
            crate::tools::strip_html(cap.get(1).map_or("", |m| m.as_str()))
                .trim()
                .to_string()
        })
        .take(max_results)
        .collect()
}

fn fetch_url_text(
    client: &reqwest::blocking::Client,
    url: &str,
    max_chars: usize,
) -> Result<String, String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(format!(
            "only http:// and https:// URLs are supported, got: {url}"
        ));
    }

    if crate::tools::is_private_host(url) || crate::tools::resolves_to_private(url) {
        return Err(format!(
            "requests to localhost and private network addresses are not allowed: {url}"
        ));
    }

    let response = client
        .get(url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .map_err(|e| format!("Error fetching {url}: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("server returned {} for {url}", response.status()));
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body = read_limited_text(response, BODY_LIMIT)
        .map_err(|e| format!("Error reading response body: {e}"))?;
    let text = if content_type.contains("text/html") {
        crate::tools::strip_html(&body)
    } else {
        body
    };

    Ok(truncate_tool_text(text.trim(), max_chars))
}

fn truncate_tool_text(text: &str, max_chars: usize) -> String {
    if text.len() > max_chars {
        let limit = text.floor_char_boundary(max_chars);
        format!(
            "{}\n\n[… truncated at {max_chars} chars — use max_chars to get more]",
            &text[..limit]
        )
    } else {
        text.to_string()
    }
}

fn read_limited_text<R: Read>(mut reader: R, limit: usize) -> std::io::Result<String> {
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut limited = (&mut reader).take(limit as u64 + 1);
    limited.read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        bytes.truncate(limit);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::{read_limited_text, truncate_tool_text};
    use std::io::Cursor;

    #[test]
    fn read_limited_text_stops_at_limit() {
        let body = vec![b'x'; 2 * 1024 * 1024 + 512];
        let text = read_limited_text(Cursor::new(body), 2 * 1024 * 1024).unwrap();
        assert_eq!(text.len(), 2 * 1024 * 1024);
        assert!(text.chars().all(|c| c == 'x'));
    }

    #[test]
    fn read_limited_text_allows_short_body() {
        let text = read_limited_text(Cursor::new("hello".as_bytes()), 1024).unwrap();
        assert_eq!(text, "hello");
    }

    #[test]
    fn truncate_tool_text_respects_char_boundaries() {
        let text = truncate_tool_text("aé日", 3);
        assert!(text.starts_with("aé"), "{text}");
        assert!(text.contains("truncated"), "{text}");
    }

    #[test]
    fn truncate_tool_text_returns_short_text_unchanged() {
        assert_eq!(truncate_tool_text("short", 100), "short");
    }
}
