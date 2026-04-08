// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const MAX_FILE_BYTES: u64 = 100 * 1024; // skip files larger than 100 KB

const SOURCE_EXTS: &[&str] = &[
    "rs", "toml", "json", "yaml", "yml", "md", "py", "js", "ts", "tsx", "jsx", "mjs", "cjs", "go",
    "c", "cpp", "cc", "h", "hpp", "java", "kt", "swift", "rb", "php", "sh", "bash", "zsh", "fish",
    "sql", "html", "css", "scss", "txt", "env", "lock",
];

const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".cargo",
    "dist",
    "build",
    "__pycache__",
    ".venv",
    "venv",
    "vendor",
    ".next",
    ".nuxt",
];

/// Collect `(path, content)` pairs from a file or a directory tree.
/// Directories are walked recursively; only source-like text files are kept.
pub fn collect(root: &Path) -> Result<Vec<(PathBuf, String)>> {
    if root.is_file() {
        // Apply the same size cap as collect_dir to prevent accidentally loading
        // a giant binary or log file into the model's context window.
        let size = root.metadata().map(|m| m.len()).unwrap_or(0);
        if size > MAX_FILE_BYTES {
            anyhow::bail!(
                "File too large to include: {} ({} KB, limit is {} KB)",
                root.display(),
                size / 1024,
                MAX_FILE_BYTES / 1024,
            );
        }
        let content = std::fs::read_to_string(root)
            .with_context(|| format!("Cannot read file: {}", root.display()))?;
        return Ok(vec![(root.to_path_buf(), content)]);
    }

    if !root.is_dir() {
        anyhow::bail!("Path not found: {}", root.display());
    }

    let mut files = Vec::new();
    collect_dir(root, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

/// Maximum directory recursion depth to prevent runaway traversal via symlink
/// loops or extremely deep trees.
const MAX_DEPTH: usize = 20;

fn collect_dir(dir: &Path, out: &mut Vec<(PathBuf, String)>) -> Result<()> {
    collect_dir_depth(dir, out, 0)
}

fn collect_dir_depth(dir: &Path, out: &mut Vec<(PathBuf, String)>, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("Cannot read directory: {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Use symlink_metadata to detect symlinks without following them.
        let meta = match std::fs::symlink_metadata(entry.path()) {
            Ok(m) => m,
            Err(_) => continue,
        };

        if meta.file_type().is_symlink() {
            continue; // skip symlinks to avoid escaping the project tree
        }

        if meta.is_dir() {
            if !SKIP_DIRS.contains(&name_str.as_ref()) {
                collect_dir_depth(&path, out, depth + 1)?;
            }
            continue;
        }

        if !meta.is_file() {
            continue;
        }
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !SOURCE_EXTS.contains(&ext) {
            continue;
        }

        if let Ok(content) = std::fs::read_to_string(&path) {
            out.push((path, content));
        }
    }
    Ok(())
}

/// Format collected files as fenced code blocks ready to inject into a message.
pub fn format_as_blocks(files: &[(PathBuf, String)]) -> String {
    let mut s = String::new();
    for (path, content) in files {
        let lang = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        s.push_str(&format!(
            "File: {}\n```{lang}\n{content}\n```\n\n",
            path.display()
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_tree(dir: &Path) {
        fs::write(dir.join("main.rs"), "fn main() {}").unwrap();
        fs::write(dir.join("README.md"), "# hi").unwrap();
        fs::write(dir.join("data.bin"), &[0u8, 1, 2, 3]).unwrap(); // no matching ext
        let sub = dir.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("util.rs"), "pub fn util() {}").unwrap();
        let target = dir.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("skip.rs"), "// should be skipped").unwrap();
    }

    #[test]
    fn collects_source_files_and_skips_target() {
        let dir = std::env::temp_dir().join("shio_ctx_test");
        fs::create_dir_all(&dir).unwrap();
        make_tree(&dir);

        let files = collect(&dir).unwrap();
        let names: Vec<&str> = files
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_str().unwrap())
            .collect();

        assert!(names.contains(&"main.rs"));
        assert!(names.contains(&"README.md"));
        assert!(names.contains(&"util.rs"));
        assert!(!names.contains(&"data.bin")); // wrong extension
        assert!(!names.contains(&"skip.rs")); // inside target/

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn collects_single_file() {
        let path = std::env::temp_dir().join("shio_ctx_single.rs");
        fs::write(&path, "fn main() {}").unwrap();
        let files = collect(&path).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].1, "fn main() {}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn nonexistent_path_returns_error() {
        assert!(collect(Path::new("/nonexistent/shio_ctx_path")).is_err());
    }

    #[test]
    fn format_as_blocks_produces_fenced_code() {
        let files = vec![(PathBuf::from("foo.rs"), "fn f() {}".to_string())];
        let out = format_as_blocks(&files);
        assert!(out.contains("File: foo.rs"));
        assert!(out.contains("```rs"));
        assert!(out.contains("fn f() {}"));
    }

    #[test]
    fn collect_single_file_over_limit_returns_error() {
        let path = std::env::temp_dir().join("shio_ctx_big.rs");
        // Write just over the 100 KB limit.
        fs::write(&path, vec![b'x'; (MAX_FILE_BYTES + 1) as usize]).unwrap();
        let result = collect(&path);
        assert!(result.is_err(), "expected error for oversized file");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn collect_dir_silently_skips_oversized_files() {
        let dir = std::env::temp_dir().join("shio_ctx_big_dir");
        fs::create_dir_all(&dir).unwrap();
        let small = dir.join("small.rs");
        let big = dir.join("big.rs");
        fs::write(&small, "fn small() {}").unwrap();
        fs::write(&big, vec![b'x'; (MAX_FILE_BYTES + 1) as usize]).unwrap();
        let files = collect(&dir).unwrap();
        let names: Vec<&str> = files
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert!(names.contains(&"small.rs"), "small file should be included");
        assert!(
            !names.contains(&"big.rs"),
            "oversized file should be skipped"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_empty_directory_returns_empty_vec() {
        let dir = std::env::temp_dir().join("shio_ctx_empty_dir");
        fs::create_dir_all(&dir).unwrap();
        // Make sure it's empty.
        for entry in fs::read_dir(&dir).unwrap() {
            let _ = fs::remove_file(entry.unwrap().path());
        }
        let files = collect(&dir).unwrap();
        assert!(files.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
