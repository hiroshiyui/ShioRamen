// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use std::path::Path;
use tempfile::{NamedTempFile, TempDir};

pub(crate) fn executor(confirm_writes: bool, confirm_shell: bool) -> ToolExecutor {
    ToolExecutor {
        confirm_writes,
        confirm_shell,
        ..ToolExecutor::try_new().expect("test ToolExecutor VM init failed")
    }
}

pub(crate) fn call_tool(ex: &ToolExecutor, name: &str, args: serde_json::Value) -> String {
    ex.vm.lock().unwrap().call_tool(name, &args.to_string())
}

pub(crate) fn call_tool_raw(ex: &ToolExecutor, name: &str, args: &str) -> String {
    ex.vm.lock().unwrap().call_tool(name, args)
}

pub(crate) fn path_str(path: &Path) -> &str {
    path.to_str().expect("test path should be valid UTF-8")
}

pub(crate) fn temp_file(contents: &str) -> NamedTempFile {
    let file = NamedTempFile::new().expect("create temp file");
    std::fs::write(file.path(), contents).expect("write temp file");
    file
}

pub(crate) fn temp_path(file_name: &str) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().expect("create temp dir");
    let path = dir.path().join(file_name);
    (dir, path)
}
