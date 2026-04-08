# mRuby Scripting Layer — Reference Manual

Tool handlers are implemented as Ruby scripts evaluated by an embedded mRuby VM.
Rust retains all OS/network operations as native `Shio.*` methods; Ruby handles
argument extraction, logic, and result formatting.

User-extensible: drop a `.rb` file in `~/.config/shio/tools/` to add or override
tools without recompiling.

---

## File layout

```
vendor/mruby/                       ← git submodule (commit a309524d)
mruby_configs/
  shio.rb                           ← mRuby build config
  mcp_safe.gembox                   ← restricted gembox (stdlib + math + mruby-compiler)
build.rs                            ← mRuby build phase + glue.c compile
src/ruby/
  mod.rs
  ffi.rs                            ← extern "C": mrb_open/close, shio_mrb_eval, shio_register_native
  glue.c                            ← C shims hiding mrb_value from Rust
  native.rs                         ← Rust extern "C" implementations of Shio.* methods
  vm.rs                             ← ShioVm struct (eval, call_tool, tool_schemas)
  registry.rs                       ← (future: ToolRegistry wrapping ShioVm)
  prelude.rb                        ← Tool class, $shio_tools registry, define_tool DSL
tools/builtin/                      ← 22 built-in tool scripts
```

## Native methods exposed to Ruby (`Shio.*`)

| Method | Rust operation | Notes |
|---|---|---|
| `Shio.current_dir()` | `env::current_dir()` | |
| `Shio.read_file(path)` | `fs::read_to_string` | |
| `Shio.write_file(path, content)` | `fs::write` | creates parent dirs |
| `Shio.append_file(path, content)` | `OpenOptions::append` | |
| `Shio.read_dir(path)` | `fs::read_dir` | returns newline-joined names, dirs get `/` suffix |
| `Shio.create_dir_all(path)` | `fs::create_dir_all` | |
| `Shio.delete_file(path)` | `fs::remove_file` | |
| `Shio.rename(src, dst)` | `fs::rename` | |
| `Shio.run_shell(cmd)` | `Command::new("sh").arg("-c")` | returns stdout+stderr |
| `Shio.http_get(url, max_chars)` | reqwest blocking GET | SSRF check + HTML strip inside Rust |
| `Shio.lsp_query(op, file, line, col)` | `crate::lsp::query()` | |
| `Shio.grep(pattern, path, case_insensitive)` | regex walk | skips .git/target/node_modules/vendor |
| `Shio.glob(pattern, base)` | glob walk | returns newline-joined paths |

## Tool DSL

Every `.rb` tool file follows this pattern:

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

## Key design decisions

- **Security boundary:** gembox restricted to `stdlib` + `math` + `mruby-compiler` (no `File`, `IO`, `eval()`, `define_method`)
- **No mruby-json gem:** JSON parsed in Rust with `serde_json`, converted to mRuby hash literals via `value_to_ruby()`
- **SSRF protection in Rust:** `Shio.http_get` enforces scheme check, IP block list, HTML stripping — never in Ruby
- **VM concurrency:** single `ShioVm` instance behind `Arc<Mutex<>>`, shared across `spawn_blocking` clones
- **LSP config:** passed via thread-local JSON string set before each `call_tool`
- **`patch_file` fallback chain:** exact match → `trim_end()` tolerance → anchor match (first 2 + last 2 lines for blocks >= 4 lines)
- **Plan mode tools:** `enter_plan_mode`/`exit_plan_mode` are thin Ruby stubs; the TUI intercepts before dispatch
