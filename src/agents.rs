// SPDX-License-Identifier: GPL-3.0-or-later
//! AGENTS.md support — load project-specific AI instructions.
//!
//! AGENTS.md (<https://agents.md/>) is a Markdown file placed in a project's
//! root (and optionally in subdirectories) that provides context and
//! conventions for AI coding agents.
//!
//! This module walks the directory tree upward from a starting path, stopping
//! at the nearest `.git` root, and collects any AGENTS.md files found along
//! the way.  Multiple files are concatenated root-first so that subdirectory
//! files can add to (or override) root-level instructions.

use std::path::{Path, PathBuf};

/// Walk upward from `start`, returning paths of every `AGENTS.md` that exists,
/// ordered from the outermost (git root) to the innermost (closest to `start`).
///
/// The walk stops when a `.git` directory is found in the current directory
/// (that directory is treated as the project root) or when the filesystem root
/// is reached.
pub fn find_agents_files(start: &Path) -> Vec<PathBuf> {
    // Normalise to a directory.
    let start = if start.is_file() {
        start.parent().unwrap_or(start)
    } else {
        start
    };

    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut current = start.to_path_buf();

    loop {
        dirs.push(current.clone());
        let is_git_root = current.join(".git").exists();
        match current.parent() {
            Some(parent) if parent != current && !is_git_root => {
                current = parent.to_path_buf();
            }
            _ => break,
        }
    }

    // `dirs` is cwd-first; reverse so root (outermost) comes first.
    dirs.reverse();

    dirs.into_iter()
        .map(|d| d.join("AGENTS.md"))
        .filter(|p| p.exists())
        .collect()
}

/// Load all applicable AGENTS.md files for `cwd`, returning the combined
/// content or `None` if no files were found or all were empty.
///
/// Multiple files are separated by a blank line.
pub fn load(cwd: &Path) -> Option<String> {
    let files = find_agents_files(cwd);
    if files.is_empty() {
        return None;
    }

    let parts: Vec<String> = files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// Return `base` prepended with AGENTS.md project instructions (if any).
///
/// The result format is:
/// ```text
/// Project instructions (from AGENTS.md):
///
/// <agents content>
///
/// ---
///
/// <base system prompt>
/// ```
pub fn augment_system_prompt(base: &str, cwd: &Path) -> String {
    match load(cwd) {
        Some(content) => {
            format!("Project instructions (from AGENTS.md):\n\n{content}\n\n---\n\n{base}")
        }
        None => base.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Create a uniquely-named temp directory and return its path.
    fn tmp_dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("shio_agents_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    // ── find_agents_files ─────────────────────────────────────────────────────

    #[test]
    fn find_agents_files_returns_empty_when_none_present() {
        let dir = tmp_dir("empty");
        // Create a .git to stop the walk here.
        fs::create_dir_all(dir.join(".git")).unwrap();
        assert!(find_agents_files(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_agents_files_finds_file_in_start_dir() {
        let dir = tmp_dir("single");
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join("AGENTS.md"), "# agents").unwrap();
        let files = find_agents_files(&dir);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], dir.join("AGENTS.md"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_agents_files_walk_stops_at_git_root() {
        // root/.git  root/AGENTS.md  root/sub/
        // Walking from sub should find root/AGENTS.md but not go above root.
        let root = tmp_dir("gitstop");
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("AGENTS.md"), "# Root").unwrap();

        let files = find_agents_files(&sub);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], root.join("AGENTS.md"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn find_agents_files_root_first_ordering() {
        // root/.git  root/AGENTS.md  root/sub/AGENTS.md
        // Walking from sub should return [root/AGENTS.md, sub/AGENTS.md].
        let root = tmp_dir("ordering");
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("AGENTS.md"), "# Root").unwrap();
        fs::write(sub.join("AGENTS.md"), "# Sub").unwrap();

        let files = find_agents_files(&sub);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0], root.join("AGENTS.md"));
        assert_eq!(files[1], sub.join("AGENTS.md"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn find_agents_files_accepts_file_path_uses_parent() {
        let dir = tmp_dir("filepath");
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join("AGENTS.md"), "# agents").unwrap();
        let src = dir.join("main.rs");
        fs::write(&src, "fn main() {}").unwrap();

        // Passing a file path — should use its parent directory.
        let files = find_agents_files(&src);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], dir.join("AGENTS.md"));
        let _ = fs::remove_dir_all(&dir);
    }

    // ── load ──────────────────────────────────────────────────────────────────

    #[test]
    fn load_returns_none_when_no_files() {
        let dir = tmp_dir("load_none");
        fs::create_dir_all(dir.join(".git")).unwrap();
        assert!(load(&dir).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_returns_content() {
        let dir = tmp_dir("load_content");
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join("AGENTS.md"), "# Build\nrun `cargo test`\n").unwrap();
        let content = load(&dir).unwrap();
        assert!(content.contains("Build"));
        assert!(content.contains("cargo test"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_concatenates_multiple_files() {
        let root = tmp_dir("load_multi");
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("AGENTS.md"), "Root instructions.").unwrap();
        fs::write(sub.join("AGENTS.md"), "Sub instructions.").unwrap();

        let content = load(&sub).unwrap();
        assert!(content.contains("Root instructions."));
        assert!(content.contains("Sub instructions."));
        // Root appears before sub.
        let root_pos = content.find("Root").unwrap();
        let sub_pos = content.find("Sub").unwrap();
        assert!(root_pos < sub_pos);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn load_returns_none_for_empty_file() {
        let dir = tmp_dir("load_empty");
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join("AGENTS.md"), "   \n  \n").unwrap();
        assert!(load(&dir).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    // ── augment_system_prompt ─────────────────────────────────────────────────

    #[test]
    fn augment_returns_base_when_no_agents_md() {
        let dir = tmp_dir("aug_none");
        fs::create_dir_all(dir.join(".git")).unwrap();
        let result = augment_system_prompt("Be helpful.", &dir);
        assert_eq!(result, "Be helpful.");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn augment_prepends_agents_content() {
        let dir = tmp_dir("aug_prepend");
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join("AGENTS.md"), "Always use snake_case.").unwrap();
        let result = augment_system_prompt("Be helpful.", &dir);
        assert!(result.starts_with("Project instructions"));
        assert!(result.contains("snake_case"));
        assert!(result.contains("Be helpful."));
        // Agents content appears before base prompt.
        let agents_pos = result.find("snake_case").unwrap();
        let base_pos = result.find("Be helpful.").unwrap();
        assert!(agents_pos < base_pos);
        let _ = fs::remove_dir_all(&dir);
    }
}
