// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Result, bail};
use std::path::Path;

#[derive(clap::Args, Debug)]
pub struct InitArgs {}

const TEMPLATE: &str = r#"# shio.toml — ShioRamen configuration
# All keys are optional; uncomment and adjust what you need.

[server]
# Path to the llama-server binary.
# bin = "./bin/llama-server"

# Host and port the server listens on.
# host = "127.0.0.1"
# port = 8080

# Number of model layers to offload to the GPU (-1 = all, 0 = CPU-only).
# ngl = 99

# Context window size in tokens.
# ctx = 8192

# KV-cache quantization (reduces VRAM; "q4_0", "q8_0", or "f16").
# cache_type_k = "f16"
# cache_type_v = "f16"

# Enable flash attention (faster inference on supported hardware).
# flash_attn = false

# Enable continuous batching.
# cont_batching = false

[chat]
# Default model file used by `shio chat` and `shio ask`.
# model = "./models/your-model.gguf"

# Sampling temperature (0.0 = deterministic, 1.0 = creative).
# temperature = 0.7

# Override the built-in system prompt.
# system_prompt = "You are a helpful assistant."

# System prompt style: "auto" (default — detect from model size),
# "full" (detailed, 30B+), "concise" (7B–30B), "minimal" (< 7B).
# Ignored when system_prompt is set explicitly.
# prompt_style = "auto"

[paths]
# Directory where `shio pull` saves downloaded models.
# models_dir = "./models"

[tools]
# Allow the agent to use file-I/O and shell tools.
# enabled = true

# Ask for confirmation before writing or patching files.
# confirm_writes = true

# Ask for confirmation before running shell commands.
# confirm_shell = true

# If set, only commands whose first token matches this list are allowed.
# All other commands are blocked before execution.
# shell_allowlist = ["cargo", "git", "grep", "find", "ls", "cat", "head", "wc"]

# Commands whose first token matches this list are always blocked.
# shell_denylist = ["rm", "curl", "wget", "ssh", "scp", "sudo", "su"]

[lsp.servers]
# Map language names or file extensions to LSP server commands.
# rust   = "rust-analyzer"
# python = "pylsp"
# ts     = "typescript-language-server --stdio"

# ── Custom skills ─────────────────────────────────────────────────────────────
# Define named prompt templates invokable as /slash commands in `shio chat`.
# Use {args} as a placeholder for any text typed after the skill name.
# If the prompt has no {args} and the user supplies text, it is appended.
#
# [skills.commit]
# description = "Write a conventional git commit message"
# prompt      = "Write a conventional git commit message for the staged changes."
#
# [skills.review]
# description = "Review code for correctness and style"
# prompt      = "Review this for correctness, edge cases, and style: {args}"
"#;

pub fn run(_args: &InitArgs) -> Result<()> {
    write_config(Path::new("shio.toml"))
}

fn write_config(dest: &Path) -> Result<()> {
    if dest.exists() {
        bail!("{} already exists", dest.display());
    }
    std::fs::write(dest, TEMPLATE)?;
    println!("Created {}", dest.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(name);
        let _ = fs::remove_file(&p);
        p
    }

    #[test]
    fn creates_config_file() {
        let dest = tmp("shio_init_create.toml");
        write_config(&dest).unwrap();
        assert!(dest.exists());
        let _ = fs::remove_file(&dest);
    }

    #[test]
    fn generated_file_is_valid_toml() {
        let dest = tmp("shio_init_valid.toml");
        write_config(&dest).unwrap();
        let contents = fs::read_to_string(&dest).unwrap();
        toml::from_str::<toml::Value>(&contents).expect("must be valid TOML");
        let _ = fs::remove_file(&dest);
    }

    #[test]
    fn errors_when_file_already_exists() {
        let dest = tmp("shio_init_exists.toml");
        fs::write(&dest, "# existing").unwrap();
        assert!(write_config(&dest).is_err());
        // Existing content must be untouched.
        assert_eq!(fs::read_to_string(&dest).unwrap(), "# existing");
        let _ = fs::remove_file(&dest);
    }

    #[test]
    fn template_covers_all_sections() {
        for section in [
            "[server]",
            "[chat]",
            "[paths]",
            "[tools]",
            "[lsp.servers]",
            "[skills.",
        ] {
            assert!(TEMPLATE.contains(section), "missing section: {section}");
        }
    }
}
