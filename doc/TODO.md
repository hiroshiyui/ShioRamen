# TODO: mRuby Scripting Layer for Tool Handlers

**Goal:** Replace the monolithic `src/tools.rs` Rust dispatch with an mRuby scripting layer.
Each tool becomes a `.rb` file using a `define_tool` DSL. Rust retains all OS/network operations
as native `Shio.*` methods; Ruby handles argument extraction, logic, and result formatting.
User-extensible: drop a `.rb` file in `~/.config/shio/tools/` to add or override tools without recompiling.

**Reference implementation:** `/home/yhh/MyProjects/rrcad` — copy patterns freely from there.

**Invariants (never break these):**
- `cargo test` must pass with zero failures before any commit
- `cargo clippy -- -D warnings` zero warnings
- `cargo fmt` default style
- Every new `.rs` file starts with `// SPDX-License-Identifier: GPL-3.0-or-later`
- No `.unwrap()` in production paths — use `anyhow::Result` + `?`

---

## Phase A — Infrastructure ✓ DONE

**Files created:**
- `build.rs` — invokes `rake MRUBY_CONFIG=shio`, links `libmruby.a`, compiles `src/ruby/glue.c` via `cc`
- `mruby_configs/shio.rb` + `mruby_configs/mcp_safe.gembox`
- `src/ruby/ffi.rs` — `extern "C"` bindings for `mrb_open/close`, `shio_mrb_eval`, `shio_register_native`
- `src/ruby/glue.c` — `shio_mrb_eval` impl; `shio_register_native` is a Phase A stub
- `src/ruby/native.rs` — thread-local `LAST_ERR`/`set_err` infrastructure; no methods yet
- `src/ruby/prelude.rb` — `Tool` class, `$shio_tools` registry, `define_tool` DSL, `shio_tool_schemas`, `shio_hash_to_json`
- `src/ruby/vm.rs` — `ShioVm` wrapping `*mut MrbState`; `eval`, `call_tool`, `tool_schemas`, `load_builtin_tools` (stub), `load_user_tools` (`~/.config/shio/tools/*.rb`)
- `src/ruby/registry.rs` — empty stub
- `src/ruby/mod.rs` + `mod ruby;` in `src/main.rs`

**Key decisions:**
- mRuby pinned to commit `a309524d0` (same as rrcad — C API surface is identical, glue.c copied verbatim)
- gembox restricted to `stdlib` + `math` + `mruby-compiler` (security boundary: no `File`, `IO`, `eval()`, `define_method`)
- `#![allow(dead_code)]` on Phase A stubs — removed in Phase B when items become used
- `unsafe impl Send for ShioVm` — safe because it will always be held behind `Arc<Mutex<>>`
- `dirs = "5"` added to `[dependencies]`; `cc = "1"` added to `[build-dependencies]`

**Verified:** `cargo build` ✓ · **320 tests pass** ✓ · `clippy -D warnings` ✓ · `cargo fmt` ✓

---

## Phase B — Parallel dispatch + ToolDef plumbing

Still no tools migrated. This phase wires the VM into `ToolExecutor` so the Ruby path
can be tested alongside the existing Rust dispatch before any migration begins.

### B1 — Fix `ToolDef` lifetime

Currently in `src/client.rs` (around line 62–80):
```rust
pub struct FunctionSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: serde_json::Value,
}
```

Change both fields to owned `String`:
```rust
pub struct FunctionSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}
```

This will break every `FunctionSpec { name: "read_file", ... }` literal in `tools.rs`
(25 of them). Fix by adding `.into()` or `"...".to_string()` on each. The compiler
will flag every site. **Do not skip any** — fix them all before moving on.

Also update `ChatSession` in `src/chat.rs`: `tools: all_tools()` stays as-is for now.

### B2 — Add `ShioVm` to `ToolExecutor`

In `src/tools.rs`, add a `vm` field to `ToolExecutor`:

```rust
use std::sync::{Arc, Mutex};
use crate::ruby::vm::ShioVm;

pub struct ToolExecutor {
    pub confirm_writes: bool,
    pub confirm_shell: bool,
    pub lsp: std::collections::HashMap<String, String>,
    pub max_tool_result_chars: usize,
    pub(crate) vm: Arc<Mutex<ShioVm>>,  // NEW
}
```

