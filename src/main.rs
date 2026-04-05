mod chat;
mod client;
mod config;
mod doctor;
mod pull;
mod server;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use chat::ChatSession;
use client::LlamaClient;
use config::{
    Config, ShioConfig, DEFAULT_CTX, DEFAULT_HOST, DEFAULT_NGL, DEFAULT_PORT, DEFAULT_SERVER_BIN,
    DEFAULT_TEMP,
};
use server::ServerProcess;

/// ShioRamen — local AI coding assistant powered by llama.cpp
#[derive(Parser, Debug)]
#[command(name = "shio", version, arg_required_else_help = true)]
struct Cli {
    /// Config file to load [default: ./shio.toml]
    #[arg(long, global = true, default_value = "./shio.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Launch llama-server and keep it running until Ctrl+C
    Serve(ServeArgs),
    /// Start an interactive chat session
    Chat(ChatArgs),
    /// Check that all required components are present and working
    Doctor(doctor::DoctorArgs),
    /// Download a model from HuggingFace or a direct URL into ./models/
    Pull(pull::PullArgs),
}

#[derive(clap::Args, Debug)]
struct ServeArgs {
    /// GGUF model file to load [config: chat.model]
    #[arg(short, long)]
    model: Option<PathBuf>,

    /// llama-server binary [config: server.bin, default: ./bin/llama-server]
    #[arg(long)]
    server_bin: Option<PathBuf>,

    /// Server host [config: server.host, default: 127.0.0.1]
    #[arg(long)]
    host: Option<String>,

    /// Server port [config: server.port, default: 8080]
    #[arg(long)]
    port: Option<u16>,

    /// GPU layers to offload [config: server.ngl, default: 99]
    #[arg(long)]
    ngl: Option<i32>,

    /// Context window size in tokens [config: server.ctx, default: 8192]
    #[arg(long)]
    ctx: Option<u32>,

    /// KV cache quantization type for keys, e.g. q4_0, q8_0, f16 [config: server.cache_type_k]
    #[arg(long)]
    cache_type_k: Option<String>,

    /// KV cache quantization type for values [config: server.cache_type_v]
    #[arg(long)]
    cache_type_v: Option<String>,

    /// Enable flash attention [config: server.flash_attn]
    #[arg(long)]
    flash_attn: bool,

    /// Enable continuous batching [config: server.cont_batching]
    #[arg(long)]
    cont_batching: bool,
}

#[derive(clap::Args, Debug)]
struct ChatArgs {
    /// GGUF model file to load [config: chat.model]
    #[arg(short, long)]
    model: Option<PathBuf>,

    /// llama-server binary [config: server.bin, default: ./bin/llama-server]
    #[arg(long)]
    server_bin: Option<PathBuf>,

    /// Server host [config: server.host, default: 127.0.0.1]
    #[arg(long)]
    host: Option<String>,

    /// Server port [config: server.port, default: 8080]
    #[arg(long)]
    port: Option<u16>,

    /// GPU layers to offload [config: server.ngl, default: 99]
    #[arg(long)]
    ngl: Option<i32>,

    /// Context window size in tokens [config: server.ctx, default: 8192]
    #[arg(long)]
    ctx: Option<u32>,

    /// KV cache quantization type for keys, e.g. q4_0, q8_0, f16 [config: server.cache_type_k]
    #[arg(long)]
    cache_type_k: Option<String>,

    /// KV cache quantization type for values [config: server.cache_type_v]
    #[arg(long)]
    cache_type_v: Option<String>,

    /// Enable flash attention [config: server.flash_attn]
    #[arg(long)]
    flash_attn: bool,

    /// Enable continuous batching [config: server.cont_batching]
    #[arg(long)]
    cont_batching: bool,

    /// Sampling temperature [config: chat.temperature, default: 0.7]
    #[arg(long)]
    temp: Option<f32>,

    /// Skip spawning llama-server; connect to an already running instance
    #[arg(long)]
    no_spawn: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = ShioConfig::load_or_default(&cli.config);

    // Shared resolution helpers (CLI > config > hardcoded default)
    macro_rules! resolve {
        ($cli:expr, $cfg:expr, $default:expr) => {
            $cli.or_else(|| $cfg).unwrap_or_else(|| $default)
        };
    }

    match cli.command {
        Commands::Serve(args) => {
            let model = args
                .model
                .or_else(|| cfg.chat.model.clone())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "--model <PATH> is required (or set chat.model in shio.toml)"
                    )
                })?;
            let config = Config {
                model_path:    model,
                server_bin:    resolve!(args.server_bin, cfg.server.bin.clone(), PathBuf::from(DEFAULT_SERVER_BIN)),
                host:          resolve!(args.host, cfg.server.host.clone(), DEFAULT_HOST.to_string()),
                port:          resolve!(args.port, cfg.server.port, DEFAULT_PORT),
                n_gpu_layers:  resolve!(args.ngl, cfg.server.ngl, DEFAULT_NGL),
                ctx_size:      resolve!(args.ctx, cfg.server.ctx, DEFAULT_CTX),
                cache_type_k:  args.cache_type_k.or_else(|| cfg.server.cache_type_k.clone()),
                cache_type_v:  args.cache_type_v.or_else(|| cfg.server.cache_type_v.clone()),
                flash_attn:    args.flash_attn    || cfg.server.flash_attn.unwrap_or(false),
                cont_batching: args.cont_batching || cfg.server.cont_batching.unwrap_or(false),
            };
            let _server = ServerProcess::spawn(&config).await?;
            println!("Server running. Press Ctrl+C to stop.");
            tokio::signal::ctrl_c().await?;
            println!("\nShutting down...");
        }

        Commands::Chat(args) => {
            let server_bin = resolve!(args.server_bin, cfg.server.bin.clone(), PathBuf::from(DEFAULT_SERVER_BIN));
            let host       = resolve!(args.host, cfg.server.host.clone(), DEFAULT_HOST.to_string());
            let port       = resolve!(args.port, cfg.server.port, DEFAULT_PORT);
            let temp       = resolve!(args.temp, cfg.chat.temperature, DEFAULT_TEMP);

            let server = if args.no_spawn {
                let url = format!("http://{host}:{port}");
                println!("Connecting to {url} ...");
                ServerProcess::external(url)
            } else {
                let model = args
                    .model
                    .or_else(|| cfg.chat.model.clone())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "--model <PATH> is required (or set chat.model in shio.toml)"
                        )
                    })?;
                let config = Config {
                    model_path:    model,
                    server_bin,
                    host:          host.clone(),
                    port,
                    n_gpu_layers:  resolve!(args.ngl, cfg.server.ngl, DEFAULT_NGL),
                    ctx_size:      resolve!(args.ctx, cfg.server.ctx, DEFAULT_CTX),
                    cache_type_k:  args.cache_type_k.or_else(|| cfg.server.cache_type_k.clone()),
                    cache_type_v:  args.cache_type_v.or_else(|| cfg.server.cache_type_v.clone()),
                    flash_attn:    args.flash_attn    || cfg.server.flash_attn.unwrap_or(false),
                    cont_batching: args.cont_batching || cfg.server.cont_batching.unwrap_or(false),
                };
                ServerProcess::spawn(&config).await?
            };

            println!();
            let client = LlamaClient::new(server.url.clone());
            let mut session = ChatSession::new(client, temp);
            session.run().await?;
            drop(server);
        }

        Commands::Doctor(args) => {
            doctor::run(&args, &cfg).await;
        }

        Commands::Pull(args) => {
            pull::run(&args, &cfg).await?;
        }
    }

    Ok(())
}
