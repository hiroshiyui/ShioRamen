---
name: commit-and-push
description: Commit and push changes to Git, grouped by topic. Use this skill whenever the user asks to "commit and push", "push my changes", "save and push", "git commit and push", or wants to stage/commit/push any set of changes — especially when there are multiple unrelated changes that should be organized into separate topical commits before pushing.
---

# Commit and Push by Topic

Group all pending changes into cohesive, topic-based commits, then push to the remote.

## Step 1: Assess the current state

Run these in parallel:

```bash
git status
git diff
git diff --cached
git log --oneline -5
```

Read the output to understand:
- Which files are modified, staged, or untracked
- What the actual changes are
- The existing commit style (message format, conventions, capitalization)

## Step 2: Group changes by topic

Analyze the diff and mentally cluster files by what they accomplish together. A "topic" is a cohesive unit of intent — not just a file type or directory. Good examples:

- **feat(auth): add JWT refresh token support** — multiple files all serving one feature
- **fix(parser): handle empty input edge case** — a bug fix touching test + source
- **chore: update dependencies** — lockfile + package manifest

Do NOT split arbitrarily by file type or directory. Ask: "Would reverting this commit leave the repo in a coherent, working state?"

If all changes belong to a single topic, make a single commit. If changes are clearly unrelated (e.g., a bug fix AND a refactor AND a dependency update), split them into separate commits.

## Step 3: Commit each topic

For each topic group, in a logical order (dependencies first, then features, then chores):

1. Stage only the relevant files:
   ```bash
   git add <file1> <file2> ...
   ```
   Prefer naming files explicitly over `git add .` to avoid accidentally staging unintended files (`.env`, build artifacts, etc.).

2. Write a commit message that:
   - Matches the existing project style (check the log from Step 1)
   - Summarizes the *why*, not just the *what*
   - Is concise (50–72 chars for the subject line)
   - Uses conventional commits format if the project does (`type(scope): message`)

3. Commit using a heredoc to preserve formatting:
   ```bash
   git commit -m "$(cat <<'EOF'
   type(scope): short summary

   Optional body explaining the why, if the change is non-obvious.

   Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
   EOF
   )"
   ```

Repeat for each topic group.

## Step 4: Push

After all commits are made, push to the tracked remote branch:

```bash
git push
```

If the branch has no upstream yet:
```bash
git push -u origin <branch-name>
```

If the push is rejected due to upstream changes, **do not force push**. Instead:
```bash
git pull --rebase
git push
```

Inform the user if there were conflicts that required manual resolution.

## Step 5: Confirm

Show the user a summary:
- How many commits were made and their messages
- The branch and remote that were pushed to
- The new `git log --oneline -3` so they can see the result

## Guidelines

**Never:**
- Use `git add -A` or `git add .` without first reviewing what it would stage
- Force push (`--force`) without explicit user instruction
- Skip hooks (`--no-verify`)
- Amend commits that have already been pushed

**Always:**
- Check for sensitive files (`.env`, secrets, credentials) before staging — warn the user if any are present
- Respect the project's existing commit message conventions
- Make commits atomic: each one should represent a single logical change that passes tests on its own

**When in doubt about grouping:** err on the side of fewer, larger commits rather than many tiny ones. Overly granular commits add noise to the history without clarity.
