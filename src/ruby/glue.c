/* src/ruby/glue.c — C shim layer between Rust and mRuby.
 * SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Hides mrb_value manipulation from Rust.  All functions use only primitive
 * C types (char*, int, void*) in their signatures so Rust can call them via
 * extern "C" without knowing about mrb_value or mrb_state internals.
 */
#include <mruby.h>
#include <mruby/compile.h>
#include <mruby/error.h>
#include <mruby/string.h>
#include <mruby/class.h>

/* Evaluate Ruby source code.
 *
 * On success:  returns a pointer to a NUL-terminated inspect-string of the
 *              result value (owned by the mRuby GC — valid until next GC).
 *              *error_out is set to NULL.
 * On failure:  returns NULL.
 *              *error_out is set to a NUL-terminated error message string
 *              (also owned by the mRuby GC).
 *
 * The caller must copy either string before triggering another mRuby
 * allocation or GC cycle.
 */
const char* shio_mrb_eval(mrb_state* mrb, const char* code, const char** error_out) {
    *error_out = NULL;
    mrb_value result = mrb_load_string(mrb, code);
    if (mrb->exc) {
        mrb_value exc = mrb_obj_value(mrb->exc);
        mrb_value msg = mrb_inspect(mrb, exc);
        *error_out = mrb_string_value_cstr(mrb, &msg);
        mrb->exc = NULL;
        return NULL;
    }
    mrb_value inspected = mrb_inspect(mrb, result);
    return mrb_string_value_cstr(mrb, &inspected);
}

/* Register the Shio native module and all its methods.
 * Phase A stub — native methods are added here in Phase B/C as each tool
 * migrates from Rust to Ruby.
 */
void shio_register_native(mrb_state* mrb) {
    /* Phase A stub — filled in during Phase B/C */
    (void)mrb;
}
