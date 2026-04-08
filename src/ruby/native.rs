// SPDX-License-Identifier: GPL-3.0-or-later
// Thread-local error slot infrastructure for propagating Rust errors through
// C FFI back to mRuby exceptions.
//
// Actual extern "C" functions for each Shio.* native method are added in Phase C.
#![allow(clippy::missing_safety_doc, dead_code)]

use std::ffi::{CString, c_char};

thread_local! {
    static LAST_ERR: std::cell::RefCell<Option<CString>> =
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
