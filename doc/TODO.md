# TODO

See [reference_manual.md](reference_manual.md) for architecture documentation.

---

## Bracketed Paste Support

- [ ] Send `EnableBracketedPaste` (`ESC[?2004h`) during terminal init in `src/tui.rs`
- [ ] Send `DisableBracketedPaste` (`ESC[?2004l`) during terminal teardown
- [ ] Handle `Event::Paste(String)` in the event loop — insert pasted text into `app.input` at cursor position
- [ ] Ensure pasted newlines become literal newlines in the input buffer (not submission triggers)

## Multi-line Input Improvements

- [ ] Add `Shift+Enter` support for inserting newlines (terminal-dependent; may need `/terminal-setup` style config hints)
- [ ] Add `Ctrl+J` as a universal newline fallback
- [ ] Add `\` + `Enter` escape sequence for newline insertion

## Image Pasting

- [ ] Read image data from the system clipboard on `Ctrl+V` (investigate `arboard` or `cli-clipboard` crates)
- [ ] Detect clipboard content type (text vs. image) and branch accordingly
- [ ] Base64-encode image data for inclusion in LLM API requests
- [ ] Display an `[Image #N]` text chip in the input area as a placeholder
- [ ] Forward image data to llama-server's `/v1/chat/completions` as a vision input (requires multimodal model support)

## Terminal Image Display (stretch goal)

- [ ] Investigate Kitty graphics protocol for inline image rendering in responses
- [ ] Investigate Sixel protocol as a fallback for broader terminal compatibility
- [ ] Auto-detect terminal capabilities and choose the best available protocol
