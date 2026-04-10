// SPDX-License-Identifier: GPL-3.0-or-later
//! Session persistence — auto-save conversations to disk and reload them.
//!
//! Sessions are stored as JSON files under
//! `~/.local/share/shio/sessions/<cwd-slug>/`, where `<cwd-slug>` is the
//! current working directory with `/` replaced by `-` (Claude Code style).
//! Scoping by cwd prevents shio sessions from different projects clobbering
//! each other through a single shared `latest.json`.
//!
//! Each file contains a timestamped array of [`Message`]s that can be loaded
//! back into a new `ChatSession`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::client::Message;

/// Return the global root sessions directory, creating it if it does not exist.
pub fn sessions_dir() -> Result<PathBuf> {
    let dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("shio/sessions");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Cannot create sessions directory: {}", dir.display()))?;
    Ok(dir)
}

/// Encode an absolute path as a single filesystem-safe directory name by
/// replacing every `/` with `-`.  E.g. `/home/yhh/MyProjects/ShioRamen` →
/// `-home-yhh-MyProjects-ShioRamen`.  Mirrors how Claude Code organises its
/// per-project data under `~/.claude/projects/`.
///
/// Non-UTF-8 path components are lossily converted; the resulting slug is not
/// guaranteed to round-trip back to the original path, but is stable for any
/// given input.
fn slugify_path(path: &Path) -> String {
    path.to_string_lossy().replace('/', "-")
}

/// Return the per-project sessions directory for the current working
/// directory, creating it if it does not exist.  Falls back to the global
/// sessions root if the cwd cannot be determined.
pub fn project_sessions_dir() -> Result<PathBuf> {
    let root = sessions_dir()?;
    let dir = match std::env::current_dir() {
        Ok(cwd) => root.join(slugify_path(&cwd)),
        Err(_) => root,
    };
    std::fs::create_dir_all(&dir).with_context(|| {
        format!(
            "Cannot create project sessions directory: {}",
            dir.display()
        )
    })?;
    Ok(dir)
}

/// Save a message history to a session file.
/// Returns the path of the saved file.
pub fn save(messages: &[Message], path: &Path) -> Result<PathBuf> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(messages).context("Failed to serialise session")?;
    std::fs::write(path, &json)
        .with_context(|| format!("Cannot write session: {}", path.display()))?;
    Ok(path.to_path_buf())
}

/// Load a message history from a session file.
pub fn load(path: &Path) -> Result<Vec<Message>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("Cannot read session: {}", path.display()))?;
    let msgs: Vec<Message> = serde_json::from_str(&text)
        .with_context(|| format!("Cannot parse session: {}", path.display()))?;
    Ok(msgs)
}

/// Return the path for the "latest" session file (auto-save target) for the
/// current working directory.
pub fn latest_path() -> Result<PathBuf> {
    Ok(project_sessions_dir()?.join("latest.json"))
}

/// Find the most recently modified session file for the current working
/// directory.  Returns `None` when this project has no saved sessions —
/// sessions belonging to other projects are intentionally not considered, so
/// `--resume` never surfaces a foreign conversation.
pub fn find_latest() -> Result<Option<PathBuf>> {
    let dir = project_sessions_dir()?;
    let latest = dir.join("latest.json");
    if latest.exists() {
        return Ok(Some(latest));
    }
    // Scan for any .json file in this project's directory and pick the newest.
    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().is_none_or(|(_, t)| mtime > *t) {
                best = Some((path, mtime));
            }
        }
    }
    Ok(best.map(|(p, _)| p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Message;

    #[test]
    fn save_and_load_round_trip() {
        let dir = std::env::temp_dir().join("shio_session_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.json");

        let msgs = vec![
            Message::system("be helpful"),
            Message::user("hello"),
            Message::assistant("hi there"),
        ];
        save(&msgs, &path).unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].role, "system");
        assert_eq!(loaded[1].role, "user");
        assert_eq!(loaded[1].text_content(), Some("hello"));
        assert_eq!(loaded[2].role, "assistant");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_nonexistent_returns_error() {
        assert!(load(Path::new("/nonexistent/session.json")).is_err());
    }

    #[test]
    fn sessions_dir_creates_directory() {
        let dir = sessions_dir().unwrap();
        assert!(dir.is_dir());
    }

    #[test]
    fn slugify_path_replaces_slashes_with_dashes() {
        let p = Path::new("/home/yhh/MyProjects/ShioRamen");
        assert_eq!(slugify_path(p), "-home-yhh-MyProjects-ShioRamen");
    }

    #[test]
    fn slugify_path_preserves_dots_and_dashes() {
        let p = Path::new("/tmp/foo.bar-baz/qux");
        assert_eq!(slugify_path(p), "-tmp-foo.bar-baz-qux");
    }

    #[test]
    fn slugify_path_handles_relative_paths() {
        let p = Path::new("foo/bar");
        assert_eq!(slugify_path(p), "foo-bar");
    }

    #[test]
    fn project_sessions_dir_creates_per_cwd_subdirectory() {
        let dir = project_sessions_dir().unwrap();
        assert!(dir.is_dir());
        // Must live underneath the global sessions root.
        assert!(dir.starts_with(sessions_dir().unwrap()));
        // And must include the slugified cwd as the leaf.
        let cwd = std::env::current_dir().unwrap();
        let expected_leaf = slugify_path(&cwd);
        assert_eq!(
            dir.file_name().and_then(|n| n.to_str()),
            Some(expected_leaf.as_str())
        );
    }
}
