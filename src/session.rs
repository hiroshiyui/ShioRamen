// SPDX-License-Identifier: GPL-3.0-or-later
//! Session persistence — auto-save conversations to disk and reload them.
//!
//! Sessions are stored as JSON files under `~/.local/share/shio/sessions/`.
//! Each file contains a timestamped array of [`Message`]s that can be loaded
//! back into a new `ChatSession`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::client::Message;

/// Return the sessions directory, creating it if it does not exist.
pub fn sessions_dir() -> Result<PathBuf> {
    let dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("shio/sessions");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Cannot create sessions directory: {}", dir.display()))?;
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

/// Return the path for the "latest" session file (auto-save target).
pub fn latest_path() -> Result<PathBuf> {
    Ok(sessions_dir()?.join("latest.json"))
}

/// Find the most recently modified session file in the sessions directory.
/// Falls back to `latest.json` if it exists.
pub fn find_latest() -> Result<Option<PathBuf>> {
    let dir = sessions_dir()?;
    let latest = dir.join("latest.json");
    if latest.exists() {
        return Ok(Some(latest));
    }
    // Scan for any .json file and pick the newest.
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
}
