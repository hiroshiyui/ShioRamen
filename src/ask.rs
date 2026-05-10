// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::Result;
use std::path::PathBuf;

use crate::ServerArgs;
use crate::client::{LlamaClient, Message, SamplingParams};
use crate::config::{DEFAULT_TEMP, ShioConfig};
use crate::context;

#[derive(clap::Args, Debug)]
pub struct AskArgs {
    /// Question to ask the model
    pub question: String,

    /// File(s) to include as context (repeatable)
    #[arg(short, long = "file", value_name = "PATH")]
    pub files: Vec<PathBuf>,

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

    /// Min-p sampling — drop tokens below `min_p * p_max` [config: chat.min_p]
    #[arg(long)]
    pub min_p: Option<f32>,

    /// Repetition penalty (> 1.0 discourages repetition) [config: chat.repeat_penalty]
    #[arg(long)]
    pub repeat_penalty: Option<f32>,

    /// Tokens to keep from the initial prompt during context shift; -1 = all [config: chat.n_keep]
    #[arg(long)]
    pub keep: Option<i32>,

    /// Skip spawning llama-server; connect to an already running instance
    #[arg(long)]
    pub no_spawn: bool,
}

pub async fn run(args: &AskArgs, cfg: &ShioConfig) -> Result<()> {
    let sampling = SamplingParams {
        temperature: args.temp.or(cfg.chat.temperature).unwrap_or(DEFAULT_TEMP),
        top_p: args.top_p.or(cfg.chat.top_p),
        min_p: args.min_p.or(cfg.chat.min_p),
        repeat_penalty: args.repeat_penalty.or(cfg.chat.repeat_penalty),
        n_keep: args.keep.or(cfg.chat.n_keep),
    };
    let system_prompt = crate::resolve_system_prompt(cfg);
    let server = args
        .server
        .spawn_or_connect(args.no_spawn, args.model.clone(), cfg)
        .await?;

    // Build user message: collect files/dirs as fenced code blocks, then append the question.
    // context::collect uses std::fs, so run it on a blocking thread to avoid
    // stalling the tokio executor.
    let paths = args.files.clone();
    let file_blocks = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let mut out = String::new();
        for path in &paths {
            let files = context::collect(path)?;
            out.push_str(&context::format_as_blocks(&files));
        }
        Ok(out)
    })
    .await??;
    let content = file_blocks + &args.question;

    let messages = vec![Message::system(system_prompt), Message::user(content)];

    let client = LlamaClient::new(server.url.clone());
    client.chat_stream(&messages, sampling).await?;
    drop(server);
    Ok(())
}
