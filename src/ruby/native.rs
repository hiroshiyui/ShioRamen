// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::missing_safety_doc)]

mod context;
mod web;

use context::{lsp_config_json, set_err, set_result, shell_policy};
pub(crate) use context::{set_lsp_config_json, set_shell_policy};
use std::ffi::{CStr, c_char};
use std::ptr;

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

// ── Shio.run_shell(cmd) ──────────────────────────────────────────────────────

const SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn run_shell_command(cmd: &str, timeout: std::time::Duration) -> String {
    let child = match spawn_shell(cmd) {
        Err(e) => return format!("Error running command: {e}"),
        Ok(c) => c,
    };

    let pid = child.id();

    // Run wait_with_output() in a background thread so stdout/stderr are
    // drained continuously — avoids deadlock when the child produces more
    // output than the OS pipe buffer (~64 KB on Linux).
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    let out = match rx.recv_timeout(timeout) {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return format!("Error reading command output: {e}"),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            kill_shell_tree(pid);
            let _ = rx.recv_timeout(std::time::Duration::from_secs(1));
            return format!("Error: command timed out after {}s", timeout.as_secs());
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            return "Error: command thread panicked".to_string();
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
        "(no output)".to_string()
    } else {
        result
    }
}

fn spawn_shell(cmd: &str) -> std::io::Result<std::process::Child> {
    let mut command = std::process::Command::new("sh");
    command
        .args(["-c", cmd])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc_setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    command.spawn()
}

#[cfg(unix)]
fn kill_shell_tree(pid: u32) {
    // kill(2) with a negative PID sends SIGKILL to the process group whose id
    // is `pid`. `spawn_shell` creates that group with setsid().
    libc_kill(-(pid as i32), 9);
}

#[cfg(not(unix))]
fn kill_shell_tree(_pid: u32) {}

#[cfg(unix)]
unsafe extern "C" {
    fn setsid() -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
}

#[cfg(unix)]
fn libc_setsid() -> i32 {
    unsafe { setsid() }
}

#[cfg(unix)]
fn libc_kill(pid: i32, sig: i32) -> i32 {
    unsafe { kill(pid, sig) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_run_shell(
    cmd: *const c_char,
    error_out: *mut *const c_char,
) -> *const c_char {
    let _ = error_out; // errors returned as strings, not C errors
    let cmd = unsafe { CStr::from_ptr(cmd) }.to_string_lossy();

    // Check shell allowlist/denylist before execution.
    let (allow, deny) = shell_policy();
    if let Err(msg) = crate::tools::check_shell_policy(&cmd, &allow, &deny) {
        return set_result(format!("Error: {msg}"));
    }

    set_result(run_shell_command(&cmd, SHELL_TIMEOUT))
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
    let config_json = lsp_config_json();
    let lsp_config: std::collections::HashMap<String, String> =
        serde_json::from_str(&config_json).unwrap_or_default();
    let result = crate::lsp::query(&operation, &file, line, col, &lsp_config);
    set_result(result)
}

#[cfg(test)]
mod tests {
    use super::{READ_CACHE_CAP, cached_read_file, read_cache, run_shell_command};
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

    #[cfg(unix)]
    #[test]
    fn run_shell_timeout_kills_descendant_processes() {
        let path = std::env::temp_dir().join(format!(
            "shio_timeout_descendant_{}.txt",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let cmd = format!("sh -c 'sleep 2; touch {}' & wait", path.to_string_lossy());
        let out = run_shell_command(&cmd, std::time::Duration::from_millis(100));
        assert!(out.contains("timed out"), "{out}");
        std::thread::sleep(std::time::Duration::from_secs(3));
        assert!(
            !path.exists(),
            "timeout should kill background descendants before they can touch the file"
        );
    }
}
