// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::missing_safety_doc)]

use std::ffi::{CStr, CString, c_char};
use std::ptr;

// ── thread-local string slots ────────────────────────────────────────────────
// Each slot holds the last CString returned (or set as error) by a native call
// on this thread.  The C caller (glue.c) copies the pointer immediately via
// mrb_str_new_cstr / mrb_raise, so one slot per direction is sufficient.

thread_local! {
    static LAST_ERR: std::cell::RefCell<Option<CString>> =
        const { std::cell::RefCell::new(None) };
    static LAST_RESULT: std::cell::RefCell<Option<CString>> =
        const { std::cell::RefCell::new(None) };
    /// Serialised LSP server config (`{"lang": "cmd", ...}`) set by the Rust
    /// executor just before each `call_tool` invocation.  Read by
    /// `shio_native_lsp_query` to pass user-configured servers to `lsp::query`.
    static LSP_CONFIG_JSON: std::cell::RefCell<String> =
        const { std::cell::RefCell::new(String::new()) };
    static SHELL_ALLOWLIST: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static SHELL_DENYLIST: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Called by `ToolExecutor::dispatch` before entering the Ruby VM so that
/// `shio_native_lsp_query` can resolve user-configured LSP server commands.
pub(crate) fn set_lsp_config_json(json: &str) {
    LSP_CONFIG_JSON.with(|c| *c.borrow_mut() = json.to_string());
}

pub(crate) fn set_shell_policy(allowlist: &[String], denylist: &[String]) {
    SHELL_ALLOWLIST.with(|c| *c.borrow_mut() = allowlist.to_vec());
    SHELL_DENYLIST.with(|c| *c.borrow_mut() = denylist.to_vec());
}

pub(super) unsafe fn set_err(error_out: *mut *const c_char, msg: &str) {
    let cstr = CString::new(msg).unwrap_or_else(|_| c"<error contains nul>".to_owned());
    LAST_ERR.with(|cell| {
        unsafe {
            *error_out = cstr.as_ptr();
        }
        *cell.borrow_mut() = Some(cstr);
    });
}

fn set_result(s: String) -> *const c_char {
    let cstr = CString::new(s).unwrap_or_else(|_| c"<result contains nul>".to_owned());
    LAST_RESULT.with(|cell| {
        let ptr = cstr.as_ptr();
        *cell.borrow_mut() = Some(cstr);
        ptr
    })
}

// ── Shio.current_dir ─────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_current_dir(error_out: *mut *const c_char) -> *const c_char {
    match std::env::current_dir().map(|p| p.display().to_string()) {
        Ok(s) => set_result(s),
        Err(e) => {
            unsafe { set_err(error_out, &e.to_string()) };
            ptr::null()
        }
    }
}

// ── Shio.create_dir_all ───────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_create_dir_all(
    path: *const c_char,
    error_out: *mut *const c_char,
) {
    let p = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            unsafe { set_err(error_out, "path is not valid UTF-8") };
            return;
        }
    };
    if let Err(e) = std::fs::create_dir_all(p) {
        unsafe { set_err(error_out, &e.to_string()) };
    }
}

// ── Shio.read_file ────────────────────────────────────────────────────────────
//
// Small bounded cache keyed on (path, mtime, len). Chunked read_file walks a
// large file in many Ruby calls; without this each call would reread+revalidate
// the entire file. Cache invalidates automatically when the file changes.

#[derive(Clone)]
struct CachedRead {
    path: String,
    mtime: std::time::SystemTime,
    len: u64,
    content: std::sync::Arc<String>,
}

const READ_CACHE_CAP: usize = 4;

fn read_cache() -> &'static std::sync::Mutex<Vec<CachedRead>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Vec<CachedRead>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(Vec::with_capacity(READ_CACHE_CAP)))
}

