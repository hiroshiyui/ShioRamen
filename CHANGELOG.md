# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [v0.5.1] — 2026-04-08

### Added
- (sampling): `top_p` and `repeat_penalty` parameters — CLI flags, config keys, and `SamplingParams` struct across chat, ask, and edit subcommands

## [v0.5.0] — 2026-04-08

### Added
- (doctor): mRuby VM health check — verifies VM initialisation and ≥ 22 built-in tools registered
- (chat): session persistence — auto-save on exit, resume with `shio chat --resume`
- (chat): model-aware prompt styles — `prompt_style` config key selects "full", "concise", or "minimal" system prompt based on model size
- (client): streaming in agentic turns — tokens streamed to TUI in real-time during tool-use loops
- (tools): shell command sandboxing — configurable `shell_allowlist` / `shell_denylist` in `[tools]`
- (tui): parallel tool execution — approved tools now run concurrently via `spawn_blocking`

### Documentation
- README: added `prompt_style` to `[chat]` config reference
- `doc/TODO.md`: cleaned up completed task entries

### Maintenance
- Test count: 318 → 348 (30 new tests for VM checks, session persistence, streaming, shell policy)
- `docs-engineering` skill: added §5c rule for removing completed TODO items

## [v0.4.0] — 2026-04-08

### Added
- (ruby): embedded mRuby scripting layer — tool handlers are now Ruby scripts evaluated by an mRuby VM, replacing monolithic Rust dispatch
- (ruby): Phase A infrastructure — `build.rs`, `src/ruby/` skeleton, mRuby build config with restricted gembox
- (ruby): Phase B — parallel dispatch via `Arc<Mutex<ShioVm>>`, `ToolDef` plumbing from Ruby VM
- (tools): migrate all 22 tools to Ruby (Phases C1–C9): file I/O, search, shell, web, LSP, plan mode
- (skills): `start-agents` skill for importing AGENTS.md instructions
- (tools): user-extensible tools — drop `.rb` files in `~/.config/shio/tools/` to add or override tools without recompiling

### Fixed
- (security): block Ruby `#{}` string interpolation injection in tool arguments via `value_to_ruby`
- (security): SSRF filter now catches IPv4-mapped IPv6 addresses (`::ffff:127.0.0.1`)
- (security): SSRF filter now blocks IPv6 unspecified address (`::`)
- (security): disable HTTP redirect following to prevent 302→private-host bypass
- (security): DNS-resolved hostnames checked against private IP ranges (`resolves_to_private`)
- (tools): `patch_file` anchor fallback for large `old_str` blocks
- (pull): partial downloads saved to `.part` file, renamed on success — prevents corrupt models
- (edit): backup file (`.bak`) written before overwriting original
- (context): symlinks skipped during directory collection to prevent traversal escape
- (context): recursion depth capped at 20 levels
- (client): connect timeout (10 s) and request timeout (5 min) added to `LlamaClient`
- (tui): `prev_word`/`next_word` rewritten to respect UTF-8 char boundaries (prevents panic on multi-byte input)
- (tui): scroll field widened from `u16` to `u32` to support long sessions
- (client): SSE `[DONE]` sentinel now terminates both inner and outer streaming loops

### Changed
- (tools): Phase D cleanup — removed `all_tools()`, `dispatch()`, `SHIO_USE_RUBY` flag; `tool_defs()` now queries the Ruby VM directly
- (tools): dropped `strip_line_number_prefix` (no longer needed)

### Documentation
- `doc/reference_manual.md`: project-wide reference covering architecture, config, tools, mRuby layer, LSP, and extension points
- `doc/TODO.md`: cleared completed phases, points to reference manual

### Maintenance
- (ci): fix build — checkout submodules, install Ruby and bison
- Test count: 312 → 318 (6 new security tests)

## [v0.3.0] — 2026-04-07

### Added
- (tui): display thinking blocks from extended thinking models in a collapsible panel
- (tui): Esc to abort agent turn, `/clear` to reset history, `/stats` to show context usage
- (tools): `insert_after_line` tool for positional line insertion

