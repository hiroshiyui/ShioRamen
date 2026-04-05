// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::Result;
use std::path::PathBuf;

use crate::ServerArgs;
use crate::chat::DEFAULT_SYSTEM_PROMPT;
use crate::client::{LlamaClient, Message};
use crate::config::{DEFAULT_HOST, DEFAULT_PORT, DEFAULT_TEMP, ShioConfig};
use crate::context;
use crate::server::ServerProcess;

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

    /// Skip spawning llama-server; connect to an already running instance
    #[arg(long)]
    pub no_spawn: bool,
}

pub async fn run(args: &AskArgs, cfg: &ShioConfig) -> Result<()> {
    let host = args
        .server
        .host
        .clone()
        .or_else(|| cfg.server.host.clone())
        .unwrap_or_else(|| DEFAULT_HOST.to_string());
    let port = args.server.port.or(cfg.server.port).unwrap_or(DEFAULT_PORT);
    let temp = args.temp.or(cfg.chat.temperature).unwrap_or(DEFAULT_TEMP);
    let system_prompt = cfg
        .chat
        .system_prompt
        .clone()
        .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string());

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
        let config = args.server.to_config(model, &cfg.server);
        ServerProcess::spawn(&config).await?
    };

    // Build user message: collect files/dirs as fenced code blocks, then append the question.
    let mut content = String::new();
    for path in &args.files {
        let files = context::collect(path)?;
        content.push_str(&context::format_as_blocks(&files));
    }
    content.push_str(&args.question);

    let messages = vec![Message::system(system_prompt), Message::user(content)];

    let client = LlamaClient::new(server.url.clone());
    client.chat_stream(&messages, temp).await?;
    drop(server);
    Ok(())
}
