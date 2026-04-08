---
name: docs-engineering
description: Project-wide document engineering, keep documents valid, in-sync, up-to-date.
---

# Docs Engineering

Audit and repair project documentation so it accurately reflects the current state of the code. This skill covers validity (broken links, stale examples), sync (docs match the implementation), and completeness (new features are documented).

## Step 1: Discover documents

Find all documentation files in the project (excluding vendored deps):

```bash
find . -name "*.md" ! -path "./vendor/*" ! -path "./target/*"
```

Also check config and schema files that double as documentation:

```bash
find . -name "*.toml" ! -path "./vendor/*" ! -path "./target/*" -not -name "Cargo.lock"
```

## Step 2: Understand the code surface

Before auditing docs, understand what the code actually exposes. For this Rust project:

**Commands and CLI flags** — read the clap struct definitions:
```bash
grep -r "struct.*Args\|#\[command\]\|#\[arg\]" src/ --include="*.rs" -l
```

**Config keys** — read the config structs:
```bash
grep -r "struct.*Config\|#\[serde" src/ --include="*.rs" -A 3
```

**Public API surface** — read `src/main.rs` and submodules.

Read the actual source files; do not guess from the docs what the code does.

## Step 3: Audit each document

For each document, check:

### 3a. CLI option tables
Compare every flag listed in the docs against the actual clap definitions in source. For each discrepancy:
- Flag listed in docs but not in code → remove it
- Flag in code but missing from docs → add it
- Description or default value differs → correct it

### 3b. Config file reference
Compare every key in the `shio.toml` reference block against the config structs in `src/`. For each discrepancy:
- Key listed in docs but not in struct → remove it
- Field in struct missing from docs → add it with its type and default
- Wrong type or default → correct it

### 3c. Code examples
For each shell command block in docs, verify it would actually work:
- Binary names match what `cargo install` produces
- Flags match current CLI
- Paths like `./bin/llama-server` are consistent throughout

### 3d. Internal links
Check all relative markdown links (`[text](path)`):
```bash
grep -o '\[.*\]([^)]*\.md[^)]*)' README.md
```
Verify each linked file exists. Fix or remove broken links.

### 3e. External URLs
Spot-check external links (GitHub repos, official docs). Flag any that look stale or wrong — do not auto-fix external URLs without confirming with the user.

## Step 4: Repair

Make the minimum edits needed to make docs accurate:

- **Edit the document** to match the code (not the other way around) unless the code itself appears to be wrong.
- Keep the existing prose style and structure; only change what is incorrect.
- Do not reformat sections you aren't fixing.
- Do not add documentation for features not yet implemented.

## Step 5: Maintain TODO.md

For any `doc/TODO.md` (or equivalent task-tracking document) in the project:

### 5a. Mark completed tasks

Cross-reference each task/step against the actual code on disk and git history.
A task is **done** if:
- The files it describes exist and contain the expected content, **or**
- `git log` shows a commit that implemented it

Mark done tasks by prefixing the heading with `[x]`:
```markdown
### [x] A1 — Add mRuby git submodule
```

### 5b. Summarize completed phases

When **all** steps in a phase are done, replace the entire phase section (which may contain
multi-screen code listings and prose) with a compact summary block:

```markdown
## Phase A — Infrastructure ✓ DONE

**Files created:** `build.rs`, `mruby_configs/shio.rb`, `mruby_configs/mcp_safe.gembox`,
`src/ruby/{ffi.rs, glue.c, native.rs, prelude.rb, vm.rs, registry.rs, mod.rs}`

**Key decisions recorded:**
- mRuby pinned to commit `a309524d0` (same as rrcad)
- gembox: `stdlib` + `math` + `mruby-compiler` only (security boundary)
- `#![allow(dead_code)]` on Phase A stubs — removed in Phase B when used

**Verified:** `cargo build` ✓ · 320 tests pass ✓ · `clippy -D warnings` ✓ · `cargo fmt` ✓
```

Keep the summary tight: one sentence per key decision, one line for files created, one line for verification status. Drop all code listings — they're in git.

### 5c. Remove completed items

Once a task or phase has been fully summarized (per 5b), **delete** the completed entries from
`doc/TODO.md` to keep the file focused on remaining work. If a completed item contains useful
reference information (key decisions, gotchas, API notes), move that content to
`doc/reference_manual.md` before removing it from `TODO.md`.

### 5d. Leave pending phases untouched

Do **not** modify any phase that is not yet started or only partially done. Only summarize
when the entire phase (every numbered step) is verifiably complete.

## Step 6: Verify the example config

The `shio.toml` in the repo root is an example config. Cross-check it against `src/config.rs` (or equivalent): every key present in the file must be a recognized field. Update the example file if needed.

### 5a. Sync `system_prompt` ↔ `DEFAULT_SYSTEM_PROMPT`

`shio.toml` `[chat].system_prompt` must be kept word-for-word in sync with `DEFAULT_SYSTEM_PROMPT` in `src/chat.rs`. They serve the same role: `shio.toml` is the editable copy users customise; the Rust constant is the compile-time fallback used when no config file is present.

- Read `DEFAULT_SYSTEM_PROMPT` from `src/chat.rs`.
- Read `system_prompt` from `shio.toml`.
- If they differ, update `shio.toml` to match `src/chat.rs` (the Rust source is authoritative).
- TOML `"""` strings use `\` as a line-ending escape (same as Rust), so the formatting translates directly.

## Step 6: Report

After completing repairs, give the user a concise summary:

```
Docs audit complete.

Fixed:
- README.md: removed --flash-attn flag (renamed to --flash-attention in v0.x)
- README.md: added --system-prompt option to `serve` and `chat` sections
- shio.toml: corrected cache_type_k default from "q4_0" to "f16"

No issues:
- Internal links: all valid
- Command examples: all match current CLI

Skipped (needs confirmation):
- [External URL] https://... — looks potentially stale, please verify
```

## Guidelines

**Do:**
- Read source before editing docs — trust the code, not memory
- Make targeted edits; preserve structure and voice
- Check both the narrative docs AND the inline code/config examples

**Do not:**
- Rewrite docs wholesale when a small correction suffices
- Document aspirational or planned features as if they exist
- Change code to match docs without flagging it to the user first
- Touch `vendor/` documentation