`ToolExecutor` is currently `Clone` (used in `tui.rs` — check `tui.rs` for `executor.clone()`).
`Arc<Mutex<ShioVm>>` is `Clone`, so this works without changes to call sites.

Update `ToolExecutor::default()` and any test helper constructors to create a `ShioVm::new()`.
`ShioVm::new()` returns `Result<ShioVm>` so use `.expect("ShioVm init failed")` in `default()`.

### B3 — Add parallel dispatch behind `SHIO_USE_RUBY`

In `ToolExecutor::dispatch()` (`src/tools.rs` line ~582), prepend:

```rust
fn dispatch(&self, name: &str, args: &Value) -> String {
    if std::env::var("SHIO_USE_RUBY").is_ok() {
        let args_json = args.to_string();
        let result = self.vm.lock().unwrap().call_tool(name, &args_json);
        // If the tool is registered in Ruby, it returns the result directly.
        // If not ("unknown tool: X"), fall through to the Rust match arm.
        if !result.starts_with("Error: unknown tool:") {
            return result;
        }
    }
    match name {
        "read_file" => self.read_file(args),
        // ... (existing arms unchanged)
    }
}
```

### B4 — Implement `parse_tool_schemas` in `vm.rs`

The `shio_tool_schemas` Ruby function returns an inspect-string of a nested Ruby array.
Rather than parsing Ruby inspect format, change the approach: have `shio_hash_to_json`
in `prelude.rb` serialize each schema and join with a delimiter:

In `prelude.rb`, add:
```ruby
def shio_tool_schemas_json
  $shio_tools.map do |name, tool|
    "{\"name\":#{shio_hash_to_json(name)}," \
    "\"description\":#{shio_hash_to_json(tool.description)}," \
    "\"parameters\":#{shio_hash_to_json(tool.parameters)}}"
  end.join("\n")
end
```

In `vm.rs`, `tool_schemas()` becomes:
```rust
pub fn tool_schemas(&mut self) -> Result<Vec<(String, String, serde_json::Value)>> {
    let raw = self.eval("shio_tool_schemas_json")
        .map_err(|e| anyhow!("shio_tool_schemas_json failed: {e}"))?;
    // raw is one JSON object per line; empty string if no tools registered
    if raw.trim_matches('"').is_empty() { return Ok(vec![]); }
    raw.trim_matches('"')
       .lines()
       .map(|line| {
           let v: serde_json::Value = serde_json::from_str(line)?;
           let name = v["name"].as_str().unwrap_or("").to_string();
           let desc = v["description"].as_str().unwrap_or("").to_string();
           let params = v["parameters"].clone();
           Ok((name, desc, params))
       })
       .collect()
}
```

> **Note:** `shio_tool_schemas_json` returns a Ruby string (inspect-wrapped in quotes),
> so strip the outer `"` before processing. The eval returns the `inspect` of the result,
> which for a String is the string with outer quotes. Use `.trim_matches('"')` carefully —
> or change `shio_mrb_eval` to return the raw string value when the result is a String,
> rather than its `inspect`. Consider adding a `eval_raw_string` variant.

### B5 — Add `tool_defs()` method to `ToolExecutor`

```rust
impl ToolExecutor {
    /// Returns tool definitions sourced from the Ruby VM (migrated tools)
    /// merged with the static Rust definitions (not-yet-migrated tools).
    /// During Phase C this gradually shifts from all-Rust to all-Ruby.
    pub fn tool_defs(&self) -> Vec<ToolDef> {
        // Phase B: still return all_tools() — Ruby tools not yet registered
        all_tools()
    }
}
```

This method will progressively take over from `all_tools()` in Phase D.

### B6 — Verify Phase B

```sh
cargo test        # still passes
SHIO_USE_RUBY=1 cargo run -- --help   # binary starts without panic
```

---

## Phase C — Migrate tools one batch at a time

**Per-tool migration checklist:**
1. Create `tools/builtin/<name>.rb` with the `define_tool` block
2. Add `include_str!` entry in `ShioVm::load_builtin_tools()`
3. Add the corresponding `extern "C"` Rust function in `src/ruby/native.rs`
4. Add the C shim in `src/ruby/glue.c` (`shio_register_native` + method shim)
5. Remove the Rust match arm from `dispatch()` and the handler method
6. Update tests: replace `executor.read_file(&args)` style with `executor.execute_quiet(&call)` style
7. Run `cargo test` — must pass before touching next tool

