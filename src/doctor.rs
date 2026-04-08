// SPDX-License-Identifier: GPL-3.0-or-later
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{DEFAULT_HOST, DEFAULT_PORT, DEFAULT_SERVER_BIN, ShioConfig};
use crate::ruby::vm::ShioVm;

#[derive(clap::Args, Debug)]
pub struct DoctorArgs {
    /// GGUF model file to verify [config: chat.model]
    #[arg(short, long)]
    pub model: Option<PathBuf>,

    /// llama-server binary to check [config: server.bin, default: ./bin/llama-server]
    #[arg(long)]
    pub server_bin: Option<PathBuf>,

    /// Server host to probe [config: server.host, default: 127.0.0.1]
    #[arg(long)]
    pub host: Option<String>,

    /// Server port to probe [config: server.port, default: 8080]
    #[arg(long)]
    pub port: Option<u16>,
}

const RESET: &str = "\x1b[0m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";
const BOLD: &str = "\x1b[1m";

macro_rules! ok {
    ($($arg:tt)*) => {
        println!("  {}✅  {}{}", GREEN, format_args!($($arg)*), RESET)
    };
}
macro_rules! fail {
    ($($arg:tt)*) => {
        println!("  {}{}❌  {}{}", RED, BOLD, format_args!($($arg)*), RESET)
    };
}
macro_rules! info {
    ($($arg:tt)*) => {
        println!("  {}🔍  {}{}", CYAN, format_args!($($arg)*), RESET)
    };
}

pub async fn run(args: &DoctorArgs, cfg: &ShioConfig) {
    // Resolve CLI > config > hardcoded default
    let server_bin = args
        .server_bin
        .clone()
        .or_else(|| cfg.server.bin.clone())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SERVER_BIN));
    let host = args
        .host
        .clone()
        .or_else(|| cfg.server.host.clone())
        .unwrap_or_else(|| DEFAULT_HOST.to_string());
    let port = args.port.or(cfg.server.port).unwrap_or(DEFAULT_PORT);
    let model = args.model.clone().or_else(|| cfg.chat.model.clone());

    println!("🩺 {}{}Checking components…{}\n", BOLD, CYAN, RESET);
    let mut failures = 0usize;

    // 1. llama-server binary ------------------------------------------------
    check_binary(&server_bin, &mut failures);

    // 2. Model file ---------------------------------------------------------
    if let Some(ref m) = model {
        check_model(m, &mut failures);
    }

    // 3. GPU (informational) ------------------------------------------------
    check_gpu();

    // 4. Server health ------------------------------------------------------
    let url = format!("http://{host}:{port}");
    check_server(&url, &mut failures).await;

    // 5. mRuby VM + tools ---------------------------------------------------
    check_vm(&mut failures);

    // Summary ---------------------------------------------------------------
    println!();
    if failures == 0 {
        println!("{}{}🎉  All checks passed.{}", GREEN, BOLD, RESET);
    } else {
        println!("{}{}⚠️   {failures} check(s) failed.{}", RED, BOLD, RESET);
    }
}

fn check_binary(path: &Path, failures: &mut usize) {
    if !path.is_file() {
        fail!("llama-server binary: {} (not found)", path.display());
        *failures += 1;
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let executable = std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
        if executable {
            ok!("llama-server binary: {}", path.display());
        } else {
            fail!("llama-server binary: {} (not executable)", path.display());
            *failures += 1;
        }
    }

    #[cfg(not(unix))]
    ok!("llama-server binary: {}", path.display());
}

fn check_model(path: &Path, failures: &mut usize) {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => {
            ok!("Model file: {} ({})", path.display(), fmt_bytes(meta.len()));
        }
        Ok(_) => {
            fail!("Model file: {} (not a file)", path.display());
            *failures += 1;
        }
        Err(e) => {
            fail!("Model file: {} ({e})", path.display());
            *failures += 1;
        }
    }
}

fn check_gpu() {
    match Command::new("nvidia-smi")
        .args(["--query-gpu=name,memory.total", "--format=csv,noheader"])
        .output()
    {
        Ok(out) if out.status.success() => {
            let gpu_info = String::from_utf8_lossy(&out.stdout)
                .lines()
                .collect::<Vec<_>>()
                .join(", ");
            info!("GPU (NVIDIA): {gpu_info}");
        }
        _ => {
            info!("GPU (NVIDIA): not detected");
        }
    }
}

async fn check_server(url: &str, failures: &mut usize) {
    let healthy = reqwest::get(format!("{url}/health"))
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);

    if healthy {
        ok!("Server health: {url}");
    } else {
        fail!("Server health: {url} (not reachable — run `shio serve` first)");
        *failures += 1;
    }
}

/// Number of built-in tools that must be registered after `ShioVm::new()`.
const EXPECTED_BUILTIN_TOOLS: usize = 22;

