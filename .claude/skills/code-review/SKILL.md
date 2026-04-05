---
name: code-review
description: Perform a project-wide code review covering security, correctness, code quality, documentation, UI/UX, and style.
---

# Code Review

Perform a thorough project-wide code review. The goal is to surface real issues — bugs, security holes, API misuse, and poor patterns — not to bikeshed style. Report findings grouped by severity, with file and line references.

## Step 1: Orient yourself

Run these in parallel to understand the current state:

```bash
git diff main...HEAD --stat
git log main...HEAD --oneline
cargo metadata --no-deps --format-version 1 | jq '.packages[].dependencies[].name'
```

Also read `src/main.rs` to understand the top-level structure and command surface.

If there are no changes since `main`, review the full `src/` tree instead.

## Step 2: Read the changed files

Read every file touched in the diff. For each file:

- Understand its role (what module is it, what does it own)
- Note any calls to unsafe code, external processes, or network I/O
- Identify any public API surface (functions, structs, CLI flags)

Do not skip files because they look small — bugs hide in helpers.

## Step 3: Run automated checks

```bash
cargo fmt --check 2>&1
cargo build 2>&1
cargo test 2>&1
cargo clippy -- -D warnings 2>&1
```

Record failures. These are **P0 blockers** — report them immediately.

## Step 4: Review for issues

Go through each changed file and apply the checklist below. Write down every finding as you go.

### 4a. Correctness
- Logic errors: off-by-one, wrong condition, missing branch
- Unhandled `Result`/`Option` — `.unwrap()` or `.expect()` in non-test code that can panic on valid input
- Integer overflow potential (unchecked arithmetic on untrusted input)
- Race conditions: shared state mutated across `tokio::spawn` without synchronization
- Incorrect use of `async` — blocking calls inside async context (`std::fs`, `std::thread::sleep`)
- Iterator misuse: `.collect()` vs streaming, unintended `.clone()` in a loop

### 4b. Security
- Command injection: any `std::process::Command` that interpolates user input without escaping
- Path traversal: file paths derived from user input not validated against a root
- Credential or token leakage: secrets logged, included in error messages, or returned in API responses
- Unsafe deserialization: `serde_json::from_str` on untrusted input without schema validation where it matters
- HTTP requests to user-supplied URLs without SSRF mitigations

### 4c. Error handling
- Errors swallowed with `let _ =` or logged and ignored when they should propagate
- User-facing error messages that expose internal paths, stack traces, or library internals
- `anyhow::bail!` / `?` used appropriately vs. match arms that handle specific error variants
- Missing context on propagated errors (prefer `.context("what we were doing")`)

### 4d. Resource management
- File handles, network connections, or child processes not closed on all exit paths
- `tokio` tasks spawned without being awaited or aborted — potential task leak
- Large allocations in hot paths (e.g., repeatedly cloning `String` or `Vec` that could be borrowed)

### 4e. API and interface design
- Public functions that accept `String` where `&str` suffices (prefer borrowing)
- Struct fields that should be private but are `pub`
- CLI flags whose behavior differs from their name or description
- Config keys that are parsed but never used, or used but never documented

### 4f. Test quality and coverage
- New functionality without tests
- Tests that only assert happy paths and skip error cases
- Tests that depend on execution order or global state
- `#[ignore]` tests without a comment explaining why
- Missing edge-case tests: empty input, max-length input, invalid UTF-8, zero/negative values where the type allows them
- Missing error-path tests: what happens when a file is missing, a network call fails, or a config key is malformed
- Tests that assert on overly broad output (e.g., `assert!(result.is_ok())`) instead of the actual value
- Untested public API: every `pub fn` and `pub struct` should have at least one test that exercises it from the outside

**Coverage check** — if `cargo-tarpaulin` is installed, run:
```bash
cargo tarpaulin --out Stdout --skip-clean 2>&1 | tail -20
```
Flag any module below 50% line coverage as a P1. Flag any module with 0% coverage as P0 if it contains non-trivial logic. If tarpaulin is not installed, note uncovered modules based on reading the test files manually.

### 4g. Style and maintainability (low priority — flag but do not block)
- Functions longer than ~60 lines that could be split
- Duplicated logic across modules that could be extracted
- `TODO` / `FIXME` comments without a tracking issue or owner
- Magic constants that should be named

## Step 5: Compile findings

Group findings into three tiers:

### P0 — Blockers (must fix before merge)
Build failures, panics on valid input, security vulnerabilities, data loss.

### P1 — Important (should fix soon)
Logic bugs that affect correctness, missing error handling on failure paths, test gaps for critical code.

### P2 — Suggestions (fix when convenient)
Style, minor inefficiencies, missing context on errors, non-critical TODOs.

## Step 6: Report

Write a structured report:

```
## Code Review — <branch or "full project">

### P0 Blockers
- **[src/foo.rs:42]** `unwrap()` on user-supplied JSON parse — panics if input is malformed.
  Fix: propagate with `?` or match on the error.

### P1 Important
- **[src/bar.rs:88]** Spawned task is never awaited — leaks if the parent future is dropped.

### P2 Suggestions
- **[src/baz.rs:15]** `clone()` inside loop; consider passing a reference instead.

### Automated checks
- `cargo fmt --check`: clean
- `cargo test`: 41 passed, 0 failed
- `cargo clippy`: clean

### No issues found in
- src/config.rs, src/main.rs, src/pull.rs
```

If there are no findings in a tier, omit that section. Always include the automated check results.

## Guidelines

**Do:**
- Cite exact file and line for every finding
- Read the actual code — do not guess from filenames or function signatures
- Flag anything that looks like a security issue, even if uncertain — let the user decide
- Run the automated checks; a passing `cargo clippy` is evidence, not a guarantee

**Do not:**
- Rewrite code speculatively — report issues, propose fixes, but do not apply them unless asked
- Penalize idiomatic Rust patterns (e.g., `?` propagation, `impl Trait` returns)
- Report P2 style issues as blockers
- Ignore test files — bugs in tests cause false confidence