### C1 — Batch 1: `get_working_directory`, `create_directory`

These are the simplest — one native call, no args complexity.

**Native Rust methods to add in `native.rs`:**
```rust
#[unsafe(no_mangle)]
pub unsafe extern "C" fn shio_native_current_dir(
    error_out: *mut *const c_char,
) -> *const c_char {
    match std::env::current_dir().map(|p| p.display().to_string()) {
        Ok(s) => {
            let cs = CString::new(s).unwrap();
            let ptr = cs.as_ptr();
            std::mem::forget(cs); // TODO: use thread-local string slot to avoid leak
            ptr
        }
        Err(e) => { set_err(error_out, &e.to_string()); ptr::null() }
    }
}
```

> **Important:** The return string must live long enough for C to copy it.
> Follow rrcad's pattern: store the `CString` in a thread-local `RefCell<Option<CString>>`
> (one per native method, or a shared "last result" slot). See how rrcad's `native.rs`
> handles return strings vs error strings.

**C shim in `glue.c`:**
```c
extern const char* shio_native_current_dir(const char** error_out);

static mrb_value shio_rb_current_dir(mrb_state* mrb, mrb_value self) {
    const char* err = NULL;
    const char* result = shio_native_current_dir(&err);
    if (err) mrb_raise(mrb, E_RUNTIME_ERROR, err);
    return mrb_str_new_cstr(mrb, result);
}
```

Register in `shio_register_native`:
```c
void shio_register_native(mrb_state* mrb) {
    struct RClass* shio = mrb_define_module(mrb, "Shio");
    mrb_define_module_function(mrb, shio, "current_dir", shio_rb_current_dir, MRB_ARGS_NONE());
    // (add more here as each batch migrates)
}
```

**Ruby tool script `tools/builtin/get_working_directory.rb`:**
```ruby
define_tool(
  "get_working_directory",
  "Return the current working directory.",
  { "type" => "object", "properties" => {} }
) do |_args|
  Shio.current_dir
end
```

**In `ShioVm::load_builtin_tools()`:**
```rust
fn load_builtin_tools(&mut self) -> Result<()> {
    for (name, code) in &[
        ("get_working_directory", include_str!("../../tools/builtin/get_working_directory.rb")),
    ] {
        self.eval(code).map_err(|e| anyhow!("builtin tool {name} failed: {e}"))?;
    }
    Ok(())
}
```

### C2 — Batch 2: `read_file`, `list_directory`, `delete_file`, `move_file`

**Native methods needed:** `Shio.read_file(path)`, `Shio.read_dir(path)`,
`Shio.delete_file(path)`, `Shio.rename(src, dst)`.

These are straightforward `fs::` calls. For `read_dir`, return a newline-separated
list of filenames (Ruby splits on `\n`).

**Gotcha — `list_directory`:** The current Rust implementation annotates directories
with `/` suffix and returns a formatted string. Replicate that formatting in the Ruby
script so output is identical. Test with the existing tests before removing the Rust handler.

### C3 — Batch 3: `write_file`, `append_file`, `insert_after_line`

These need the confirmation flag. The confirm flag state (`confirm_writes`) must be
communicated from Rust to the Ruby VM before tool execution.

**Pattern:** Add a `Shio.confirm_writes?` and `Shio.confirm_shell?` native method
that reads from thread-locals set by `ToolExecutor::execute_quiet()`:

```rust
// In native.rs
thread_local! {
    pub(super) static CONFIRM_WRITES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(super) static CONFIRM_SHELL:  std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
```

In `execute_quiet()` (before calling `vm.call_tool`):
```rust
crate::ruby::native::CONFIRM_WRITES.with(|c| c.set(self.confirm_writes));
crate::ruby::native::CONFIRM_SHELL.with(|c| c.set(self.confirm_shell));
```

**Important:** The actual `y/N` confirmation prompt is handled in `tui.rs` by
`needs_confirm()` (line ~1856), not by the tool handler. The `confirm_writes` flag
only controls whether the TUI asks for confirmation — `execute_quiet()` is called
*after* confirmation is granted. So the Ruby tools do **not** need to call any
`Shio.confirm()` method. The flag is passed through just in case a tool needs to
know whether it was run in "trusted" mode.