fn cached_read_file(path: &str) -> std::io::Result<std::sync::Arc<String>> {
    let meta = std::fs::metadata(path)?;
    let mtime = meta.modified()?;
    let len = meta.len();

    if let Ok(mut cache) = read_cache().lock()
        && let Some(pos) = cache
            .iter()
            .position(|c| c.path == path && c.mtime == mtime && c.len == len)
    {
        // Move hit to back (most-recently-used) so it survives eviction.
        let hit = cache.remove(pos);
        let content = hit.content.clone();
        cache.push(hit);
        return Ok(content);
    }

    let content = std::sync::Arc::new(std::fs::read_to_string(path)?);

    if let Ok(mut cache) = read_cache().lock() {
        if cache.len() >= READ_CACHE_CAP {
            cache.remove(0);
        }
        cache.push(CachedRead {
            path: path.to_string(),
            mtime,
            len,
            content: content.clone(),
        });
    }
    Ok(content)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_read_file(
    path: *const c_char,
    error_out: *mut *const c_char,
) -> *const c_char {
    let p = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            unsafe { set_err(error_out, "path is not valid UTF-8") };
            return ptr::null();
        }
    };
    match cached_read_file(p) {
        Ok(content) => set_result((*content).clone()),
        Err(e) => {
            // Provide a user-friendly hint for binary files (InvalidData from
            // a UTF-8 decode failure is the typical symptom).
            let msg = if e.kind() == std::io::ErrorKind::InvalidData {
                format!("{p}: file appears to be binary (not valid UTF-8)")
            } else {
                e.to_string()
            };
            unsafe { set_err(error_out, &msg) };
            ptr::null()
        }
    }
}

// ── Shio.read_dir ─────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_read_dir(
    path: *const c_char,
    error_out: *mut *const c_char,
) -> *const c_char {
    let p = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            unsafe { set_err(error_out, "path is not valid UTF-8") };
            return ptr::null();
        }
    };
    match std::fs::read_dir(p) {
        Err(e) => {
            unsafe { set_err(error_out, &e.to_string()) };
            ptr::null()
        }
        Ok(entries) => {
            let mut lines: Vec<String> = entries
                .filter_map(|e| e.ok())
                .map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if e.path().is_dir() {
                        format!("{name}/")
                    } else {
                        name
                    }
                })
                .collect();
            lines.sort();
            set_result(lines.join("\n"))
        }
    }
}

// ── Shio.delete_file ──────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_delete_file(
    path: *const c_char,
    error_out: *mut *const c_char,
) {
    let p = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            unsafe { set_err(error_out, "path is not valid UTF-8") };
            return;
        }
    };
    if let Err(e) = std::fs::remove_file(p) {
        unsafe { set_err(error_out, &e.to_string()) };
    }
}

// ── Shio.rename ───────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_rename(
    src: *const c_char,
    dst: *const c_char,
    error_out: *mut *const c_char,
) {
    let src_s = match unsafe { CStr::from_ptr(src) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            unsafe { set_err(error_out, "src path is not valid UTF-8") };
            return;
        }
    };
    let dst_s = match unsafe { CStr::from_ptr(dst) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            unsafe { set_err(error_out, "dst path is not valid UTF-8") };
            return;
        }
    };
    if let Some(parent) = std::path::Path::new(dst_s).parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        unsafe { set_err(error_out, &e.to_string()) };
        return;
    }
    if let Err(e) = std::fs::rename(src_s, dst_s) {
        unsafe { set_err(error_out, &e.to_string()) };
    }
}

// ── Shio.write_file ───────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_write_file(
    path: *const c_char,
    content: *const c_char,
    error_out: *mut *const c_char,
) {
    let p = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            unsafe { set_err(error_out, "path is not valid UTF-8") };
            return;
        }
    };
    let c = match unsafe { CStr::from_ptr(content) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            unsafe { set_err(error_out, "content is not valid UTF-8") };
            return;
        }
    };
    if let Some(parent) = std::path::Path::new(p).parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        unsafe { set_err(error_out, &e.to_string()) };
        return;
    }
    if let Err(e) = std::fs::write(p, c) {
        unsafe { set_err(error_out, &e.to_string()) };
    }
}

// ── Shio.append_file ──────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_append_file(
    path: *const c_char,
    content: *const c_char,
    error_out: *mut *const c_char,
) {
    let p = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            unsafe { set_err(error_out, "path is not valid UTF-8") };
            return;
        }
    };
    let c = match unsafe { CStr::from_ptr(content) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            unsafe { set_err(error_out, "content is not valid UTF-8") };
            return;
        }
    };
    if let Some(parent) = std::path::Path::new(p).parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        unsafe { set_err(error_out, &e.to_string()) };
        return;
    }
    use std::io::Write as _;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
    {
        Err(e) => unsafe { set_err(error_out, &e.to_string()) },
        Ok(mut f) => {
            if let Err(e) = f.write_all(c.as_bytes()) {
                unsafe { set_err(error_out, &e.to_string()) };
            }
        }
    }
}

