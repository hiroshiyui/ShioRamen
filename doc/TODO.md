# TODO

See [reference_manual.md](reference_manual.md) for architecture documentation.

---

## ~~1. Sandbox `run_shell`~~ ✓ DONE

Implemented configurable `shell_allowlist` / `shell_denylist` in `[tools]` config.
Commands are checked by first token before execution; pipelines and chained
commands are split and each segment is checked independently.

## ~~2. Streaming in the agentic loop~~ ✓ DONE

Replaced `chat_agent` (non-streaming) with `chat_agent_stream` which streams
tokens to the TUI via `StreamToken` events while accumulating tool-call deltas
internally. The user now sees reasoning text in real-time during agentic turns.

## ~~3. Session persistence~~ ✓ DONE

Sessions auto-save to `~/.local/share/shio/sessions/latest.json` on exit.
Resume with `shio chat --resume`. The system prompt is always refreshed
from the current config; only user/assistant/tool messages are restored.

## ~~4. Model-aware prompt tuning~~ ✓ DONE

Added `[chat].prompt_style` config key with three built-in styles: `"full"`
(detailed, for 30B+), `"concise"` (mid-size 7B–30B), `"minimal"` (< 7B).
`"auto"` (default) detects from model filename parameter count. Explicit
`system_prompt` always takes precedence.

## 5. Verify mRuby VM in `shio doctor`

Currently checks binary, model, GPU, and server. Add a `ShioVm::new()` smoke test
that verifies the VM initialises and all 22 tools register — catches build/linking
issues early.

## 6. Parallel tool execution

The current loop executes tool calls sequentially. When the model requests multiple
independent tools in one turn (e.g. read 3 files), they could run concurrently via
`tokio::task::spawn_blocking` per call. The VM mutex serialises them today, but
future native-only tools could benefit.
