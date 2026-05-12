// SPDX-License-Identifier: GPL-3.0-or-later
use super::context::{lsp_config_json, set_result};
use std::ffi::{CStr, c_char};

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
