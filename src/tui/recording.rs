// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::EntryKind;

/// Buffered, append-only writer that mirrors chat entries to a Markdown file
/// while a `/record` session is active.
pub(super) struct Recorder {
    pub(super) path: PathBuf,
    writer: std::io::BufWriter<std::fs::File>,
}

impl Recorder {
    pub(super) fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Cannot create recording directory: {}", parent.display())
            })?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("Cannot open recording file: {}", path.display()))?;
        let mut writer = std::io::BufWriter::new(file);
        writeln!(writer, "# shio recording — started {}", unix_seconds())?;
        writer.flush()?;
        Ok(Self { path, writer })
    }

    pub(super) fn write_entry(&mut self, kind: EntryKind, text: &str) {
        let header = match kind {
            EntryKind::User => "you",
            EntryKind::Assistant => "shio",
            EntryKind::Thinking => "thinking",
            EntryKind::ToolCall => "tool call",
            EntryKind::ToolResult => "tool result",
            EntryKind::Info => "info",
            EntryKind::Error => "error",
        };
        let _ = writeln!(self.writer, "\n### {header}\n\n{text}");
        let _ = self.writer.flush();
    }
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(super) fn default_recording_path() -> Result<PathBuf> {
    let root = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("shio/recordings");
    let dir = match std::env::current_dir() {
        Ok(cwd) => root.join(cwd.to_string_lossy().replace('/', "-")),
        Err(_) => root,
    };
    Ok(dir.join(format!("rec-{}.md", unix_seconds())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_record_path(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shio_record_test_{}_{}_{}.md",
            tag,
            std::process::id(),
            unix_seconds()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn recorder_writes_header_and_entries() {
        let path = fresh_record_path("basic");
        let mut rec = Recorder::open(path.clone()).unwrap();
        rec.write_entry(EntryKind::User, "hello");
        rec.write_entry(EntryKind::Assistant, "hi there");
        drop(rec);

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("# shio recording — started "));
        assert!(body.contains("### you\n\nhello\n"));
        assert!(body.contains("### shio\n\nhi there\n"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recorder_creates_missing_parent_directory() {
        let dir = std::env::temp_dir().join(format!(
            "shio_record_nested_{}_{}",
            std::process::id(),
            unix_seconds()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("a/b/c/rec.md");
        let mut rec = Recorder::open(path.clone()).unwrap();
        rec.write_entry(EntryKind::Info, "ok");
        drop(rec);
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recorder_appends_to_existing_file() {
        let path = fresh_record_path("append");
        {
            let mut rec = Recorder::open(path.clone()).unwrap();
            rec.write_entry(EntryKind::User, "first");
        }
        {
            let mut rec = Recorder::open(path.clone()).unwrap();
            rec.write_entry(EntryKind::User, "second");
        }
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("### you\n\nfirst\n"));
        assert!(body.contains("### you\n\nsecond\n"));
        assert_eq!(
            body.matches("# shio recording — started ").count(),
            2,
            "each open should write a fresh start banner"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recorder_labels_all_entry_kinds() {
        let path = fresh_record_path("kinds");
        let mut rec = Recorder::open(path.clone()).unwrap();
        rec.write_entry(EntryKind::User, "u");
        rec.write_entry(EntryKind::Assistant, "a");
        rec.write_entry(EntryKind::Thinking, "t");
        rec.write_entry(EntryKind::ToolCall, "tc");
        rec.write_entry(EntryKind::ToolResult, "tr");
        rec.write_entry(EntryKind::Info, "i");
        rec.write_entry(EntryKind::Error, "e");
        drop(rec);

        let body = std::fs::read_to_string(&path).unwrap();
        for header in [
            "### you",
            "### shio",
            "### thinking",
            "### tool call",
            "### tool result",
            "### info",
            "### error",
        ] {
            assert!(body.contains(header), "missing header: {header}");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn default_recording_path_lives_under_shio_recordings() {
        let p = default_recording_path().unwrap();
        let s = p.to_string_lossy();
        assert!(s.contains("shio/recordings"), "got {s}");
        assert!(
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("rec-") && n.ends_with(".md"))
                .unwrap_or(false),
            "unexpected file name: {s}"
        );
    }
}