// ── Shio.glob ─────────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_glob(
    pattern: *const c_char,
    error_out: *mut *const c_char,
) -> *const c_char {
    let pat = match unsafe { CStr::from_ptr(pattern) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            unsafe { set_err(error_out, "pattern is not valid UTF-8") };
            return ptr::null();
        }
    };
    match glob::glob(pat) {
        Err(e) => {
            unsafe { set_err(error_out, &format!("Invalid pattern: {e}")) };
            ptr::null()
        }
        Ok(paths) => {
            let matches: Vec<String> = paths
                .filter_map(|p| p.ok())
                .map(|p| p.display().to_string())
                .collect();
            set_result(matches.join("\n"))
        }
    }
}

// ── Shio.grep ─────────────────────────────────────────────────────────────────

fn grep_path_native(path: &std::path::Path, re: &regex::Regex, out: &mut Vec<String>) {
    if path.is_dir() {
        let skip = [".git", "target", "node_modules", "vendor"];
        if let Ok(entries) = std::fs::read_dir(path) {
            let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let p = entry.path();
                let name = entry.file_name();
                if p.is_dir() && skip.contains(&name.to_string_lossy().as_ref()) {
                    continue;
                }
                grep_path_native(&p, re, out);
            }
        }
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for (i, line) in text.lines().enumerate() {
        if re.is_match(line) {
            out.push(format!("{}:{}: {}", path.display(), i + 1, line));
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_grep(
    pattern: *const c_char,
    path: *const c_char,
    case_insensitive: std::ffi::c_int,
    error_out: *mut *const c_char,
) -> *const c_char {
    let pat = match unsafe { CStr::from_ptr(pattern) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            unsafe { set_err(error_out, "pattern is not valid UTF-8") };
            return ptr::null();
        }
    };
    let p = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            unsafe { set_err(error_out, "path is not valid UTF-8") };
            return ptr::null();
        }
    };
    let re = match regex::RegexBuilder::new(pat)
        .case_insensitive(case_insensitive != 0)
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            unsafe { set_err(error_out, &format!("Invalid regex: {e}")) };
            return ptr::null();
        }
    };
    let mut results = Vec::new();
    grep_path_native(std::path::Path::new(p), &re, &mut results);
    set_result(results.join("\n"))
}

