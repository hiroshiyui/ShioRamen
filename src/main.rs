mod ask;
mod chat;
mod client;
mod config;
mod context;
mod doctor;
mod edit;
mod pull;
mod server;
mod tools;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use chat::{ChatSession, DEFAULT_SYSTEM_PROMPT};
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
    /// One-shot question with optional file context; streams answer to stdout
    Ask(ask::AskArgs),
    /// Apply an AI-suggested edit to a file (shows diff, asks to confirm)
    Edit(edit::EditArgs),
    /// Check that all required components are present and working
    Doctor(doctor::DoctorArgs),
    /// Download a model from HuggingFace or a direct URL into ./models/
    Pull(pull::PullArgs),
}

/// Server-launch flags shared across subcommands that need to spawn llama-server.
#[derive(clap::Args, Debug)]
pub struct ServerArgs {
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

    /// KV cache quantization type for keys, e.g. q4_0, q8_0, f16 [config: server.cache_type_k]
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
}

impl ServerArgs {
    /// Build a `Config` from CLI flags, config file values, and hardcoded defaults.
    /// `model` is passed separately because it lives outside `ServerArgs`.
    pub fn to_config(&self, model: PathBuf, cfg: &config::ServerSection) -> Config {
        Config {
            model_path: model,
            server_bin: self
                .server_bin
                .clone()
                .or_else(|| cfg.bin.clone())
                .unwrap_or_else(|| PathBuf::from(DEFAULT_SERVER_BIN)),
            host: self
                .host
                .clone()
                .or_else(|| cfg.host.clone())
                .unwrap_or_else(|| DEFAULT_HOST.to_string()),
            port: self.port.or(cfg.port).unwrap_or(DEFAULT_PORT),
            n_gpu_layers: self.ngl.or(cfg.ngl).unwrap_or(DEFAULT_NGL),
            ctx_size: self.ctx.or(cfg.ctx).unwrap_or(DEFAULT_CTX),
            cache_type_k: self.cache_type_k.clone().or_else(|| cfg.cache_type_k.clone()),
            cache_type_v: self.cache_type_v.clone().or_else(|| cfg.cache_type_v.clone()),
            flash_attn: self.flash_attn || cfg.flash_attn.unwrap_or(false),
            cont_batching: self.cont_batching || cfg.cont_batching.unwrap_or(false),
        }
    }
}

#[derive(clap::Args, Debug)]
struct ServeArgs {
    /// GGUF model file to load [config: chat.model]
    #[arg(short, long)]
    model: Option<PathBuf>,

    #[command(flatten)]
    server: ServerArgs,
}

#[derive(clap::Args, Debug)]
struct ChatArgs {
    /// GGUF model file to load [config: chat.model]
    #[arg(short, long)]
    model: Option<PathBuf>,

    #[command(flatten)]
    server: ServerArgs,

    /// Sampling temperature [config: chat.temperature, default: 0.7]
    #[arg(long)]
    temp: Option<f32>,

    /// Skip spawning llama-server; connect to an already running instance
    #[arg(long)]
    no_spawn: bool,

