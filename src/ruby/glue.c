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
 * On success:  If the result is a Ruby String, returns a pointer to its raw
 *              NUL-terminated bytes (owned by the mRuby GC).
 *              For all other result types, returns the inspect-string.
 *              *error_out is set to NULL.
 * On failure:  returns NULL.
 *              *error_out is set to a NUL-terminated error message string
 *              (also owned by the mRuby GC).
 *
 * Returning String results raw (not inspect-wrapped) means:
 *   - Tool handler results arrive in Rust without surrounding quotes.
 *   - shio_tool_schemas_json output can be split on "\n" directly.
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
    if (mrb_string_p(result)) {
        return mrb_string_value_cstr(mrb, &result);
    }
    mrb_value inspected = mrb_inspect(mrb, result);
    return mrb_string_value_cstr(mrb, &inspected);
}

/* ── Rust native implementations ─────────────────────────────────────────── */

extern const char* shio_native_current_dir(const char** error_out);
extern void        shio_native_create_dir_all(const char* path, const char** error_out);
extern const char* shio_native_read_file(const char* path, const char** error_out);
extern const char* shio_native_read_dir(const char* path, const char** error_out);
extern void        shio_native_delete_file(const char* path, const char** error_out);
extern void        shio_native_rename(const char* src, const char* dst, const char** error_out);
extern void        shio_native_write_file(const char* path, const char* content, const char** error_out);
extern void        shio_native_append_file(const char* path, const char* content, const char** error_out);
extern const char* shio_native_glob(const char* pattern, const char** error_out);
extern const char* shio_native_grep(const char* pattern, const char* path, int case_insensitive, const char** error_out);
extern const char* shio_native_fetch_url(const char* url, int max_chars, const char** error_out);
extern const char* shio_native_web_search(const char* query, int max_results, const char** error_out);
extern const char* shio_native_lsp_query(const char* op, const char* file, int line, int col, const char** error_out);

/* ── mRuby shim: Shio.current_dir ─────────────────────────────────────────── */

static mrb_value shio_rb_current_dir(mrb_state* mrb, mrb_value self) {
    (void)self;
    const char* err = NULL;
    const char* result = shio_native_current_dir(&err);
    if (err) mrb_raise(mrb, E_RUNTIME_ERROR, err);
    return mrb_str_new_cstr(mrb, result);
}

/* ── mRuby shim: Shio.create_dir_all(path) ───────────────────────────────── */

static mrb_value shio_rb_create_dir_all(mrb_state* mrb, mrb_value self) {
    (void)self;
    const char* path_arg;
    mrb_get_args(mrb, "z", &path_arg);
    const char* err = NULL;
    shio_native_create_dir_all(path_arg, &err);
    if (err) mrb_raise(mrb, E_RUNTIME_ERROR, err);
    return mrb_nil_value();
}

/* ── mRuby shim: Shio.read_file(path) ───────────────────────────────────── */

static mrb_value shio_rb_read_file(mrb_state* mrb, mrb_value self) {
    (void)self;
    const char* path_arg;
    mrb_get_args(mrb, "z", &path_arg);
    const char* err = NULL;
    const char* result = shio_native_read_file(path_arg, &err);
    if (err) mrb_raise(mrb, E_RUNTIME_ERROR, err);
    return mrb_str_new_cstr(mrb, result);
}

/* ── mRuby shim: Shio.read_dir(path) ────────────────────────────────────── */

static mrb_value shio_rb_read_dir(mrb_state* mrb, mrb_value self) {
    (void)self;
    const char* path_arg;
    mrb_get_args(mrb, "z", &path_arg);
    const char* err = NULL;
    const char* result = shio_native_read_dir(path_arg, &err);
    if (err) mrb_raise(mrb, E_RUNTIME_ERROR, err);
    return mrb_str_new_cstr(mrb, result);
}

/* ── mRuby shim: Shio.delete_file(path) ─────────────────────────────────── */

static mrb_value shio_rb_delete_file(mrb_state* mrb, mrb_value self) {
    (void)self;
    const char* path_arg;
    mrb_get_args(mrb, "z", &path_arg);
    const char* err = NULL;
    shio_native_delete_file(path_arg, &err);
    if (err) mrb_raise(mrb, E_RUNTIME_ERROR, err);
    return mrb_nil_value();
}

/* ── mRuby shim: Shio.rename(src, dst) ──────────────────────────────────── */

static mrb_value shio_rb_rename(mrb_state* mrb, mrb_value self) {
    (void)self;
    const char* src_arg;
    const char* dst_arg;
    mrb_get_args(mrb, "zz", &src_arg, &dst_arg);
    const char* err = NULL;
    shio_native_rename(src_arg, dst_arg, &err);
    if (err) mrb_raise(mrb, E_RUNTIME_ERROR, err);
    return mrb_nil_value();
}

/* ── mRuby shim: Shio.write_file(path, content) ──────────────────────────── */

static mrb_value shio_rb_write_file(mrb_state* mrb, mrb_value self) {
    (void)self;
    const char* path_arg;
    const char* content_arg;
    mrb_get_args(mrb, "zz", &path_arg, &content_arg);
    const char* err = NULL;
    shio_native_write_file(path_arg, content_arg, &err);
    if (err) mrb_raise(mrb, E_RUNTIME_ERROR, err);
    return mrb_nil_value();
}

