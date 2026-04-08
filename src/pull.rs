// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::{DEFAULT_MODELS_DIR, ShioConfig};

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
    let models_dir = args
        .models_dir
        .clone()
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
        // Rewrite HuggingFace blob page URLs to direct download URLs.
        // /blob/main/ is the web viewer; /resolve/main/ is the actual file.
        let url = if source.contains("huggingface.co") && source.contains("/blob/") {
            source.replace("/blob/", "/resolve/")
        } else {
            source.to_string()
        };
        let filename = url
            .split('/')
            .next_back()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Cannot determine filename from URL: {url}"))?
            .to_string();
        Ok((url, filename))
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
                anyhow::anyhow!("Expected owner/repo/filename.gguf or a URL, got: {source}")
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

    // Download into a .part file first; rename on success so an interrupted
    // download doesn't leave a partial file that looks complete.
    let part = dest.with_extension("gguf.part");
    let total = response.content_length();
    let mut file = std::fs::File::create(&part)
        .with_context(|| format!("Cannot create file: {}", part.display()))?;
    let mut received: u64 = 0;
    let mut stream = response.bytes_stream();

    let result: Result<()> = async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("Stream read error")?;
            file.write_all(&chunk).context("File write error")?;
            received += chunk.len() as u64;
            render_progress(received, total);
        }
        Ok(())
    }
    .await;

    if let Err(e) = result {
        // Clean up the partial file on failure.
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }

    std::fs::rename(&part, dest)
        .with_context(|| format!("Cannot rename {} → {}", part.display(), dest.display()))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── run: early return when file already exists ───────────────────────────

    #[tokio::test]
    async fn run_skips_download_when_file_exists() {
        let dir = std::env::temp_dir();
        let existing = dir.join("already_exists.gguf");
        std::fs::write(&existing, b"").unwrap();

        let args = PullArgs {
            source: "https://example.com/already_exists.gguf".to_string(),
            models_dir: Some(dir.clone()),
        };
        let cfg = crate::config::ShioConfig::default();
        // Must return Ok without attempting any network request.
        run(&args, &cfg).await.unwrap();

        let _ = std::fs::remove_file(&existing);
    }

    // ── resolve_source ───────────────────────────────────────────────────────

    #[test]
    fn hf_path_expands_to_correct_url() {
        let (url, name) = resolve_source("owner/repo/model.gguf").unwrap();
        assert_eq!(
            url,
            "https://huggingface.co/owner/repo/resolve/main/model.gguf"
        );
        assert_eq!(name, "model.gguf");
    }

    #[test]
    fn hf_path_with_subdir_strips_subdir_from_local_name() {
        let (url, name) = resolve_source("owner/repo/subdir/model.gguf").unwrap();
        assert_eq!(
            url,
            "https://huggingface.co/owner/repo/resolve/main/subdir/model.gguf"
        );
        assert_eq!(name, "model.gguf");
    }

    #[test]
    fn hf_blob_url_rewritten_to_resolve() {
        let blob = "https://huggingface.co/ggml-org/gemma-4-E4B-it-GGUF/blob/main/gemma-4-e4b-it-Q8_0.gguf";
        let (url, name) = resolve_source(blob).unwrap();
        assert_eq!(
            url,
            "https://huggingface.co/ggml-org/gemma-4-E4B-it-GGUF/resolve/main/gemma-4-e4b-it-Q8_0.gguf"
        );
        assert_eq!(name, "gemma-4-e4b-it-Q8_0.gguf");
    }

    #[test]
    fn direct_url_passes_through_unchanged() {
        let src = "https://example.com/path/to/model.gguf";
        let (url, name) = resolve_source(src).unwrap();
        assert_eq!(url, src);
        assert_eq!(name, "model.gguf");
    }

    #[test]
    fn hf_path_missing_filename_returns_error() {
        assert!(resolve_source("owner/repo").is_err());
    }

    #[test]
    fn single_segment_returns_error() {
        assert!(resolve_source("just-one-part").is_err());
    }

    // ── fmt_bytes ────────────────────────────────────────────────────────────

    #[test]
    fn fmt_bytes_below_gib_uses_mib() {
        assert_eq!(fmt_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(fmt_bytes(512 * 1024 * 1024), "512.0 MiB");
    }

    #[test]
    fn fmt_bytes_at_or_above_gib_uses_gib() {
        assert_eq!(fmt_bytes(1024 * 1024 * 1024), "1.00 GiB");
        assert_eq!(fmt_bytes(4 * 1024 * 1024 * 1024), "4.00 GiB");
    }

    #[test]
    fn fmt_bytes_boundary() {
        assert!(fmt_bytes(1024 * 1024 * 1024 - 1).ends_with("MiB"));
        assert!(fmt_bytes(1024 * 1024 * 1024).ends_with("GiB"));
    }
}
