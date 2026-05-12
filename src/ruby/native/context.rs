// SPDX-License-Identifier: GPL-3.0-or-later
use std::ffi::{CString, c_char};

thread_local! {
    static LAST_ERR: std::cell::RefCell<Option<CString>> =
        const { std::cell::RefCell::new(None) };
    static LAST_RESULT: std::cell::RefCell<Option<CString>> =
        const { std::cell::RefCell::new(None) };
    /// Serialised LSP server config (`{"lang": "cmd", ...}`) set by the Rust
    /// executor just before each `call_tool` invocation. Read by
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

pub(crate) fn lsp_config_json() -> String {
    LSP_CONFIG_JSON.with(|c| c.borrow().clone())
}

pub(crate) fn set_shell_policy(allowlist: &[String], denylist: &[String]) {
    SHELL_ALLOWLIST.with(|c| *c.borrow_mut() = allowlist.to_vec());
    SHELL_DENYLIST.with(|c| *c.borrow_mut() = denylist.to_vec());
}

pub(crate) fn shell_policy() -> (Vec<String>, Vec<String>) {
    let allow = SHELL_ALLOWLIST.with(|c| c.borrow().clone());
    let deny = SHELL_DENYLIST.with(|c| c.borrow().clone());
    (allow, deny)
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

pub(super) fn set_result(s: String) -> *const c_char {
    let cstr = CString::new(s).unwrap_or_else(|_| c"<result contains nul>".to_owned());
    LAST_RESULT.with(|cell| {
        let ptr = cstr.as_ptr();
        *cell.borrow_mut() = Some(cstr);
        ptr
    })
}