// ── Shio.fetch_url ────────────────────────────────────────────────────────────

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

    if !url.starts_with("http://") && !url.starts_with("https://") {
        unsafe {
            set_err(
                error_out,
                &format!("only http:// and https:// URLs are supported, got: {url}"),
            )
        };
        return ptr::null();
    }

    if crate::tools::is_private_host(url) || crate::tools::resolves_to_private(url) {
        unsafe {
            set_err(
                error_out,
                &format!(
                    "requests to localhost and private network addresses are not allowed: {url}"
                ),
            )
        };
        return ptr::null();
    }

    let client = match crate::tools::http_client() {
        Ok(c) => c,
        Err(e) => {
            unsafe { set_err(error_out, &e) };
            return ptr::null();
        }
    };

    let response = match client
        .get(url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
    {
        Ok(r) => r,
        Err(e) => {
            unsafe { set_err(error_out, &format!("Error fetching {url}: {e}")) };
            return ptr::null();
        }
    };

    if !response.status().is_success() {
        unsafe {
            set_err(
                error_out,
                &format!("server returned {} for {url}", response.status()),
            )
        };
        return ptr::null();
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    const BODY_LIMIT: usize = 2 * 1024 * 1024;
    let body = match response.text() {
        Ok(t) => {
            if t.len() > BODY_LIMIT {
                t[..t.floor_char_boundary(BODY_LIMIT)].to_string()
            } else {
                t
            }
        }
        Err(e) => {
            unsafe { set_err(error_out, &format!("Error reading response body: {e}")) };
            return ptr::null();
        }
    };

    let text = if content_type.contains("text/html") {
        crate::tools::strip_html(&body)
    } else {
        body
    };

    let text = text.trim().to_string();
    let result = if text.len() > max_chars {
        let limit = text.floor_char_boundary(max_chars);
        format!(
            "{}\n\n[… truncated at {max_chars} chars — use max_chars to get more]",
            &text[..limit]
        )
    } else {
        text
    };

    set_result(result)
}

// ── Shio.web_search ───────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_web_search(
    query: *const c_char,
    max_results: std::ffi::c_int,
    error_out: *mut *const c_char,
) -> *const c_char {
    use std::sync::OnceLock;

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

    // Percent-encode the query for use in a URL.
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

    static RE_RESULT: OnceLock<regex::Regex> = OnceLock::new();
    static RE_SNIPPET: OnceLock<regex::Regex> = OnceLock::new();
    static RE_UDDG: OnceLock<regex::Regex> = OnceLock::new();

    let re_result = RE_RESULT.get_or_init(|| {
        regex::Regex::new(r#"(?s)<a[^>]+href="[^"]*uddg=[^"]*"[^>]*>(.*?)</a>"#).unwrap()
    });
    let re_snippet = RE_SNIPPET.get_or_init(|| {
        regex::Regex::new(r#"(?s)<td[^>]*class="result-snippet"[^>]*>(.*?)</td>"#).unwrap()
    });
    let re_uddg = RE_UDDG.get_or_init(|| regex::Regex::new(r"uddg=([^&\s]+)").unwrap());

    let results: Vec<(String, String)> = re_result
        .captures_iter(&body)
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
        .collect();

    if results.is_empty() {
        return set_result(format!(
            "No results found for: {query}\n\
             (try rephrasing or use fetch_url with a known URL)"
        ));
    }

    let snippets: Vec<String> = re_snippet
        .captures_iter(&body)
        .map(|cap| {
            crate::tools::strip_html(cap.get(1).map_or("", |m| m.as_str()))
                .trim()
                .to_string()
        })
        .take(max_results)
        .collect();

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

// ── Shio.run_shell(cmd) ──────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_run_shell(
    cmd: *const c_char,
    error_out: *mut *const c_char,
) -> *const c_char {
    let _ = error_out; // errors returned as strings, not C errors
    let cmd = unsafe { CStr::from_ptr(cmd) }.to_string_lossy();

    // Check shell allowlist/denylist before execution.
    let allow = SHELL_ALLOWLIST.with(|c| c.borrow().clone());
    let deny = SHELL_DENYLIST.with(|c| c.borrow().clone());
    if let Err(msg) = crate::tools::check_shell_policy(&cmd, &allow, &deny) {
        return set_result(format!("Error: {msg}"));
    }

    const SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    let child = match std::process::Command::new("sh")
        .args(["-c", &*cmd])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Err(e) => return set_result(format!("Error running command: {e}")),
        Ok(c) => c,
    };

    // Grab the PID so we can kill from the main thread on timeout.
    let pid = child.id();

    // Run wait_with_output() in a background thread so stdout/stderr are
    // drained continuously — avoids deadlock when the child produces more
    // output than the OS pipe buffer (~64 KB on Linux).
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    let out = match rx.recv_timeout(SHELL_TIMEOUT) {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return set_result(format!("Error reading command output: {e}")),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // Kill via raw PID — the Child was moved into the thread.
            // Use kill(2) directly via the nix-style raw syscall to avoid
            // adding a crate dependency for a single call.
            #[cfg(unix)]
            unsafe {
                // SAFETY: sending SIGKILL to a known PID we spawned.
                unsafe extern "C" {
                    fn kill(pid: i32, sig: i32) -> i32;
                }
                kill(pid as i32, 9); // SIGKILL = 9
            }
            return set_result(format!(
                "Error: command timed out after {}s",
                SHELL_TIMEOUT.as_secs()
            ));
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            return set_result("Error: command thread panicked".to_string());
        }
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("[stderr]\n");
        result.push_str(&stderr);
    }
    if !out.status.success() {
        result.push_str(&format!(
            "\n[exit code: {}]",
            out.status.code().unwrap_or(-1)
        ));
    }
    if result.is_empty() {
        set_result("(no output)".to_string())
    } else {
        set_result(result)
    }
}

