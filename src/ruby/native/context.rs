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

pub(crate) struct NativeToolContext<'a> {
    pub(crate) lsp_config_json: &'a str,
    pub(crate) shell_allowlist: &'a [String],
    pub(crate) shell_denylist: &'a [String],
}

/// Called before entering the Ruby VM so native callbacks can use the current
/// executor's LSP configuration and shell policy.
pub(crate) fn set_tool_context(ctx: NativeToolContext<'_>) {
    LSP_CONFIG_JSON.with(|c| *c.borrow_mut() = ctx.lsp_config_json.to_string());
    SHELL_ALLOWLIST.with(|c| *c.borrow_mut() = ctx.shell_allowlist.to_vec());
    SHELL_DENYLIST.with(|c| *c.borrow_mut() = ctx.shell_denylist.to_vec());
}

pub(super) fn lsp_config_json() -> String {
    LSP_CONFIG_JSON.with(|c| c.borrow().clone())
}

pub(super) fn shell_policy() -> (Vec<String>, Vec<String>) {
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

#[cfg(test)]
mod tests {
    use super::{NativeToolContext, lsp_config_json, set_tool_context, shell_policy};

    #[test]
    fn set_tool_context_updates_lsp_config_and_shell_policy() {
        let allow = vec!["cargo".to_string()];
        let deny = vec!["curl".to_string()];

        set_tool_context(NativeToolContext {
            lsp_config_json: r#"{"rust":"rust-analyzer"}"#,
            shell_allowlist: &allow,
            shell_denylist: &deny,
        });

        assert_eq!(lsp_config_json(), r#"{"rust":"rust-analyzer"}"#);
        assert_eq!(shell_policy(), (allow, deny));
    }

    #[test]
    fn set_tool_context_replaces_previous_shell_policy() {
        set_tool_context(NativeToolContext {
            lsp_config_json: "{}",
            shell_allowlist: &["cargo".to_string()],
            shell_denylist: &["curl".to_string()],
        });

        set_tool_context(NativeToolContext {
            lsp_config_json: "[]",
            shell_allowlist: &[],
            shell_denylist: &[],
        });

        assert_eq!(lsp_config_json(), "[]");
        assert_eq!(shell_policy(), (Vec::new(), Vec::new()));
    }
}
