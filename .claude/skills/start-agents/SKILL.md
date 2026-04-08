---
name: start-agents
description: Read and import AGENTS.md into the current session context. Use this skill when the user says "start agents", "load agents", "import AGENTS.md", "read AGENTS.md", or wants to apply agent instructions from an AGENTS.md file.
allowed-tools: [Read, Glob]
---

# Start Agents

Read `AGENTS.md` from the project root and apply its instructions as active context for this session.

## Step 1: Locate AGENTS.md

Check the project root for `AGENTS.md`:

```
<project-root>/AGENTS.md
```

If not found, check parent directories up to the repo root. If still not found, inform the user that no `AGENTS.md` was found and stop.

## Step 2: Read the file

Use the Read tool to load the full contents of `AGENTS.md`.

## Step 3: Apply the instructions

Parse and internalize all instructions in `AGENTS.md` as if they had been written in `CLAUDE.md`. This includes:

- Coding conventions and style rules
- Tool usage preferences
- Workflow instructions
- Any agent-specific behavior overrides

Treat the content with the same authority as `CLAUDE.md` — it governs how you should behave for the remainder of this session.

## Step 4: Confirm

Report to the user:
- The path of the `AGENTS.md` file that was loaded
- A brief summary (3–5 bullet points) of the key instructions it contains

Do not repeat the full file — just the salient points so the user can verify the right file was loaded and the right instructions are active.