// ── Shio.lsp_query(operation, file, line, col) ───────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_lsp_query(
    operation: *const c_char,
    file: *const c_char,
    line: std::ffi::c_int,
    col: std::ffi::c_int,
    error_out: *mut *const c_char,
) -> *const c_char {
    let _ = error_out; // lsp::query never returns an Err; errors come back as strings
    let operation = unsafe { CStr::from_ptr(operation) }.to_string_lossy();
    let file = unsafe { CStr::from_ptr(file) }.to_string_lossy();
    let line = (line as u32).max(1);
    let col = (col as u32).max(1);
    let config_json = LSP_CONFIG_JSON.with(|c| c.borrow().clone());
    let lsp_config: std::collections::HashMap<String, String> =
        serde_json::from_str(&config_json).unwrap_or_default();
    let result = crate::lsp::query(&operation, &file, line, col, &lsp_config);
    set_result(result)
}

#[cfg(test)]
mod tests {
    use super::{READ_CACHE_CAP, cached_read_file, read_cache};
    use std::fs;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Serialise tests in this module — they all share the process-wide
    /// `read_cache()`, so running in parallel would let one test's clear
    /// or insert race with another's assertions.
    fn test_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn clear_cache() {
        if let Ok(mut c) = read_cache().lock() {
            c.clear();
        }
    }

    fn cache_len() -> usize {
        read_cache().lock().map(|c| c.len()).unwrap_or(0)
    }

    fn cache_contains(path: &str) -> bool {
        read_cache()
            .lock()
            .map(|c| c.iter().any(|e| e.path == path))
            .unwrap_or(false)
    }

    #[test]
    fn cached_read_file_returns_content() {
        let _g = test_lock();
        clear_cache();
        let path = std::env::temp_dir().join("shio_native_cache_basic.txt");
        fs::write(&path, b"hello").unwrap();
        let got = cached_read_file(path.to_str().unwrap()).unwrap();
        assert_eq!(&*got, "hello");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn cached_read_file_inserts_entry_on_miss() {
        let _g = test_lock();
        clear_cache();
        let path = std::env::temp_dir().join("shio_native_cache_miss.txt");
        fs::write(&path, b"miss-then-hit").unwrap();
        let path_str = path.to_str().unwrap();
        assert!(!cache_contains(path_str));
        cached_read_file(path_str).unwrap();
        assert!(cache_contains(path_str), "cache should hold the entry");
        // A second call for the same path must not duplicate the entry.
        let before = cache_len();
        cached_read_file(path_str).unwrap();
        assert_eq!(cache_len(), before, "cache must not duplicate on hit");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn cached_read_file_invalidates_when_file_changes() {
        let _g = test_lock();
        clear_cache();
        let path = std::env::temp_dir().join("shio_native_cache_inval.txt");
        fs::write(&path, b"first").unwrap();
        let path_str = path.to_str().unwrap();
        let first = cached_read_file(path_str).unwrap();
        assert_eq!(&*first, "first");
        // Sleep briefly to ensure mtime resolution registers the change on
        // file systems with second-granularity mtimes (some tmpfs configs).
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(&path, b"updated-different-length").unwrap();
        let second = cached_read_file(path_str).unwrap();
        assert_eq!(&*second, "updated-different-length");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn cached_read_file_evicts_when_cap_exceeded() {
        let _g = test_lock();
        clear_cache();
        let mut paths: Vec<std::path::PathBuf> = Vec::new();
        for i in 0..(READ_CACHE_CAP + 2) {
            let p = std::env::temp_dir().join(format!("shio_native_cache_lru_{i}.txt"));
            fs::write(&p, format!("body {i}").as_bytes()).unwrap();
            cached_read_file(p.to_str().unwrap()).unwrap();
            paths.push(p);
        }
        assert!(cache_len() <= READ_CACHE_CAP, "cache exceeded cap");
        // Earliest-inserted path must have been evicted.
        assert!(
            !cache_contains(paths[0].to_str().unwrap()),
            "oldest entry should be evicted"
        );
        for p in &paths {
            let _ = fs::remove_file(p);
        }
    }

    #[test]
    fn cached_read_file_propagates_missing_file_error() {
        let _g = test_lock();
        clear_cache();
        let err = cached_read_file("/nonexistent/shio_cache_missing.txt").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