fn check_vm(failures: &mut usize) {
    match ShioVm::new() {
        Ok(mut vm) => match vm.tool_schemas() {
            Ok(schemas) => {
                let count = schemas.len();
                if count >= EXPECTED_BUILTIN_TOOLS {
                    ok!(
                        "mRuby VM: initialised, {count} tools registered (expected ≥ {EXPECTED_BUILTIN_TOOLS})"
                    );
                } else {
                    fail!(
                        "mRuby VM: only {count} tools registered (expected ≥ {EXPECTED_BUILTIN_TOOLS})"
                    );
                    *failures += 1;
                }
            }
            Err(e) => {
                fail!("mRuby VM: initialised but tool_schemas() failed: {e}");
                *failures += 1;
            }
        },
        Err(e) => {
            fail!("mRuby VM: failed to initialise: {e}");
            *failures += 1;
        }
    }
}

fn fmt_bytes(bytes: u64) -> String {
    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── check_binary ─────────────────────────────────────────────────────────

    #[test]
    fn check_binary_nonexistent_increments_failures() {
        let mut failures = 0;
        check_binary(&PathBuf::from("/nonexistent/llama-server"), &mut failures);
        assert_eq!(failures, 1);
    }

    #[test]
    fn check_binary_existing_executable_no_failure() {
        // Use the test binary itself — guaranteed to exist and be executable.
        let bin = std::env::current_exe().unwrap();
        let mut failures = 0;
        check_binary(&bin, &mut failures);
        assert_eq!(failures, 0);
    }

    // ── check_model ──────────────────────────────────────────────────────────

    #[test]
    fn check_model_nonexistent_increments_failures() {
        let mut failures = 0;
        check_model(&PathBuf::from("/nonexistent/model.gguf"), &mut failures);
        assert_eq!(failures, 1);
    }

    #[test]
    fn check_model_existing_file_no_failure() {
        let path = std::env::temp_dir().join("shio_test_model.gguf");
        std::fs::write(&path, b"fake").unwrap();
        let mut failures = 0;
        check_model(&path, &mut failures);
        assert_eq!(failures, 0);
        let _ = std::fs::remove_file(&path);
    }

    // ── fmt_bytes ────────────────────────────────────────────────────────────

    #[test]
    fn fmt_bytes_below_gb_uses_mb() {
        assert_eq!(fmt_bytes(1024 * 1024), "1 MB");
        assert_eq!(fmt_bytes(512 * 1024 * 1024), "512 MB");
    }

    #[test]
    fn fmt_bytes_at_or_above_gb_uses_gb() {
        assert_eq!(fmt_bytes(1024 * 1024 * 1024), "1.0 GB");
        assert_eq!(fmt_bytes(4 * 1024 * 1024 * 1024), "4.0 GB");
    }

    #[test]
    fn fmt_bytes_boundary() {
        assert!(fmt_bytes(1024 * 1024 * 1024 - 1).ends_with("MB"));
        assert!(fmt_bytes(1024 * 1024 * 1024).ends_with("GB"));
    }

    // ── check_vm ─────────────────────────────────────────────────────────────

    #[test]
    fn check_vm_initialises_and_registers_tools() {
        let mut failures = 0;
        check_vm(&mut failures);
        assert_eq!(failures, 0, "VM should initialise and register all tools");
    }

    #[test]
    fn vm_registers_expected_builtin_tools() {
        let mut vm = ShioVm::new().expect("ShioVm should initialise");
        let schemas = vm.tool_schemas().expect("tool_schemas should succeed");
        let names: Vec<&str> = schemas.iter().map(|(n, _, _)| n.as_str()).collect();

        // Verify all 22 built-in tools are present.
        for expected in [
            "get_working_directory",
            "create_directory",
            "read_file",
            "list_directory",
            "delete_file",
            "move_file",
            "write_file",
            "append_file",
            "insert_after_line",
            "read_file_range",
            "read_many_files",
            "search_files",
            "grep_files",
            "save_memory",
            "write_todos",
            "fetch_url",
            "web_search",
            "patch_file",
            "run_shell",
            "lsp",
            "enter_plan_mode",
            "exit_plan_mode",
        ] {
            assert!(
                names.contains(&expected),
                "missing built-in tool: {expected}"
            );
        }
    }

    #[test]
    fn vm_tool_schemas_have_descriptions_and_parameters() {
        let mut vm = ShioVm::new().expect("ShioVm should initialise");
        let schemas = vm.tool_schemas().expect("tool_schemas should succeed");
        for (name, desc, params) in &schemas {
            assert!(!desc.is_empty(), "tool {name} has empty description");
            assert!(
                params.is_object(),
                "tool {name} parameters is not an object"
            );
        }
    }
}
