// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Context, Result};
use similar::{ChangeTag, TextDiff};
use std::io::{self, Write};
use std::path::PathBuf;

use crate::ServerArgs;
use crate::agents;
use crate::client::{LlamaClient, Message, SamplingParams};
use crate::config::{DEFAULT_TEMP, ShioConfig};

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

    #[command(flatten)]
    pub server: ServerArgs,

    /// Sampling temperature [config: chat.temperature, default: 0.7]
    #[arg(long)]
    pub temp: Option<f32>,

    /// Top-p (nucleus) sampling [config: chat.top_p]
    #[arg(long)]
    pub top_p: Option<f32>,

    /// Repetition penalty (> 1.0 discourages repetition) [config: chat.repeat_penalty]
    #[arg(long)]
    pub repeat_penalty: Option<f32>,

    /// Skip spawning llama-server; connect to an already running instance
    #[arg(long)]
    pub no_spawn: bool,
}

pub async fn run(args: &EditArgs, cfg: &ShioConfig) -> Result<()> {
    let sampling = SamplingParams {
        temperature: args.temp.or(cfg.chat.temperature).unwrap_or(DEFAULT_TEMP),
        top_p: args.top_p.or(cfg.chat.top_p),
        repeat_penalty: args.repeat_penalty.or(cfg.chat.repeat_penalty),
    };

    let original = tokio::fs::read_to_string(&args.file)
        .await
        .with_context(|| format!("Cannot read file: {}", args.file.display()))?;

    let server = args
        .server
        .spawn_or_connect(args.no_spawn, args.model.clone(), cfg)
        .await?;

    // Build system prompt: fixed raw-output instruction, optionally extended
    // with project conventions from AGENTS.md.
    let edit_dir = args.file.parent().unwrap_or(std::path::Path::new("."));
    let system_prompt = match agents::load(edit_dir) {
        Some(content) => {
            format!("{EDIT_SYSTEM_PROMPT}\n\nProject conventions (from AGENTS.md):\n\n{content}")
        }
        None => EDIT_SYSTEM_PROMPT.to_string(),
    };

    let lang = args.file.extension().and_then(|e| e.to_str()).unwrap_or("");
    let user_content = format!(
        "File: {}\n```{lang}\n{original}\n```\n\nInstruction: {}",
        args.file.display(),
        args.instruction,
    );

    let messages = vec![Message::system(system_prompt), Message::user(user_content)];

    eprint!("Generating...");
    io::stderr().flush().ok();

    let client = LlamaClient::new(server.url.clone());
    let raw = client.chat_collect(&messages, sampling).await?;
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

    // Write a backup before overwriting so the user can recover.
    let bak = args.file.with_extension({
        let mut ext = args.file.extension().unwrap_or_default().to_os_string();
        ext.push(".bak");
        ext
    });
    std::fs::write(&bak, &original)
        .with_context(|| format!("Cannot write backup: {}", bak.display()))?;

    std::fs::write(&args.file, updated)
        .with_context(|| format!("Cannot write file: {}", args.file.display()))?;
    eprintln!("Saved: {} (backup: {})", args.file.display(), bak.display());
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
        None => return s,
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
    const RED: &str = "\x1b[31m";
    const GREEN: &str = "\x1b[32m";
    const DIM: &str = "\x1b[2m";
    const RESET: &str = "\x1b[0m";

    let diff = TextDiff::from_lines(original, updated);
    let mut out = String::new();
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Delete => out.push_str(&format!("{RED}-{}{RESET}", change)),
            ChangeTag::Insert => out.push_str(&format!("{GREEN}+{}{RESET}", change)),
            ChangeTag::Equal => out.push_str(&format!("{DIM} {}{RESET}", change)),
        }
    }
    // Only return the diff string if there were actual changes.
    if diff.ratio() >= 1.0 {
        String::new()
    } else {
        out
    }
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

    #[test]
    fn strip_fences_no_closing_fence_returns_original() {
        // Model forgot the closing ``` — must not mangle the content.
        let input = "```rust\nfn main() {}";
        assert_eq!(strip_fences(input), input);
    }

    #[test]
    fn strip_fences_trailing_text_after_closing_fence_is_stripped() {
        // Model appended an explanation after the closing fence.
        let input = "```rust\nfn main() {}\n```\nThis completes the edit.";
        assert_eq!(strip_fences(input), "fn main() {}");
    }

    #[test]
    fn strip_fences_empty_body_between_fences() {
        let input = "```\n```";
        assert_eq!(strip_fences(input), "");
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
