# ShioRamen — Reference Manual

A local AI coding assistant powered by llama.cpp, running entirely offline.

---

## Project layout

```
shio.toml                           ← user config (all keys optional)
AGENTS.md                           ← project-specific AI instructions
build.rs                            ← mRuby build phase + C glue compile
envsetup.sh                         ← one-shot llama.cpp build + install
src/
  main.rs                           ← CLI (clap), command dispatch, trust prompt
  config.rs                         ← ShioConfig / Config structs, defaults
  server.rs                         ← ServerProcess: spawn or connect to llama-server
  client.rs                         ← LlamaClient: HTTP to /v1/chat/completions
  chat.rs                           ← ChatSession, DEFAULT_SYSTEM_PROMPT
  tui.rs                            ← full-screen TUI (ratatui + crossterm)
  tools.rs                          ← ToolExecutor: dispatch, HTTP client, SSRF guard
  lsp.rs                            ← LSP client: hover, definition, references, diagnostics
  context.rs                        ← /include file collection + size-capping
  agents.rs                         ← AGENTS.md discovery and system prompt augmentation
  ask.rs                            ← `shio ask` one-shot command
  edit.rs                           ← `shio edit` diff-and-confirm command
  doctor.rs                         ← `shio doctor` component health check
  pull.rs                           ← `shio pull` GGUF model download
  init.rs                           ← `shio init` config scaffolding
  ruby/
    mod.rs
    ffi.rs                          ← extern "C": mrb_open/close, shio_mrb_eval
    glue.c                          ← C shims hiding mrb_value from Rust
    native.rs                       ← Rust extern "C" Shio.* implementations
    vm.rs                           ← ShioVm: eval, call_tool, tool_schemas
    prelude.rb                      ← Tool class, define_tool DSL, $shio_tools registry
tools/builtin/                      ← 22 Ruby tool scripts (embedded at compile time)
vendor/
  llama.cpp/                        ← git submodule
  mruby/                            ← git submodule (commit a309524d)
mruby_configs/
  shio.rb                           ← mRuby build config
  mcp_safe.gembox                   ← restricted gembox (stdlib + math + mruby-compiler)
```

---

## Config system (`src/config.rs`)

CLI flags always override config file values, which override hardcoded defaults.

### Hardcoded defaults

| Constant | Value |
|---|---|
| `DEFAULT_SERVER_BIN` | `./bin/llama-server` |
| `DEFAULT_HOST` | `127.0.0.1` |
| `DEFAULT_PORT` | `8080` |
| `DEFAULT_NGL` | `99` |
| `DEFAULT_CTX` | `8192` |
| `DEFAULT_TEMP` | `0.7` |
| `DEFAULT_MODELS_DIR` | `./models` |

### TOML sections

| Section | Fields |
|---|---|
| `[server]` | `bin`, `host`, `port`, `ngl`, `ctx`, `cache_type_k`, `cache_type_v`, `flash_attn`, `cont_batching` |
| `[chat]` | `model`, `temperature`, `system_prompt`, `show_thinking` |
| `[paths]` | `models_dir` |
| `[tools]` | `enabled`, `confirm_writes`, `confirm_shell`, `shell_allowlist`, `shell_denylist` |
| `[lsp.servers]` | `<lang_or_ext> = "<command>"` (e.g. `rust = "rust-analyzer"`) |
| `[skills.<name>]` | `description`, `prompt` (with optional `{args}` placeholder) |

---

## Client / API layer (`src/client.rs`)

Talks to `llama-server` via the OpenAI-compatible `/v1/chat/completions` endpoint.

### Key types

```
Message { role, content?, tool_calls?, tool_call_id? }
AgentTurn::Text(String) | AgentTurn::ToolCalls(Vec<ToolCallItem>)
ToolCallItem { id, kind: "function", function: { name, arguments } }
ToolDef { kind: "function", function: FunctionSpec { name, description, parameters } }
```

### LlamaClient methods

| Method | Use |
|---|---|
| `chat_agent(messages, temp, tools)` | Agentic turn — returns `AgentTurn` |
| `chat_collect(messages, temp)` | Collect full response (no streaming) |
| `chat_stream(messages, temp)` | Stream tokens to stdout |
| `chat_stream_cb(messages, temp, on_token)` | Stream with per-token callback |

---

## Server management (`src/server.rs`)

`ServerProcess` either spawns a child `llama-server` or wraps an external URL.