### C4 — Batch 4: `read_file_range`, `read_many_files`

`read_file_range` reads N lines from a file. The current Rust implementation in
`src/tools.rs` formats output with a header line and `│` prefix on each line.
Replicate this format in Ruby exactly, because the TUI and tests expect it.
Read the current Rust output format from `src/tools.rs` before writing the Ruby version.

`read_many_files` just loops and calls `Shio.read_file` — pure Ruby, no new native methods needed.

### C5 — Batch 5: `search_files`, `grep_files`

**New native methods:** `Shio.glob(pattern, base_dir)` → newline-joined paths,
`Shio.grep(pattern, path, case_insensitive)` → formatted match output.

The current `grep_files` Rust implementation (`src/tools.rs` around line 808) skips
`.git`, `target`, `node_modules`, `vendor` directories. Replicate this skip list
in the Rust native `Shio.grep` implementation — do not move it to Ruby, since
directory traversal logic is easier to keep in Rust.

### C6 — Batch 6: `save_memory`, `write_todos`

Pure Ruby logic + `Shio.read_file` + `Shio.write_file`. No new native methods.

**Gotcha — `save_memory`:** The Rust implementation does an atomic write via a temp
file + rename (`src/tools.rs` around line 1313). Do the same in Ruby:
```ruby
tmp = path + ".tmp"
Shio.write_file(tmp, new_content)
Shio.rename(tmp, path)
```
`Shio.rename` is already added in Batch 2.

### C7 — Batch 7: `fetch_url`, `web_search`

**All SSRF security logic stays in Rust.** The Ruby tool calls `Shio.http_get(url, max_chars)`.
`Shio.http_get` in Rust does: scheme check (https/http only), IP block list check
(127.0.0.0/8, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16, ::1, fc00::/7, fe80::/10),
HTTP GET with reqwest blocking, HTML stripping (script/style removal, tag stripping, entity decode),
size cap at `max_chars`.

`web_search` uses `Shio.http_get` internally — the Ruby tool calls it with the
DuckDuckGo lite URL and parses the result. The URL construction and result parsing
can move to Ruby.

**Gotcha — reqwest blocking in Rust native:** `Shio.http_get` must use `reqwest`'s
**blocking** client (already a dependency: `reqwest = { features = ["blocking"] }` in `Cargo.toml`).
Do not use async — mRuby calls are synchronous. Using the async client here would
require `block_on`, which panics if called from within an async context (the TUI runs on Tokio).
Use `reqwest::blocking::Client` with a timeout.

### C8 — Batch 8: `patch_file`

Most complex tool. The Rust implementation has three fallback strategies
(`src/tools.rs` lines 933–1080):
1. Exact string match (must appear exactly once)
2. Line-by-line match with `trim_end()` tolerance
3. Anchor match for large blocks (≥4 lines: first 2 + last 2 must match exactly)

Keep `strip_line_number_prefix` in Rust as `Shio.strip_line_prefix(s)` — it handles
multi-byte box-drawing characters (`│`) and the logic is fiddly. Move the three-level
fallback orchestration to Ruby, calling `Shio.read_file`, `Shio.strip_line_prefix`,
and `Shio.write_file`.

**Gotcha:** The existing tests for `patch_file` are extensive. Run them at each
intermediate step to catch regressions before removing the Rust handler.

### C9 — Batch 9: `lsp`, `enter_plan_mode`, `exit_plan_mode`

`lsp` delegates to `src/lsp.rs`. Add `Shio.lsp_query(operation, file, line, col)` native
method that calls `crate::lsp::query(...)`.

`enter_plan_mode` and `exit_plan_mode` are special: they do not execute in the tool
handler — the TUI agentic loop in `tui.rs` intercepts them before calling `execute_quiet`
(lines 1756–1776 of `tui.rs`). So the Ruby tools for these are thin stubs that just
return a confirmation string; the real state change happens in `tui.rs`.

Keep `PLAN_MODE_ALLOWED` constant in `tui.rs` exactly as-is — it filters by tool name,
which still works regardless of where the handler lives.

