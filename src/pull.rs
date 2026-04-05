use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::{ShioConfig, DEFAULT_MODELS_DIR};

const HF_BASE: &str = "https://huggingface.co";

#[derive(clap::Args, Debug)]
pub struct PullArgs {
    /// HuggingFace path (owner/repo/filename.gguf) or a direct HTTPS URL
    pub source: String,

    /// Directory to save downloaded models [config: paths.models_dir, default: ./models]
    #[arg(long)]
    pub models_dir: Option<PathBuf>,
}

pub async fn run(args: &PullArgs, cfg: &ShioConfig) -> Result<()> {
    let models_dir = args.models_dir.clone()
        .or_else(|| cfg.paths.models_dir.clone())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_MODELS_DIR));

    let (url, filename) = resolve_source(&args.source)?;
    let dest = models_dir.join(&filename);

    if dest.exists() {
        println!("✅ Already downloaded: {}", dest.display());
        return Ok(());
    }

    std::fs::create_dir_all(&models_dir)
        .with_context(|| format!("Cannot create directory: {}", models_dir.display()))?;

    println!("📥 Downloading {filename}");
    println!("   → {}", dest.display());

    println!();

    download(&url, &dest).await?;

    println!("\n\n✅ Saved to {}", dest.display());
    Ok(())
}

/// Resolve a HuggingFace shorthand or raw URL into (url, local_filename).
fn resolve_source(source: &str) -> Result<(String, String)> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let filename = source
            .split('/')
            .last()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Cannot determine filename from URL: {source}"))?
            .to_string();
        Ok((source.to_string(), filename))
    } else {
        // owner/repo/filename  (filename may itself contain '/' for repo subdirs)
        let (prefix, filename) = source
            .splitn(3, '/')
            .collect::<Vec<_>>()
            .chunks(3)
            .next()
            .and_then(|parts| {
                if parts.len() == 3 {
                    Some((format!("{}/{}", parts[0], parts[1]), parts[2].to_string()))
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Expected owner/repo/filename.gguf or a URL, got: {source}"
                )
            })?;

        // Use only the basename for the local file (strip any subfolder path inside the repo)
        let local_name = Path::new(&filename)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(filename.clone());

        let url = format!("{HF_BASE}/{prefix}/resolve/main/{filename}");
        Ok((url, local_name))
    }
}

async fn download(url: &str, dest: &Path) -> Result<()> {
    let response = reqwest::get(url)
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("Server error from {url}"))?;

    let total = response.content_length();
    let mut file = std::fs::File::create(dest)
        .with_context(|| format!("Cannot create file: {}", dest.display()))?;
    let mut received: u64 = 0;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Stream read error")?;
        file.write_all(&chunk).context("File write error")?;
        received += chunk.len() as u64;
        render_progress(received, total);
    }

    Ok(())
}

fn render_progress(received: u64, total: Option<u64>) {
    const BAR: usize = 35;
    const CYAN: &str = "\x1b[36m";
    const GREEN: &str = "\x1b[32m";
    const RESET: &str = "\x1b[0m";

    match total {
        Some(total) => {
            let pct = (received as f64 / total as f64).clamp(0.0, 1.0);
            let filled = (BAR as f64 * pct) as usize;
            let bar = format!(
                "{}{}{}{}",
                GREEN,
                "█".repeat(filled),
                "░".repeat(BAR - filled),
                RESET,
            );
            print!(
                "\r  {bar} {CYAN}{:5.1}%{RESET}  {} / {}  ",
                pct * 100.0,
                fmt_bytes(received),
                fmt_bytes(total),
            );
        }
        None => {
            print!("\r  {CYAN}{}{RESET}  ", fmt_bytes(received));
        }
    }
    std::io::stdout().flush().ok();
}

fn fmt_bytes(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    }
}
