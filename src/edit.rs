use anyhow::{Context, Result};
use similar::{ChangeTag, TextDiff};
use std::io::{self, Write};
use std::path::PathBuf;

use crate::client::{LlamaClient, Message};
use crate::config::{
    Config, ShioConfig, DEFAULT_CTX, DEFAULT_HOST, DEFAULT_NGL, DEFAULT_PORT, DEFAULT_SERVER_BIN,
    DEFAULT_TEMP,
};
use crate::server::ServerProcess;

const EDIT_SYSTEM_PROMPT: &str = "\
You are a precise code editor. When given a file and an instruction, output ONLY \
the complete updated file content. No markdown fences, no explanation, no commentary. \
Preserve indentation, line endings, and coding style. Output the raw file content \
exactly as it should be written to disk.";

#[derive(clap::Args, Debug)]
pub struct EditArgs {
    /// File to edit
    pub file: PathBuf,

    /// What to change (e.g. "add error handling to the parse function")
    pub instruction: String,

    /// Apply without asking for confirmation
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// GGUF model file [config: chat.model]
    #[arg(short, long)]
    pub model: Option<PathBuf>,

    /// llama-server binary [config: server.bin, default: ./bin/llama-server]
    #[arg(long)]
    pub server_bin: Option<PathBuf>,

    /// Server host [config: server.host, default: 127.0.0.1]
    #[arg(long)]
    pub host: Option<String>,

    /// Server port [config: server.port, default: 8080]
    #[arg(long)]
    pub port: Option<u16>,

    /// GPU layers to offload [config: server.ngl, default: 99]
    #[arg(long)]
    pub ngl: Option<i32>,

    /// Context window size in tokens [config: server.ctx, default: 8192]
    #[arg(long)]
    pub ctx: Option<u32>,

    /// KV cache quantization type for keys [config: server.cache_type_k]
    #[arg(long)]
    pub cache_type_k: Option<String>,

    /// KV cache quantization type for values [config: server.cache_type_v]
    #[arg(long)]
    pub cache_type_v: Option<String>,

    /// Enable flash attention [config: server.flash_attn]
    #[arg(long)]
    pub flash_attn: bool,

    /// Enable continuous batching [config: server.cont_batching]
    #[arg(long)]
    pub cont_batching: bool,

    /// Sampling temperature [config: chat.temperature, default: 0.7]
    #[arg(long)]
    pub temp: Option<f32>,

    /// Skip spawning llama-server; connect to an already running instance
    #[arg(long)]
    pub no_spawn: bool,
}

pub async fn run(args: &EditArgs, cfg: &ShioConfig) -> Result<()> {
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
    let temp = args.temp.or(cfg.chat.temperature).unwrap_or(DEFAULT_TEMP);

    let original = std::fs::read_to_string(&args.file)
        .with_context(|| format!("Cannot read file: {}", args.file.display()))?;

    let server = if args.no_spawn {
        ServerProcess::external(format!("http://{host}:{port}"))
    } else {
        let model = args
            .model
            .clone()
            .or_else(|| cfg.chat.model.clone())
            .ok_or_else(|| {
                anyhow::anyhow!("--model <PATH> is required (or set chat.model in shio.toml)")
            })?;
        let config = Config {
            model_path: model,
            server_bin,
            host: host.clone(),
            port,
            n_gpu_layers: args.ngl.or(cfg.server.ngl).unwrap_or(DEFAULT_NGL),
            ctx_size: args.ctx.or(cfg.server.ctx).unwrap_or(DEFAULT_CTX),
            cache_type_k: args.cache_type_k.clone().or_else(|| cfg.server.cache_type_k.clone()),
            cache_type_v: args.cache_type_v.clone().or_else(|| cfg.server.cache_type_v.clone()),
            flash_attn: args.flash_attn || cfg.server.flash_attn.unwrap_or(false),
            cont_batching: args.cont_batching || cfg.server.cont_batching.unwrap_or(false),
        };
        ServerProcess::spawn(&config).await?
    };

    let lang = args
        .file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let user_content = format!(
        "File: {}\n```{lang}\n{original}\n```\n\nInstruction: {}",
        args.file.display(),
        args.instruction,
    );

    let messages = vec![
        Message::system(EDIT_SYSTEM_PROMPT),
        Message::user(user_content),
    ];

    eprint!("Generating...");
    io::stderr().flush().ok();

    let client = LlamaClient::new(server.url.clone());
    let raw = client.chat_collect(&messages, temp).await?;
    drop(server);

    eprintln!(" done.");

    let updated = strip_fences(&raw);

    // Show diff
    let diff_lines = build_diff(&original, updated);
    if diff_lines.is_empty() {
        eprintln!("No changes.");
        return Ok(());
    }
    println!("{diff_lines}");

    // Confirm
    if !args.yes {
        eprint!("Apply? [y/N] ");
        io::stderr().flush().ok();
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !answer.trim().eq_ignore_ascii_case("y") {
            eprintln!("Aborted.");
            return Ok(());
        }
    }

    std::fs::write(&args.file, updated)
        .with_context(|| format!("Cannot write file: {}", args.file.display()))?;
    eprintln!("Saved: {}", args.file.display());
    Ok(())
}

