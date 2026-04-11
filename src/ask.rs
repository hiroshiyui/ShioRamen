// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::Result;
use std::io::Write;
use std::path::PathBuf;

use crate::ServerArgs;
use crate::build_engine;
use crate::client::{Message, SamplingParams};
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

    /// Repetition penalty (> 1.0 discourages repetition) [config: chat.repeat_penalty]
    #[arg(long)]
    pub repeat_penalty: Option<f32>,

    /// Skip spawning llama-server; connect to an already running instance
    #[arg(long)]
    pub no_spawn: bool,

    /// Run inference in-process via llama-cpp-2 (no llama-server).
    /// Requires the `inprocess` Cargo feature at build time.
    #[arg(long)]
    pub in_process: bool,
}

pub async fn run(args: &AskArgs, cfg: &ShioConfig) -> Result<()> {
    let sampling = SamplingParams {
        temperature: args.temp.or(cfg.chat.temperature).unwrap_or(DEFAULT_TEMP),
        top_p: args.top_p.or(cfg.chat.top_p),
        repeat_penalty: args.repeat_penalty.or(cfg.chat.repeat_penalty),
    };
    let system_prompt = crate::resolve_system_prompt(cfg);

    let (engine, server_guard) = build_engine(
        args.in_process,
        args.no_spawn,
        args.model.clone(),
        &args.server,
        cfg,
    )
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

    let mut on_token = |token: &str| {
        print!("{token}");
        std::io::stdout().flush().ok();
    };
    engine
        .chat_stream_cb(&messages, sampling, &mut on_token)
        .await?;
    println!();
    drop(server_guard);
    Ok(())
}
