// SPDX-License-Identifier: GPL-3.0-or-later

use super::App;
use super::input::split_path;

pub(super) fn do_complete(app: &mut App) {
    const SLASH_CMDS: &[&str] = &[
        "/exit",
        "/quit",
        "/new",
        "/reset",
        "/clear",
        "/compact",
        "/resume",
        "/model",
        "/stats",
        "/include ",
        "/tools",
        "/skills",
        "/record",
        "/record ",
        "/stop-record",
    ];

    let typed = app.editor.input[..app.editor.cursor].to_string();

    if app.editor.comp_candidates.is_empty() {
        let candidates: Vec<String> = if let Some(path_part) = typed.strip_prefix("/include ") {
            let (dir, prefix) = split_path(path_part);
            list_path_completions(&dir, &prefix)
                .into_iter()
                .map(|c| format!("/include {c}"))
                .collect()
        } else if typed.starts_with('/') {
            let mut all: Vec<String> = SLASH_CMDS
                .iter()
                .filter(|&&c| c.starts_with(typed.as_str()))
                .map(|&c| c.to_string())
                .collect();
            for name in app.skills.keys() {
                let slash_name = format!("/{name}");
                if slash_name.starts_with(typed.as_str()) {
                    all.push(slash_name);
                }
            }
            all.sort();
            all.dedup();
            all
        } else {
            return;
        };

        if candidates.is_empty() {
            return;
        }
        app.editor.comp_candidates = candidates;
        app.editor.comp_idx = 0;
    } else {
        app.editor.comp_idx = (app.editor.comp_idx + 1) % app.editor.comp_candidates.len();
    }

    let c = app.editor.comp_candidates[app.editor.comp_idx].clone();
    app.editor.input = c;
    app.editor.cursor = app.editor.input.len();
}

fn list_path_completions(dir: &str, prefix: &str) -> Vec<String> {
    let read_path = if dir.is_empty() { "." } else { dir };
    let Ok(entries) = std::fs::read_dir(read_path) else {
        return vec![];
    };
    let mut results: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(prefix) {
                let trail = if e.path().is_dir() { "/" } else { "" };
                Some(format!("{dir}{name}{trail}"))
            } else {
                None
            }
        })
        .collect();
    results.sort();
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shio_completion_test_{}_{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn list_path_completions_filters_prefix_and_marks_dirs() {
        let dir = fresh_dir("filters");
        std::fs::write(dir.join("main.rs"), "").unwrap();
        std::fs::write(dir.join("map.rs"), "").unwrap();
        std::fs::write(dir.join("other.rs"), "").unwrap();
        std::fs::create_dir_all(dir.join("module")).unwrap();

        let mut dir_arg = dir.to_string_lossy().to_string();
        dir_arg.push('/');
        let got = list_path_completions(&dir_arg, "m");

        assert_eq!(
            got,
            vec![
                format!("{dir_arg}main.rs"),
                format!("{dir_arg}map.rs"),
                format!("{dir_arg}module/")
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_path_completions_missing_dir_returns_empty() {
        let dir = fresh_dir("missing");
        let missing = dir.join("missing/");
        let got = list_path_completions(&missing.to_string_lossy(), "");
        assert!(got.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
