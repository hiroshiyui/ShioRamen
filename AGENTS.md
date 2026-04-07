# ShioRamen — Agent Instructions

ShioRamen is a local AI coding assistant powered by llama.cpp.
It runs entirely offline with no cloud API.
The CLI binary is named `shio`.

## Build & Test

```sh
cargo build                  # debug build
cargo build --release        # release build
cargo test                   # run all tests (~259 tests, must all pass)
cargo clippy -- -D warnings  # no warnings allowed
```

Always run `cargo test` after any change. Never leave failing tests.

## Repository Layout

| Path | Purpose |
|---|---|
| `src/main.rs` | CLI entry point, subcommand routing, system-prompt assembly |
| `src/tui.rs` | Full-screen TUI chat loop (ratatui + crossterm) |
| `src/client.rs` | llama-server HTTP client, message types, tool-call parsing |
| `src/tools.rs` | All AI tool implementations + `ToolExecutor` |
| `src/agents.rs` | AGENTS.md discovery and loading |
| `src/config.rs` | `shio.toml` parsing, `Config` struct, defaults |
| `src/context.rs` | Source-file collection for `/include` |
| `src/lsp.rs` | Language Server Protocol wrapper tool |
| `src/edit.rs` | `shio edit` subcommand (diff preview + apply) |
| `src/chat.rs` | `ChatSession` initialisation |
| `src/server.rs` | llama-server process management |
| `src/ask.rs` | `shio ask` one-shot subcommand |
| `src/doctor.rs` | `shio doctor` health check |
| `src/pull.rs` | `shio pull` model downloader |
| `src/init.rs` | `shio init` config scaffolding |

## Architecture Notes

### TUI (`tui.rs`)
- `App` struct holds all mutable state; the render loop is a `tokio::select!` over keyboard events, model-task results, and a 400 ms animation ticker.
- `AppStatus` drives input gating: `Idle` → normal editing, `Waiting` → model is running, `Confirming` → awaiting y/n for a destructive tool, `ConfirmExit` → awaiting y/n to quit.
- Input is a `String` with a byte-offset `cursor`. Multi-line input is supported via embedded `\n`; `line_starts()` and `cursor_line_col()` do all line/column arithmetic.
- Code blocks are rendered with syntax highlighting (syntect, `"Solarized (light)"` theme) and Solarized-tinted backgrounds. Diff blocks get per-line background colours based on `+`/`-`/`@@` prefixes.

### Tools (`tools.rs`)
- `all_tools()` returns the full `Vec<ToolDef>` sent to the model.
- `ToolExecutor::execute_quiet()` dispatches a `ToolCallItem` to the matching handler.
- Confirmation flags `confirm_writes` and `confirm_shell` gate destructive operations.
- Adding a new tool requires: (1) a `ToolDef` entry in `all_tools()`, (2) a match arm in `execute_quiet()`, (3) a handler function, (4) tests.

### Colour Scheme
All colours are Solarized. Constants are defined at the top of `tui.rs`:

```rust
SOL_BASE02  // title bar background
SOL_BASE01  // secondary text, fence labels
SOL_BASE2   // code block background (= CODE_BG)
SOL_YELLOW  // tool calls, waiting spinner
SOL_ORANGE  // confirmation prompts
SOL_RED     // errors, exit prompt
SOL_CYAN    // user messages
SOL_GREEN   // tool results, select-mode notice
```

Do not introduce new `Color::` literals; map everything to an existing `SOL_*` constant or add a named one.

## Code Conventions

- **License header**: every new `.rs` file starts with `// SPDX-License-Identifier: GPL-3.0-or-later`.
- **Error handling**: use `anyhow::Result` and `?`. No `.unwrap()` in production paths.
- **Async**: Tokio throughout. Keep blocking work off the async executor (`tokio::task::spawn_blocking` if needed).
- **Tests**: inline `#[cfg(test)] mod tests { use super::*; … }` at the bottom of each file. Pure functions must be tested; I/O-heavy functions use `tempfile`-style temp dirs. Every code change (add / modify / delete) must be accompanied by tests that directly target the changed behaviour — new functions get new tests, modified logic gets updated tests, deleted code gets its tests removed.
- **No speculative abstractions**: add helpers only when used in two or more places.
- **Formatting**: `cargo fmt` default style.

## Key Invariants

- `cargo test` must pass with zero failures before committing.
- Colour constants must remain Solarized — do not introduce arbitrary `Rgb` values outside the `SOL_*` block.
- The `line_starts` / `cursor_line_col` helpers own all multi-line cursor math; do not duplicate offset arithmetic elsewhere in `tui.rs`.
- `fence_prefix_len` is the single source of truth for detecting markdown code fences — use it instead of ad-hoc prefix checks.
- Tool confirmation (`needs_confirm`) checks `confirm_writes` and `confirm_shell` flags from `ToolExecutor`; do not bypass this in new tools.
