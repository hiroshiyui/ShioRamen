mod chat;
mod client;
mod config;
mod server;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use chat::ChatSession;
use client::LlamaClient;
use config::Config;
use server::ServerProcess;

/// ShioRamen — local AI coding assistant powered by llama.cpp
#[derive(Parser, Debug)]
#[command(name = "shio", version, arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Launch llama-server and keep it running until Ctrl+C
    Serve(ServeArgs),
    /// Start an interactive chat session
    Chat(ChatArgs),
}

#[derive(clap::Args, Debug)]
struct ServeArgs {
    /// GGUF model file to load
    #[arg(short, long, required = true)]
    model: PathBuf,

    /// llama-server binary to launch
    #[arg(long, default_value = "./bin/llama-server")]
    server_bin: PathBuf,

    /// Server host
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Server port
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// GPU layers to offload (99 = all)
    #[arg(long, default_value_t = 99)]
    ngl: i32,

    /// Context window size in tokens
    #[arg(long, default_value_t = 8192)]
    ctx: u32,
}

#[derive(clap::Args, Debug)]
struct ChatArgs {
    /// GGUF model file to load (required unless --no-spawn)
    #[arg(short, long)]
    model: Option<PathBuf>,

    /// llama-server binary to launch
    #[arg(long, default_value = "./bin/llama-server")]
    server_bin: PathBuf,

    /// Server host
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Server port
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// GPU layers to offload (99 = all)
    #[arg(long, default_value_t = 99)]
    ngl: i32,

    /// Context window size in tokens
    #[arg(long, default_value_t = 8192)]
    ctx: u32,

    /// Sampling temperature
    #[arg(long, default_value_t = 0.7)]
    temp: f32,

    /// Skip spawning llama-server; connect to an already running instance
    #[arg(long)]
    no_spawn: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve(args) => {
            let config = Config {
                model_path: args.model,
                server_bin: args.server_bin,
                host: args.host,
                port: args.port,
                n_gpu_layers: args.ngl,
                ctx_size: args.ctx,
            };
            let _server = ServerProcess::spawn(&config).await?;
            println!("Server running. Press Ctrl+C to stop.");
            tokio::signal::ctrl_c().await?;
            println!("\nShutting down...");
        }

        Commands::Chat(args) => {
            let server = if args.no_spawn {
                let url = format!("http://{}:{}", args.host, args.port);
                println!("Connecting to {url} ...");
                ServerProcess::external(url)
            } else {
                let model = args.model.as_ref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "--model <PATH> is required (or use --no-spawn to attach to a running server)"
                    )
                })?;
                let config = Config {
                    model_path: model.clone(),
                    server_bin: args.server_bin,
                    host: args.host.clone(),
                    port: args.port,
                    n_gpu_layers: args.ngl,
                    ctx_size: args.ctx,
                };
                ServerProcess::spawn(&config).await?
            };

            println!();
            let client = LlamaClient::new(server.url.clone());
            let mut session = ChatSession::new(client, args.temp);
            session.run().await?;
            drop(server);
        }
    }

    Ok(())
}
