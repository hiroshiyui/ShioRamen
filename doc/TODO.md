# TODO

See [reference_manual.md](reference_manual.md) for architecture documentation.

---

## Bracketed Paste Support

- [x] Send `EnableBracketedPaste` (`ESC[?2004h`) during terminal init in `src/tui.rs`
- [x] Send `DisableBracketedPaste` (`ESC[?2004l`) during terminal teardown
- [x] Handle `Event::Paste(String)` in the event loop — insert pasted text into `app.input` at cursor position
- [x] Ensure pasted newlines become literal newlines in the input buffer (not submission triggers)

## Multi-line Input Improvements

- [x] Add `Shift+Enter` support for inserting newlines (terminal-dependent; may need `/terminal-setup` style config hints)
- [x] Add `Ctrl+J` as a universal newline fallback
- [x] Add `\` + `Enter` escape sequence for newline insertion

## Image Pasting

- [x] Read image data from the system clipboard on `Ctrl+V` (investigate `arboard` or `cli-clipboard` crates)
- [x] Detect clipboard content type (text vs. image) and branch accordingly
- [x] Base64-encode image data for inclusion in LLM API requests
- [x] Display an `[Image #N]` text chip in the input area as a placeholder
- [x] Forward image data to llama-server's `/v1/chat/completions` as a vision input (requires multimodal model support)

## Drag-and-Drop Image Support

- [x] Detect image file paths in `Event::Paste` (terminals deliver drag-and-drop as pasted text)
- [x] Recognize common image extensions (`.png`, `.jpg`, `.jpeg`, `.gif`, `.webp`, `.bmp`)
- [x] Read and base64-encode the dropped file, attach it as an image (reuse existing `attached_images` flow)
- [x] Handle multiple files dropped at once (split on newlines, process each path)