/* ── mRuby shim: Shio.append_file(path, content) ─────────────────────────── */

static mrb_value shio_rb_append_file(mrb_state* mrb, mrb_value self) {
    (void)self;
    const char* path_arg;
    const char* content_arg;
    mrb_get_args(mrb, "zz", &path_arg, &content_arg);
    const char* err = NULL;
    shio_native_append_file(path_arg, content_arg, &err);
    if (err) mrb_raise(mrb, E_RUNTIME_ERROR, err);
    return mrb_nil_value();
}

/* ── mRuby shim: Shio.glob(pattern) ──────────────────────────────────────── */

static mrb_value shio_rb_glob(mrb_state* mrb, mrb_value self) {
    (void)self;
    const char* pattern_arg;
    mrb_get_args(mrb, "z", &pattern_arg);
    const char* err = NULL;
    const char* result = shio_native_glob(pattern_arg, &err);
    if (err) mrb_raise(mrb, E_RUNTIME_ERROR, err);
    return mrb_str_new_cstr(mrb, result);
}

/* ── mRuby shim: Shio.grep(pattern, path, case_insensitive) ──────────────── */

static mrb_value shio_rb_grep(mrb_state* mrb, mrb_value self) {
    (void)self;
    const char* pattern_arg;
    const char* path_arg;
    mrb_bool ci;
    mrb_get_args(mrb, "zzb", &pattern_arg, &path_arg, &ci);
    const char* err = NULL;
    const char* result = shio_native_grep(pattern_arg, path_arg, (int)ci, &err);
    if (err) mrb_raise(mrb, E_RUNTIME_ERROR, err);
    return mrb_str_new_cstr(mrb, result);
}

/* ── mRuby shim: Shio.fetch_url(url, max_chars) ──────────────────────────── */

static mrb_value shio_rb_fetch_url(mrb_state* mrb, mrb_value self) {
    (void)self;
    const char* url_arg;
    mrb_int max_chars;
    mrb_get_args(mrb, "zi", &url_arg, &max_chars);
    const char* err = NULL;
    const char* result = shio_native_fetch_url(url_arg, (int)max_chars, &err);
    if (err) mrb_raise(mrb, E_RUNTIME_ERROR, err);
    return mrb_str_new_cstr(mrb, result);
}

/* ── mRuby shim: Shio.web_search(query, max_results) ─────────────────────── */

static mrb_value shio_rb_web_search(mrb_state* mrb, mrb_value self) {
    (void)self;
    const char* query_arg;
    mrb_int max_results;
    mrb_get_args(mrb, "zi", &query_arg, &max_results);
    const char* err = NULL;
    const char* result = shio_native_web_search(query_arg, (int)max_results, &err);
    if (err) mrb_raise(mrb, E_RUNTIME_ERROR, err);
    return mrb_str_new_cstr(mrb, result);
}

/* ── mRuby shim: Shio.lsp_query(op, file, line, col) ─────────────────────── */

static mrb_value shio_rb_lsp_query(mrb_state* mrb, mrb_value self) {
    (void)self;
    const char* op_arg;
    const char* file_arg;
    mrb_int line_arg;
    mrb_int col_arg;
    mrb_get_args(mrb, "zzii", &op_arg, &file_arg, &line_arg, &col_arg);
    const char* err = NULL;
    const char* result = shio_native_lsp_query(op_arg, file_arg, (int)line_arg, (int)col_arg, &err);
    if (err) mrb_raise(mrb, E_RUNTIME_ERROR, err);
    return mrb_str_new_cstr(mrb, result);
}

/* Register the Shio native module and all its methods. */
void shio_register_native(mrb_state* mrb) {
    struct RClass* shio = mrb_define_module(mrb, "Shio");
    mrb_define_module_function(mrb, shio, "current_dir",    shio_rb_current_dir,    MRB_ARGS_NONE());
    mrb_define_module_function(mrb, shio, "create_dir_all", shio_rb_create_dir_all, MRB_ARGS_REQ(1));
    mrb_define_module_function(mrb, shio, "read_file",      shio_rb_read_file,      MRB_ARGS_REQ(1));
    mrb_define_module_function(mrb, shio, "read_dir",       shio_rb_read_dir,       MRB_ARGS_REQ(1));
    mrb_define_module_function(mrb, shio, "delete_file",    shio_rb_delete_file,    MRB_ARGS_REQ(1));
    mrb_define_module_function(mrb, shio, "rename",         shio_rb_rename,         MRB_ARGS_REQ(2));
    mrb_define_module_function(mrb, shio, "write_file",     shio_rb_write_file,     MRB_ARGS_REQ(2));
    mrb_define_module_function(mrb, shio, "append_file",    shio_rb_append_file,    MRB_ARGS_REQ(2));
    mrb_define_module_function(mrb, shio, "glob",           shio_rb_glob,           MRB_ARGS_REQ(1));
    mrb_define_module_function(mrb, shio, "grep",           shio_rb_grep,           MRB_ARGS_REQ(3));
    mrb_define_module_function(mrb, shio, "fetch_url",      shio_rb_fetch_url,      MRB_ARGS_REQ(2));
    mrb_define_module_function(mrb, shio, "web_search",     shio_rb_web_search,     MRB_ARGS_REQ(2));
    mrb_define_module_function(mrb, shio, "lsp_query",      shio_rb_lsp_query,      MRB_ARGS_REQ(4));
}
