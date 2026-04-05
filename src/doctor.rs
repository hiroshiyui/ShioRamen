use std::path::PathBuf;
use std::process::Command;

use crate::config::{ShioConfig, DEFAULT_HOST, DEFAULT_PORT, DEFAULT_SERVER_BIN};

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
    let server_bin = args.server_bin.clone()
        .or_else(|| cfg.server.bin.clone())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SERVER_BIN));
    let host = args.host.clone()
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

    // Summary ---------------------------------------------------------------
    println!();
    if failures == 0 {
        println!("{}{}🎉  All checks passed.{}", GREEN, BOLD, RESET);
    } else {
        println!("{}{}⚠️   {failures} check(s) failed.{}", RED, BOLD, RESET);
    }
}

fn check_binary(path: &PathBuf, failures: &mut usize) {
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

fn check_model(path: &PathBuf, failures: &mut usize) {
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

fn fmt_bytes(bytes: u64) -> String {
    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    }
}
