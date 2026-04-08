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
    match std::fs::read_to_string(p) {
        Ok(content) => set_result(content),
        Err(e) => {
            unsafe { set_err(error_out, &e.to_string()) };
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