    /// Disable tool use for this session [config: tools.enabled]
    #[arg(long)]
    no_tools: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = ShioConfig::load_or_default(&cli.config);

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
            let config = args.server.to_config(model, &cfg.server);
            let _server = ServerProcess::spawn(&config).await?;
            eprintln!("Server running. Press Ctrl+C to stop.");
            tokio::signal::ctrl_c().await?;
            eprintln!("\nShutting down...");
        }

        Commands::Chat(args) => {
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
                let url = format!("http://{host}:{port}");
                eprintln!("Connecting to {url} ...");
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
                let config = args.server.to_config(model, &cfg.server);
                ServerProcess::spawn(&config).await?
            };

            println!();
            let executor = if !args.no_tools && cfg.tools.enabled.unwrap_or(true) {
                Some(tools::ToolExecutor {
                    confirm_writes: cfg.tools.confirm_writes.unwrap_or(true),
                    confirm_shell:  cfg.tools.confirm_shell.unwrap_or(true),
                })
            } else {
                None
            };
            let client = LlamaClient::new(server.url.clone());
            let mut session = ChatSession::new(client, temp, system_prompt, executor);
            session.run().await?;
            drop(server);
        }

        Commands::Ask(args) => {
            ask::run(&args, &cfg).await?;
        }

        Commands::Edit(args) => {
            edit::run(&args, &cfg).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(args)
    }

    // ── subcommand routing ───────────────────────────────────────────────────

    #[test]
    fn no_subcommand_is_an_error() {
        assert!(parse(&["shio"]).is_err());
    }

    #[test]
    fn serve_subcommand_recognised() {
        let cli = parse(&["shio", "serve"]).unwrap();
        assert!(matches!(cli.command, Commands::Serve(_)));
    }

    #[test]
    fn chat_subcommand_recognised() {
        let cli = parse(&["shio", "chat"]).unwrap();
        assert!(matches!(cli.command, Commands::Chat(_)));
    }

    #[test]
    fn doctor_subcommand_recognised() {
        let cli = parse(&["shio", "doctor"]).unwrap();
        assert!(matches!(cli.command, Commands::Doctor(_)));
    }

    #[test]
    fn pull_subcommand_recognised() {
        let cli = parse(&["shio", "pull", "owner/repo/model.gguf"]).unwrap();
        assert!(matches!(cli.command, Commands::Pull(_)));
    }

    // ── global --config flag ─────────────────────────────────────────────────

    #[test]
    fn config_flag_overrides_default() {
        let cli = parse(&["shio", "--config", "custom.toml", "doctor"]).unwrap();
        assert_eq!(cli.config, PathBuf::from("custom.toml"));
    }

    #[test]
    fn config_flag_defaults_to_shio_toml() {
        let cli = parse(&["shio", "doctor"]).unwrap();
        assert_eq!(cli.config, PathBuf::from("./shio.toml"));
    }

    // ── serve flags ──────────────────────────────────────────────────────────

    #[test]
    fn serve_parses_model_flag() {
        let cli = parse(&["shio", "serve", "--model", "foo.gguf"]).unwrap();
        let Commands::Serve(args) = cli.command else { panic!() };
        assert_eq!(args.model, Some(PathBuf::from("foo.gguf")));
    }

    #[test]
    fn serve_parses_all_server_flags() {
        let cli = parse(&[
            "shio", "serve",
            "--model", "foo.gguf",
            "--host", "0.0.0.0",
            "--port", "9090",
            "--ngl", "42",
            "--ctx", "4096",
            "--cache-type-k", "q4_0",
            "--cache-type-v", "q8_0",
            "--flash-attn",
            "--cont-batching",
        ]).unwrap();
        let Commands::Serve(args) = cli.command else { panic!() };
        assert_eq!(args.server.host,         Some("0.0.0.0".to_string()));
        assert_eq!(args.server.port,         Some(9090u16));
        assert_eq!(args.server.ngl,          Some(42i32));
        assert_eq!(args.server.ctx,          Some(4096u32));
        assert_eq!(args.server.cache_type_k, Some("q4_0".to_string()));
        assert_eq!(args.server.cache_type_v, Some("q8_0".to_string()));
        assert!(args.server.flash_attn);
        assert!(args.server.cont_batching);
    }

    // ── chat flags ───────────────────────────────────────────────────────────

    #[test]
    fn chat_parses_no_spawn_flag() {
        let cli = parse(&["shio", "chat", "--no-spawn"]).unwrap();
        let Commands::Chat(args) = cli.command else { panic!() };
        assert!(args.no_spawn);
    }

    #[test]
    fn chat_parses_temperature() {
        let cli = parse(&["shio", "chat", "--temp", "0.3"]).unwrap();
        let Commands::Chat(args) = cli.command else { panic!() };
        assert_eq!(args.temp, Some(0.3f32));
    }

    #[test]
    fn chat_flags_absent_by_default() {
        let cli = parse(&["shio", "chat"]).unwrap();
        let Commands::Chat(args) = cli.command else { panic!() };
        assert!(!args.no_spawn);
        assert!(!args.server.flash_attn);
        assert!(!args.server.cont_batching);
        assert!(args.model.is_none());
        assert!(args.temp.is_none());
    }

    // ── pull flags ───────────────────────────────────────────────────────────

    #[test]
    fn pull_stores_source() {
        let cli = parse(&["shio", "pull", "owner/repo/model.gguf"]).unwrap();
        let Commands::Pull(args) = cli.command else { panic!() };
        assert_eq!(args.source, "owner/repo/model.gguf");
    }

    #[test]
    fn pull_parses_models_dir() {
        let cli = parse(&["shio", "pull", "src", "--models-dir", "/tmp/models"]).unwrap();
        let Commands::Pull(args) = cli.command else { panic!() };
        assert_eq!(args.models_dir, Some(PathBuf::from("/tmp/models")));
    }

    // ── ask flags ────────────────────────────────────────────────────────────

    #[test]
    fn ask_subcommand_recognised() {
        let cli = parse(&["shio", "ask", "hello"]).unwrap();
        assert!(matches!(cli.command, Commands::Ask(_)));
    }

    #[test]
    fn ask_requires_question() {
        assert!(parse(&["shio", "ask"]).is_err());
    }

    #[test]
    fn ask_parses_question() {
        let cli = parse(&["shio", "ask", "what does this do?"]).unwrap();
        let Commands::Ask(args) = cli.command else { panic!() };
        assert_eq!(args.question, "what does this do?");
    }

    #[test]
    fn ask_parses_multiple_files() {
        let cli = parse(&["shio", "ask", "explain", "--file", "a.rs", "--file", "b.rs"]).unwrap();
        let Commands::Ask(args) = cli.command else { panic!() };
        assert_eq!(args.files, vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")]);
    }

    #[test]
    fn ask_defaults_are_empty() {
        let cli = parse(&["shio", "ask", "hello"]).unwrap();
        let Commands::Ask(args) = cli.command else { panic!() };
        assert!(args.model.is_none());
        assert!(args.temp.is_none());
        assert!(args.files.is_empty());
        assert!(!args.no_spawn);
        assert!(!args.server.flash_attn);
    }

    // ── edit flags ───────────────────────────────────────────────────────────

    #[test]
    fn edit_subcommand_recognised() {
        let cli = parse(&["shio", "edit", "src/main.rs", "add a comment"]).unwrap();
        assert!(matches!(cli.command, Commands::Edit(_)));
    }

    #[test]
    fn edit_requires_file_and_instruction() {
        assert!(parse(&["shio", "edit"]).is_err());
        assert!(parse(&["shio", "edit", "foo.rs"]).is_err());
    }

    #[test]
    fn edit_parses_file_and_instruction() {
        let cli = parse(&["shio", "edit", "src/lib.rs", "remove dead code"]).unwrap();
        let Commands::Edit(args) = cli.command else { panic!() };
        assert_eq!(args.file,        PathBuf::from("src/lib.rs"));
        assert_eq!(args.instruction, "remove dead code");
    }

    #[test]
    fn edit_yes_flag() {
        let cli = parse(&["shio", "edit", "f.rs", "fix it", "--yes"]).unwrap();
        let Commands::Edit(args) = cli.command else { panic!() };
        assert!(args.yes);
    }

    #[test]
    fn edit_defaults_are_false() {
        let cli = parse(&["shio", "edit", "f.rs", "fix it"]).unwrap();
        let Commands::Edit(args) = cli.command else { panic!() };
        assert!(!args.yes);
        assert!(!args.no_spawn);
        assert!(!args.server.flash_attn);
        assert!(args.model.is_none());
    }

    // ── doctor flags ────────────────────────────────────────────────────────

    #[test]
    fn doctor_parses_model_flag() {
        let cli = parse(&["shio", "doctor", "--model", "foo.gguf"]).unwrap();
        let Commands::Doctor(args) = cli.command else { panic!() };
        assert_eq!(args.model, Some(PathBuf::from("foo.gguf")));
    }

    #[test]
    fn doctor_parses_host_and_port() {
        let cli = parse(&["shio", "doctor", "--host", "0.0.0.0", "--port", "9090"]).unwrap();
        let Commands::Doctor(args) = cli.command else { panic!() };
        assert_eq!(args.host, Some("0.0.0.0".to_string()));
        assert_eq!(args.port, Some(9090u16));
    }
}