- `spawn(config)` — starts `llama-server`, polls `GET /health` until ready (30 s timeout)
- `external(url)` — wraps an already-running server (no child process)
- If a server is already responding at the target URL, reuses it instead of spawning a duplicate

---

## Chat session (`src/chat.rs`)

`ChatSession` holds conversation state and enters the TUI via `run()`.

- `DEFAULT_SYSTEM_PROMPT` — compile-time fallback; `shio.toml` `[chat].system_prompt` overrides it
- `resolve_system_prompt()` — base prompt + `AGENTS.md` content (root-first order)
- Tool definitions come from `executor.tool_defs()` (queries the Ruby VM)

---

## TUI (`src/tui.rs`)

Full-screen terminal UI built with ratatui + crossterm.

### Async event loop

The model runs on a background task. Communication is via `mpsc` channel:

| TuiEvent | Meaning |
|---|---|
| `StreamToken(String)` | One token while streaming |
| `ToolStart(String)` | Tool invocation starting |
| `ToolDone(String)` | Tool result summary |
| `NeedsConfirm { prompt, reply_tx }` | Waiting for `[y/N]` |
| `AssistantText(String)` | Non-streaming response |
| `TurnDone(Vec<Message>)` | Turn complete |
| `TurnError(String)` | Turn failed |
| `PlanModeChanged(bool)` | Plan mode toggled |

### Context budget

- Tool results capped at `max_tool_result_chars` = `ctx_size * 4 * 75%` (min 24 000)
- History trimmed to ~80% of context budget before each dispatch
- Oldest non-system messages dropped first

### Display styles (`EntryKind`)

`User`, `Assistant`, `Thinking`, `ToolCall`, `ToolResult`, `Info`, `Error` — each with a distinct Solarized colour and 7-char prefix.

---

## Tool executor (`src/tools.rs`)

### ToolExecutor

```
confirm_writes: bool          — ask [y/N] before file writes
confirm_shell:  bool          — ask [y/N] before shell commands
lsp: HashMap<String,String>   — lang/ext → LSP command overrides
max_tool_result_chars: usize
shell_allowlist: Vec<String>  — if non-empty, only these commands allowed
shell_denylist:  Vec<String>  — these commands always blocked
vm: Arc<Mutex<ShioVm>>       — mRuby VM for all dispatch
```

### Shell sandboxing