---

## Phase D — Cleanup

### D1 — Replace `all_tools()` with `executor.tool_defs()`

`all_tools()` is called in:
- `src/chat.rs` line 92: `tools: all_tools()`
- Potentially `tui.rs` (search for `all_tools`)

Change `ChatSession` to call `executor.tool_defs()` when an executor is present,
or keep `tools` computed lazily from the executor. Also update `ToolExecutor::tool_defs()`
to actually query the VM via `ShioVm::tool_schemas()` instead of calling `all_tools()`.

### D2 — Remove `SHIO_USE_RUBY` flag

Remove the `if std::env::var("SHIO_USE_RUBY").is_ok()` branch from `dispatch()`.
At this point `dispatch()` itself should be nearly empty (all arms removed in Phase C);
delete the method entirely and have `execute_quiet` call `vm.call_tool` directly.

### D3 — Remove dead Rust code

- Delete `all_tools()` free function
- Delete every removed handler method from `ToolExecutor`
- Delete `dispatch()` if empty
- Delete `require_str!` macro if unused
- Delete `ensure_parent_dirs` if unused

### D4 — Final validation

```sh
cargo test                        # all tests pass
cargo clippy -- -D warnings       # zero warnings
cargo fmt --check                 # no formatting changes needed
SHIO_USE_RUBY=1 cargo run -- ask "what tools do you have?"   # sanity check
```

---

## Architecture Reference

### New file layout

```
vendor/mruby/                       ← git submodule (commit a309524d)
mruby_configs/
  shio.rb                           ← mRuby build config
  mcp_safe.gembox                   ← restricted gembox (copy from rrcad)
build.rs                            ← NEW: mRuby build phase + glue.c compile
src/ruby/
  mod.rs
  ffi.rs                            ← extern "C": mrb_open/close, shio_mrb_eval, shio_register_native
  glue.c                            ← C shims hiding mrb_value from Rust
  native.rs                         ← Rust extern "C" implementations of Shio.* methods
  vm.rs                             ← ShioVm struct
  registry.rs                       ← (future: ToolRegistry wrapping ShioVm)
  prelude.rb                        ← Tool DSL (embedded at compile time)
tools/builtin/
  get_working_directory.rb
  read_file.rb
  write_file.rb
  ... (25 total, added one batch at a time)
```

### Native methods exposed to Ruby (`Shio.*`)

| Method | Rust operation | Notes |
|---|---|---|
| `Shio.read_file(path)` | `fs::read_to_string` | |
| `Shio.write_file(path, content)` | `fs::write` | creates parent dirs |
| `Shio.append_file(path, content)` | `OpenOptions::append` | |
| `Shio.read_dir(path)` | `fs::read_dir` | returns newline-joined names, dirs get `/` suffix |
| `Shio.create_dir_all(path)` | `fs::create_dir_all` | |
| `Shio.delete_file(path)` | `fs::remove_file` | |
| `Shio.rename(src, dst)` | `fs::rename` | |
| `Shio.run_shell(cmd)` | `Command::new("sh").arg("-c")` | returns stdout+stderr |
| `Shio.http_get(url, max_chars)` | reqwest blocking GET | SSRF check + HTML strip inside Rust |
| `Shio.current_dir()` | `env::current_dir()` | |
| `Shio.lsp_query(op, file, line, col)` | `crate::lsp::query()` | |
| `Shio.strip_line_prefix(s)` | `strip_line_number_prefix()` | handles `│` box-drawing chars |
| `Shio.grep(pattern, path, case_insensitive)` | regex walk | skips .git/target/node_modules/vendor |
| `Shio.glob(pattern, base)` | glob walk | returns newline-joined paths |

### Tool DSL (every `.rb` file follows this pattern)

```ruby
define_tool(
  "tool_name",
  "Description sent to the model.",
  {
    "type" => "object",
    "properties" => {
      "arg1" => { "type" => "string", "description" => "..." }
    },
    "required" => ["arg1"]
  }
) do |args|
  val = args["arg1"] or raise ArgumentError, "missing 'arg1'"
  Shio.some_native_method(val)
end
```

The block receives a Hash (parsed from the JSON args string).
It must return a String (tool result). Raise on error — the VM catches it and
returns `"Error: <message>"`.