### Fixed
- (tools): strip box-drawing `│` prefix from `new_str` in patch operations; preserve plain `|`
- (tools): strip line-number prefixes in `write_file`, `append_file`, and `patch_file`; restore them only in `read_file_range` output
- (tools): preserve verbatim content in `write`/`append`/`insert`; strip prefixes only in patch `old_str`
- (tools): `patch_file` fallback tolerates trailing whitespace and rejects whitespace-only `old_str`
- (tui): dynamic tool-result cap scaled to configured context window size
- (tui): accurate context budget tracking; trim only pre-turn history mid-loop to prevent overflow
- (tui): remove mid-turn history trim from agent loop that caused incorrect trimming
- (client): correct `SlotInfo` schema deserialization

### Changed
- (tools): `read_file_range` outputs raw lines without embedded line-number prefixes

### Maintenance
- (prompt): warn model not to include line-number prefixes in `patch_file` arguments
- (prompt): forbid `append_file` for in-place edits; steer to `patch_file`
- (prompt): instruct model to call tools immediately without preamble

## [v0.2.0] — 2026-04-07

### Added
- (tui): multi-line input, syntax highlighting, and Solarized color theme
- (tui): exit confirmation dialog to prevent accidental session loss

### Fixed
- (tui): abort agent turn immediately when user denies a tool call

### Documentation
- Added AGENTS.md with project conventions and agent guidelines

## [v0.1.0] — 2026-04-06

### Added
- Custom skills: define named prompt templates under `[skills.<name>]` in `shio.toml` and invoke them as `/slash-commands` in TUI chat; `/skills` lists all defined skills; tab completion includes skill names dynamically
- AGENTS.md support: project-specific AI instructions loaded by walking the directory tree upward to the nearest `.git` root, then prepended to the system prompt automatically in `chat`, `ask`, and `edit`
- `shio init`: scaffold a fully-commented `shio.toml` config file in the current directory
- LSP client tool (`lsp`): hover, go-to-definition, find-references, and diagnostics from any language server configured in `[lsp.servers]`
- Plan mode: `enter_plan_mode` / `exit_plan_mode` tools switch the model to read-only exploration before applying multi-file changes
- Tools: `append_file` for non-destructive file extension
- TUI: horizontal input scrolling so lines longer than the box width stay fully visible

### Fixed
- (tui): retry on empty model response instead of hard-failing the turn
- (tui): nudge local models that emit EOS immediately after a tool result
- (tui): model task now stored as a `JoinHandle` and aborted on quit, preventing background tasks from outliving the session
- (client): tool calls embedded in model content (peg-gemma4 template) correctly extracted
- (tools): local-model argument-wrapper quirks handled in tool dispatch
- (server): zombie process reaped with `child.wait()` after `kill()` on startup timeout
- (client): raw bytes accumulated and decoded only up to the last newline, preventing multi-byte UTF-8 sequences from being split across HTTP chunks
- (tools): SSRF bypass via userinfo in URL authority (`user@192.168.1.1`) closed in `is_private_host`
- (tools): `write_todos` now creates parent directories before writing

### Changed
- `ServerArgs::spawn_or_connect` helper centralises server setup across `chat`, `ask`, and `edit`, removing ~36 lines of duplicated host/port/model resolution
- `require_str!` macro replaces 15 identical 4-line argument-extraction blocks in `tools.rs`
- `ensure_parent_dirs` helper replaces 4 duplicate `create_dir_all` guard blocks
- LSP session map uses a single `get_mut` lookup instead of a double lookup

### Documentation
- README: `/skills`, `/<skill-name>`, `shio init`, LSP config, and skills config reference added
- `shio.toml`: system prompt synced with the compile-time default (LSP and plan-mode paragraphs were missing)

### Maintenance
- Test count: 124 → 230
- `envsetup.sh` added for one-shot llama.cpp build and binary wiring
- Pre-commit hook committed to `.githooks/` and activated via `core.hooksPath`