Before `run_shell` executes a command, `check_shell_policy` splits it on shell
metacharacters (`;`, `|`, `&&`, `` ` ``, `$(…)`) and extracts the first token of
each segment. Each token is checked against the allowlist and denylist:

- **Allowlist non-empty:** every token must appear in the list, or the command is rejected.
- **Denylist non-empty:** any token on the list causes the command to be rejected.
- **Both set:** command must pass the allowlist *and* not appear on the denylist.
- **Both empty:** all commands allowed (default, backward-compatible).

### Dispatch flow

1. Parse `arguments` JSON; unwrap one level if model wraps under function name
2. Set thread-locals: `LSP_CONFIG_JSON`, `SHELL_ALLOWLIST`, `SHELL_DENYLIST`
3. `vm.call_tool(name, args_json)` — Ruby handler runs, returns result string

### Rust-side utilities

| Function | Purpose |
|---|---|
| `http_client()` | Process-wide `reqwest::blocking::Client` (OnceLock) |
| `is_private_host(url)` | SSRF guard — blocks loopback, RFC 1918, link-local, IMDS |
| `strip_html(html)` | Remove tags, decode entities, drop script/style |
| `percent_decode(s)` | URL percent-decoding with UTF-8 support |

---

## mRuby scripting layer (`src/ruby/`)

Tool handlers are Ruby scripts evaluated by an embedded mRuby VM.
Rust retains all OS/network operations as native `Shio.*` methods; Ruby handles
argument extraction, logic, and result formatting.

User-extensible: drop a `.rb` file in `~/.config/shio/tools/` to add or override
tools without recompiling.

### ShioVm lifecycle

1. `mrb_open()` — create mRuby interpreter
2. `shio_register_native(mrb)` — bind all `Shio.*` C shims
3. Evaluate `prelude.rb` — define `Tool` class, `define_tool` DSL
4. `load_builtin_tools()` — evaluate each `tools/builtin/*.rb` (compile-time embedded)
5. `load_user_tools()` — evaluate `~/.config/shio/tools/*.rb` (runtime)

### Native methods (`Shio.*`)

| Method | Rust operation | Notes |
|---|---|---|
| `Shio.current_dir()` | `env::current_dir()` | |
| `Shio.read_file(path)` | `fs::read_to_string` | |
| `Shio.write_file(path, content)` | `fs::write` | creates parent dirs |
| `Shio.append_file(path, content)` | `OpenOptions::append` | |
| `Shio.read_dir(path)` | `fs::read_dir` | newline-joined, dirs get `/` suffix |
| `Shio.create_dir_all(path)` | `fs::create_dir_all` | |
| `Shio.delete_file(path)` | `fs::remove_file` | |
| `Shio.rename(src, dst)` | `fs::rename` | |
| `Shio.run_shell(cmd)` | `Command::new("sh").arg("-c")` | returns stdout+stderr |
| `Shio.http_get(url, max_chars)` | reqwest blocking GET | SSRF check + HTML strip in Rust |
| `Shio.lsp_query(op, file, line, col)` | `crate::lsp::query()` | |
| `Shio.grep(pattern, path, case_insensitive)` | regex walk | skips .git/target/node_modules/vendor |
| `Shio.glob(pattern, base)` | glob walk | newline-joined paths |

### Tool DSL

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

The block receives a Hash (converted from JSON by `value_to_ruby()` in Rust).
It must return a String. Raise on error — the VM catches it and returns `"Error: <message>"`.

### Design decisions

- **Security boundary:** gembox restricted to `stdlib` + `math` + `mruby-compiler` (no `File`, `IO`, `eval()`, `define_method`)
- **No mruby-json gem:** JSON parsed in Rust, converted to mRuby hash literals via `value_to_ruby()`
- **SSRF protection in Rust:** `Shio.http_get` enforces scheme check, IP block list, HTML stripping
- **VM concurrency:** single `ShioVm` behind `Arc<Mutex<>>`, shared across `spawn_blocking` clones
- **`patch_file` fallback chain:** exact match → `trim_end()` tolerance → anchor match (first 2 + last 2 lines for blocks >= 4 lines)
- **Plan mode:** `enter_plan_mode`/`exit_plan_mode` are thin Ruby stubs; the TUI intercepts before dispatch

---

## LSP integration (`src/lsp.rs`)

Provides semantic code intelligence to the model via the `lsp` tool.

| Operation | LSP method |
|---|---|
| `hover` | `textDocument/hover` — type and documentation at cursor |
| `definition` | `textDocument/definition` — jump to declaration |
| `references` | `textDocument/references` — all usages |
| `diagnostics` | `textDocument/publishDiagnostics` — errors and warnings |

- Positions are **1-indexed** in the tool API, converted to 0-indexed for LSP protocol
- Sessions cached per server command (`static Mutex<HashMap>`)
- Server auto-spawned on first use, reused across calls
- Server resolved by: user config (`[lsp.servers]`) → project marker detection → `$PATH` scan

---

## AGENTS.md support (`src/agents.rs`)

`AGENTS.md` files provide project-specific AI instructions.

- Walk upward from cwd to the nearest `.git` root
- Collect all `AGENTS.md` files along the path
- Prepend to system prompt in root-first order (root instructions first, subdirectory overrides last)

---

## Context collection (`src/context.rs`)

Used by `/include` and `shio ask --file`.

- Single file: read if under 100 KB
- Directory: recursive walk, filtered by source extensions
- Skipped directories: `.git`, `target`, `node_modules`, `.cargo`, `dist`, `build`, `__pycache__`, `.venv`, `vendor`, `.next`, `.nuxt`
- Output: fenced code blocks with language identifier

---

## Build system (`build.rs`)

1. Copy `mruby_configs/{shio.rb, mcp_safe.gembox}` into `vendor/mruby/build_config/`
2. Run `rake -C vendor/mruby MRUBY_CONFIG=shio` → produces `libmruby.a`
3. Compile `src/ruby/glue.c` with mRuby headers via the `cc` crate
4. Link `libmruby.a` + `libm`
5. Rebuild triggers: `build.rs`, `src/ruby/glue.c`, mRuby config changes

---

## Extension points

| What | How |
|---|---|
| Custom tools | Drop `.rb` files in `~/.config/shio/tools/` |
| Custom skills | Define `[skills.<name>]` in `shio.toml` — invoke as `/<name>` in chat |
| LSP servers | Override in `[lsp.servers]` (lang/ext → command) |
| System prompt | Override in `[chat].system_prompt` or augment via `AGENTS.md` |
