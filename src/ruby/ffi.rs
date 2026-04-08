// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(dead_code)]
use std::ffi::c_char;

#[repr(C)]
pub struct MrbState {
    _private: [u8; 0],
}

unsafe extern "C" {
    pub fn mrb_open() -> *mut MrbState;
    pub fn mrb_close(mrb: *mut MrbState);

    /// Evaluate Ruby source; returns inspect-string of result, or NULL on exception.
    /// See glue.c for full contract.
    pub fn shio_mrb_eval(
        mrb: *mut MrbState,
        code: *const c_char,
        error_out: *mut *const c_char,
    ) -> *const c_char;

    /// Register the `Shio` native module and all its methods on the VM.
    /// Must be called after the prelude is evaluated.
    pub fn shio_register_native(mrb: *mut MrbState);
}
