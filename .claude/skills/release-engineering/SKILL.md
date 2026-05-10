---
name: release-engineering
description: Cut a versioned release for this Rust project. Analyzes conventional commits since the last tag, determines the correct semantic version bump, updates Cargo.toml and CHANGELOG.md, creates a release commit and tag, publishes a GitHub release, and pushes everything. Use this skill whenever the user asks to "cut a release", "release a new version", "publish a release", "tag a release", or similar.
---

# Release Engineering

Cut a complete, well-documented release: version bump → changelog → commit → tag → GitHub release → push.

## Step 1: Validate the release environment

Run these in parallel to get a complete picture before touching anything:

```bash
git status
git log --oneline -10
git tag --sort=-version:refname | head -5
git fetch --tags
```

**Gate conditions — stop and report if any fail:**
- Working tree must be clean (no uncommitted changes)
- Branch must be `main` (`git branch --show-current`)
- Local `main` must be in sync with `origin/main` (`git rev-list HEAD...origin/main --count` → must be `0`)

If any gate fails, report the issue and ask the user to resolve it before proceeding.

## Step 2: Run quality gates

Do not proceed to versioning until all of these pass:

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

- `cargo fmt --check` fails → run `cargo fmt`, stage the changes as part of a pre-release cleanup commit, then continue.
- `cargo clippy` fails → fix the warnings (they are usually trivial), re-run, then continue.
- `cargo test` fails → stop entirely. Report which tests failed and ask the user to fix them first.

## Step 3: Analyze commits since the last release

Find the last release tag and collect all commits since it:

```bash
LAST_TAG=$(git tag --sort=-version:refname | grep '^v' | head -1)
echo "Last tag: $LAST_TAG"
git log ${LAST_TAG}..HEAD --oneline
```

If no `v*` tag exists, use the full history:
```bash
git log --oneline
```

Parse the commits using conventional commit prefixes:

| Prefix | Meaning | Version impact |
|--------|---------|----------------|
| `feat!:` or `BREAKING CHANGE` in body | Breaking change | **Major** bump |
| `feat:` or `feat(scope):` | New feature | **Minor** bump |
| `fix:`, `perf:`, `refactor:` | Bug fix / improvement | **Patch** bump |
| `chore:`, `docs:`, `test:`, `ci:` | Housekeeping | No bump (unless it is the only type) |

**Bump rules (highest type wins):**
1. Any breaking change → major
2. Any `feat` (no breaking) → minor
3. Only fixes/chores → patch

If there are no meaningful commits since the last tag, ask the user whether to proceed with a patch bump or abort.

## Step 4: Determine the new version

Read the current version from `Cargo.toml`:

```bash
grep '^version' Cargo.toml
```

Parse it as `MAJOR.MINOR.PATCH` and apply the bump determined in Step 3.

Examples:
- `0.1.0` + minor → `0.2.0`
- `0.2.0` + patch → `0.2.1`
- `0.2.0` + major → `1.0.0`

**Before proceeding, confirm with the user:**
> "I'll release **v{NEW_VERSION}** (was {OLD_VERSION}). Bump type: {major|minor|patch} based on: {one-line reason}. Proceed?"

Do not make any file edits until the user confirms.

## Step 5: Update `Cargo.toml`

Edit only the `version` field at the top of `Cargo.toml`:

```toml
version = "{NEW_VERSION}"
```

Do not touch any other field. After editing, run `cargo check` to ensure the manifest is valid and `Cargo.lock` is updated:

```bash
cargo check
```

Stage both files:
```bash
git add Cargo.toml Cargo.lock
```

## Step 6: Update `CHANGELOG.md`

If `CHANGELOG.md` does not exist, create it with the header:
```markdown
# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
```

Prepend a new release section immediately after the header (above any existing release sections):

```markdown
## [v{NEW_VERSION}] — {YYYY-MM-DD}

### Added
- ...  (feat commits)

### Fixed
- ...  (fix commits)

### Changed
- ...  (refactor, perf commits)

### Documentation
- ...  (docs commits)

### Maintenance
- ...  (chore, ci, test commits)
```

