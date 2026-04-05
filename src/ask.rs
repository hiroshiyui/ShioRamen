use anyhow::Result;
use std::path::PathBuf;

use crate::chat::DEFAULT_SYSTEM_PROMPT;
use crate::client::{LlamaClient, Message};
use crate::context;
use crate::config::{
    Config, ShioConfig, DEFAULT_CTX, DEFAULT_HOST, DEFAULT_NGL, DEFAULT_PORT, DEFAULT_SERVER_BIN,
    DEFAULT_TEMP,
};
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

pub async fn run(args: &AskArgs, cfg: &ShioConfig) -> Result<()> {
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

    // Build user message: collect files/dirs as fenced code blocks, then append the question.
    let mut content = String::new();
    for path in &args.files {
        let files = context::collect(path)?;
        content.push_str(&context::format_as_blocks(&files));
    }
    content.push_str(&args.question);

    let messages = vec![
        Message::system(system_prompt),
        Message::user(content),
    ];

    let client = LlamaClient::new(server.url.clone());
    client.chat_stream(&messages, temp).await?;
    drop(server);
    Ok(())
}