## [v0.0.2] — 2026-04-06

### Added
- Tools: `web_search` (DuckDuckGo Lite, no API key required), `save_memory` (persists facts to `SHIO.md`), `read_many_files`, `write_todos`
- System prompt updated with guidance for the four new tools (both the compile-time default and the example `shio.toml`)
- 15 new unit tests covering TUI helper functions (`char_start_before`, `char_end_at`, `prev_word`, `next_word`, `split_path`, `replace_latex`)

### Fixed
- (tui): `patch_file`, `delete_file`, and `move_file` now trigger the `[y/N]` confirmation prompt when `confirm_writes = true`; previously they bypassed the gate silently
- (server): `--system-prompt` flag removed from `llama-server` invocation (flag not supported by the binary)
- Blocking filesystem I/O in `ask` and `edit` commands moved to `spawn_blocking` / `tokio::fs` to avoid stalling the async executor

### Performance
- HTTP clients in `fetch_url`, `web_search`, and `health_check` cached via `OnceLock` instead of being rebuilt on every call
- SSE line-buffer trimming changed from `to_string()` (allocation per line) to `drain()` (in-place)

### Documentation
- README: chat feature line updated to mention web search and memory; Ctrl+U description corrected
- CHANGELOG: `[Unreleased]` section added and now promoted to `[v0.0.2]`

### Maintenance
- Test count: 109 → 124

## [v0.0.1] — 2026-04-05

### Added
- Full-screen TUI with ratatui (Claude Code-style layout) replacing the line-mode REPL
- Agentic tool loop: model can read/write files, run shell commands, and search code
- Tools: `read_file`, `write_file`, `list_directory`, `run_shell`, `search_files`, `grep_files`
- Tools: `read_file_range`, `patch_file`, `delete_file`, `move_file`, `fetch_url`
- Tools: `create_directory`, `get_working_directory`
- `fetch_url` strips HTML to readable text with SSRF guard and UTF-8-safe truncation
- F2 select mode: toggle between mouse-scroll and terminal text-selection
- Mouse scroll wheel support for output pane
- Animated thinking indicator (🤔. / 🤔.. / 🤔...)
- Bash-style shortcuts: Ctrl+A, Ctrl+E, Ctrl+W in the input box
- Tab completion for slash commands and `/include` paths
- Trust prompt: confirm before enabling tools for the current directory
- `ask` subcommand: non-interactive single-question mode
- `edit` subcommand: apply an instruction to a file directly
- `/include` command: inject file or directory contents into context
- `shio.toml` config: system prompt, model path, temperature, server flags
- `shio doctor`: health-check for binary, model, GPU, and server
- `shio pull`: download GGUF models from Hugging Face
- `shio serve`: launch and manage `llama-server` as a child process
- GitHub Actions CI workflow with `rustfmt` and `clippy`

### Fixed
- Multi-byte UTF-8 character handling in input cursor and editing
- Input cursor position uses display-column width, not byte offset
- Trust prompt box alignment
- Tool-call detection uses presence of tool calls, not `finish_reason`
- HuggingFace blob URLs rewritten to resolve URLs for direct download
- `llama-server` logs now visible; `--flash-attn` flag corrected
- `&Path` preferred over `&PathBuf` at internal API boundaries
- Per-file size limit (100 KB) enforced on `/include` to avoid context overflow
- `execute!` error in `toggle_select_mode` handled gracefully

### Changed
- System prompt expanded with tool-use grounding and Unicode symbol preference
- LaTeX math notation replaced with Unicode symbols in assistant output

### Maintenance
- GPL-3.0-or-later license adopted
- `rustyline` dependency removed (line-mode REPL replaced by TUI)
- Regex statics compiled once via `OnceLock` to avoid repeated recompilation
- Claude Code skills added: `commit-and-push`, `code-review`, `docs-engineering`, `release-engineering`
- Test coverage expanded to 99 tests