Rules:
- Use today's date in `YYYY-MM-DD` format.
- Only include sections that have entries; omit empty sections.
- Write each entry as a concise one-liner summarizing the change (not the raw commit message). Strip conventional commit prefixes.
- If a commit message has a scope, include it: `(scope): summary`.
- Omit merge commits and commits that only bump the version.

Stage the file:
```bash
git add CHANGELOG.md
```

## Step 7: Create the release commit and tag

Create a single release commit containing `Cargo.toml`, `Cargo.lock`, and `CHANGELOG.md`:

```bash
git commit -m "$(cat <<'EOF'
chore(release): v{NEW_VERSION}

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
EOF
)"
```

Then tag it:

```bash
git tag -a "v{NEW_VERSION}" -m "Release v{NEW_VERSION}"
```

Verify the tag was created:
```bash
git log --oneline -3
git tag --sort=-version:refname | head -3
```

## Step 8: Create the GitHub release

Extract the new release section from `CHANGELOG.md` to use as the release body:

```bash
awk '/^## \[v{NEW_VERSION}\]/,/^## \[v/' CHANGELOG.md | head -n -1
```

Then create the GitHub release:

```bash
gh release create "v{NEW_VERSION}" \
  --title "v{NEW_VERSION}" \
  --notes "$(awk '/^## \[v{NEW_VERSION}\]/,/^## \[v/' CHANGELOG.md | head -n -1)"
```

If this is the first ever release (no previous section), use:
```bash
gh release create "v{NEW_VERSION}" \
  --title "v{NEW_VERSION}" \
  --notes-file <(awk '/^## \[v{NEW_VERSION}\]/,0' CHANGELOG.md)
```

## Step 9: Push commits and tags

Push the release commit and the new tag together:

```bash
git push origin main
git push origin "v{NEW_VERSION}"
```

If the push is rejected (upstream diverged), **do not force push**. Rebase and retry:
```bash
git pull --rebase origin main
git push origin main
git push origin "v{NEW_VERSION}"
```

## Step 10: Install the new release locally

After the release is pushed, build and install the new binary into the user's
Cargo bin so `shio` on `$PATH` matches the just-released version. Both commands
are mandatory — `cargo build --release` warms the release artefact cache and
catches any release-profile-only breakage before `cargo install` overwrites the
existing binary.

```bash
cargo build --release
cargo install --path .
```

If either step fails, report the failure but **do not** roll back the release —
the tag and GitHub release are already published. Investigate the local build
issue separately; users pulling from git are unaffected.

After install, verify the installed binary reports the new version:

```bash
shio --version
```

## Step 11: Verify and report

Confirm everything landed:

```bash
git log --oneline -3
gh release view "v{NEW_VERSION}"
```

Then give the user a concise summary:

```
Release v{NEW_VERSION} is live.

Version bump: {old} → {new} ({major|minor|patch} — {reason})
Commits included: {N} commits since v{PREV_VERSION}
GitHub release: https://github.com/{owner}/{repo}/releases/tag/v{NEW_VERSION}

Changes:
  Added:   {count} items
  Fixed:   {count} items
  Other:   {count} items
```

## Guidelines

**Never:**
- Release from a branch other than `main`
- Force push (`--force`) to push the release
- Skip the user confirmation in Step 4
- Create a tag without a corresponding CHANGELOG entry
- Release with failing tests or clippy warnings

**Always:**
- Confirm the version bump with the user before making any file edits
- Keep `Cargo.toml`, `Cargo.lock`, and `CHANGELOG.md` in the same release commit
- Use annotated tags (`-a`), not lightweight tags
- Include the GitHub release URL in the final report

**If something goes wrong mid-release:**
- Before the push: undo with `git reset --hard HEAD~1` and `git tag -d v{VERSION}`
- After the push: do not amend or revert the release commit — instead cut a patch release with a fix