/// Strip a single outer markdown fence if the model wrapped its output in one.
fn strip_fences(s: &str) -> &str {
    let s = s.trim();
    // Match opening fence: ``` optionally followed by a language tag and newline
    if !s.starts_with("```") {
        return s;
    }
    let after_open = &s["```".len()..];
    let body_start = match after_open.find('\n') {
        Some(i) => i + 1,
        None    => return s,
    };
    let body = &after_open[body_start..];
    // Strip closing fence
    if let Some(stripped) = body.strip_suffix("```") {
        stripped.trim_end_matches('\n')
    } else if let Some(pos) = body.rfind("\n```") {
        &body[..pos]
    } else {
        s
    }
}

fn build_diff(original: &str, updated: &str) -> String {
    const RED:   &str = "\x1b[31m";
    const GREEN: &str = "\x1b[32m";
    const DIM:   &str = "\x1b[2m";
    const RESET: &str = "\x1b[0m";

    let diff = TextDiff::from_lines(original, updated);
    let mut out = String::new();
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Delete => out.push_str(&format!("{RED}-{}{RESET}", change)),
            ChangeTag::Insert => out.push_str(&format!("{GREEN}+{}{RESET}", change)),
            ChangeTag::Equal  => out.push_str(&format!("{DIM} {}{RESET}", change)),
        }
    }
    // Only return the diff string if there were actual changes.
    if diff.ratio() >= 1.0 { String::new() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── strip_fences ─────────────────────────────────────────────────────────

    #[test]
    fn strip_fences_removes_rust_fence() {
        let input = "```rust\nfn main() {}\n```";
        assert_eq!(strip_fences(input), "fn main() {}");
    }

    #[test]
    fn strip_fences_removes_plain_fence() {
        let input = "```\nhello\nworld\n```";
        assert_eq!(strip_fences(input), "hello\nworld");
    }

    #[test]
    fn strip_fences_passes_through_plain_code() {
        let input = "fn main() {}";
        assert_eq!(strip_fences(input), "fn main() {}");
    }

    #[test]
    fn strip_fences_trims_surrounding_whitespace() {
        let input = "  \n```rust\ncode\n```\n  ";
        assert_eq!(strip_fences(input), "code");
    }

    // ── build_diff ───────────────────────────────────────────────────────────

    #[test]
    fn build_diff_identical_returns_empty() {
        assert!(build_diff("same\n", "same\n").is_empty());
    }

    #[test]
    fn build_diff_changed_returns_nonempty() {
        assert!(!build_diff("old\n", "new\n").is_empty());
    }

    #[test]
    fn build_diff_contains_added_line() {
        let d = build_diff("a\n", "a\nb\n");
        assert!(d.contains('+'));
    }

    #[test]
    fn build_diff_contains_removed_line() {
        let d = build_diff("a\nb\n", "a\n");
        assert!(d.contains('-'));
    }
}
