# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Tools: `web_search` (DuckDuckGo Lite, no API key required), `save_memory` (persists facts to `SHIO.md`), `read_many_files`, `write_todos`
- `DEFAULT_SYSTEM_PROMPT` updated with guidance for the four new tools

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
