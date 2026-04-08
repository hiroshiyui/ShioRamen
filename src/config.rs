// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ── TOML config file (`shio.toml`) ───────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct ShioConfig {
    #[serde(default)]
    pub server: ServerSection,
    #[serde(default)]
    pub chat: ChatSection,
    #[serde(default)]
    pub paths: PathsSection,
    #[serde(default)]
    pub tools: ToolsSection,
    #[serde(default)]
    pub lsp: LspSection,
    #[serde(default)]
    pub skills: HashMap<String, SkillDef>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ServerSection {
    pub bin: Option<PathBuf>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub ngl: Option<i32>,
    pub ctx: Option<u32>,
    /// KV cache quantization type for keys (e.g. "q4_0", "q8_0", "f16")
    pub cache_type_k: Option<String>,
    /// KV cache quantization type for values
    pub cache_type_v: Option<String>,
    /// Enable flash attention
    pub flash_attn: Option<bool>,
    /// Enable continuous batching
    pub cont_batching: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ChatSection {
    pub model: Option<PathBuf>,
    pub temperature: Option<f32>,
    /// Top-p (nucleus) sampling.  Lower values focus on the most likely tokens.
    pub top_p: Option<f32>,
    /// Repetition penalty.  Values > 1.0 discourage the model from repeating
    /// the same tokens, reducing looping in long outputs.
    pub repeat_penalty: Option<f32>,
    pub system_prompt: Option<String>,
    /// Show `<think>…</think>` blocks from reasoning models in a dimmed style.
    /// Default: true.
    pub show_thinking: Option<bool>,
    /// System prompt style: "full" (default, detailed tool guidance), "concise"
    /// (shorter for capable mid-size models), "minimal" (bare essentials for
    /// small models), or "auto" (detect from model filename).
    /// Ignored when `system_prompt` is set explicitly.
    pub prompt_style: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct PathsSection {
    pub models_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ToolsSection {
    /// Enable tool use (file I/O, shell) in chat sessions. Default: true.
    pub enabled: Option<bool>,
    /// Ask for confirmation before writing files. Default: true.
    pub confirm_writes: Option<bool>,
    /// Ask for confirmation before running shell commands. Default: true.
    pub confirm_shell: Option<bool>,
    /// If set, only shell commands whose first token matches one of these entries
    /// are allowed.  All other commands are rejected before execution.
    /// Example: `["cargo", "git", "grep", "ls"]`
    #[serde(default)]
    pub shell_allowlist: Vec<String>,
    /// Shell commands whose first token matches one of these entries are rejected
    /// before execution.  Checked after the allowlist (if both are set, a command
    /// must pass the allowlist *and* not appear on the denylist).
    /// Example: `["rm", "curl", "wget", "ssh"]`
    #[serde(default)]
    pub shell_denylist: Vec<String>,
}

/// `[lsp]` section — override which LSP server to use per language or extension.
///
/// Keys are language names (`rust`, `python`, `typescript`, …) or file extensions
/// (`rs`, `py`, `ts`, …).  Values are the command to run (binary + args).
///
/// Example:
/// ```toml
/// [lsp.servers]
/// rust = "rust-analyzer"
/// python = "pylsp"
/// ```
#[derive(Debug, Deserialize, Default)]
pub struct LspSection {
    #[serde(default)]
    pub servers: HashMap<String, String>,
}

/// One entry under `[skills.<name>]` in shio.toml.
///
/// Example:
/// ```toml
/// [skills.commit]
/// description = "Write a conventional git commit message"
/// prompt      = "Write a conventional git commit message for the staged changes."
/// ```
#[derive(Debug, Deserialize, Clone)]
pub struct SkillDef {
    pub description: String,
    pub prompt: String,
}

impl ShioConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("Cannot read config file: {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("Failed to parse {}", path.display()))
    }

    /// Load the given path if it exists; silently return defaults otherwise.
    pub fn load_or_default(path: &Path) -> Self {
        if path.exists() {
            match Self::load(path) {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!("⚠️  Config warning: {e}");
                    Self::default()
                }
            }
        } else {
            Self::default()
        }
    }
}

// ── Hardcoded fallback defaults ───────────────────────────────────────────────

pub const DEFAULT_SERVER_BIN: &str = "./bin/llama-server";
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 8080;
pub const DEFAULT_NGL: i32 = 99;
pub const DEFAULT_CTX: u32 = 8192;
pub const DEFAULT_TEMP: f32 = 0.7;
pub const DEFAULT_MODELS_DIR: &str = "./models";

// ── Runtime config (passed to ServerProcess) ─────────────────────────────────

#[derive(Debug, Clone)]
pub struct Config {
    pub model_path: PathBuf,
    pub server_bin: PathBuf,
    pub host: String,
    pub port: u16,
    pub n_gpu_layers: i32,
    pub ctx_size: u32,
    pub cache_type_k: Option<String>,
    pub cache_type_v: Option<String>,
    pub flash_attn: bool,
    pub cont_batching: bool,
}

impl Config {
    pub fn server_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Config::server_url ───────────────────────────────────────────────────

    #[test]
    fn server_url_combines_host_and_port() {
        let cfg = Config {
            model_path: PathBuf::from("model.gguf"),
            server_bin: PathBuf::from("./bin/llama-server"),
            host: "127.0.0.1".to_string(),
            port: 8080,
            n_gpu_layers: 99,
            ctx_size: 8192,
            cache_type_k: None,
            cache_type_v: None,
            flash_attn: false,
            cont_batching: false,
        };
        assert_eq!(cfg.server_url(), "http://127.0.0.1:8080");
    }

    #[test]
    fn server_url_non_default_port() {
        let cfg = Config {
            model_path: PathBuf::new(),
            server_bin: PathBuf::new(),
            host: "0.0.0.0".to_string(),
            port: 9090,
            n_gpu_layers: 0,
            ctx_size: 0,
            cache_type_k: None,
            cache_type_v: None,
            flash_attn: false,
            cont_batching: false,
        };
        assert_eq!(cfg.server_url(), "http://0.0.0.0:9090");
    }

    // ── ShioConfig TOML parsing ──────────────────────────────────────────────

    #[test]
    fn parse_full_config() {
        let src = r#"
            [server]
            bin  = "./bin/llama-server"
            host = "0.0.0.0"
            port = 9090
            ngl  = 42
            ctx  = 32768
            cache_type_k  = "q4_0"
            cache_type_v  = "q4_0"
            flash_attn    = true
            cont_batching = true

            [chat]
            model          = "./models/model.gguf"
            temperature    = 0.3
            top_p          = 0.9
            repeat_penalty = 1.15

            [paths]
            models_dir = "./models"
        "#;
        let cfg: ShioConfig = toml::from_str(src).unwrap();
        assert_eq!(cfg.server.host.as_deref(), Some("0.0.0.0"));
        assert_eq!(cfg.server.port, Some(9090));
        assert_eq!(cfg.server.ngl, Some(42));
        assert_eq!(cfg.server.ctx, Some(32768));
        assert_eq!(cfg.server.cache_type_k.as_deref(), Some("q4_0"));
        assert_eq!(cfg.server.flash_attn, Some(true));
        assert_eq!(cfg.server.cont_batching, Some(true));
        assert_eq!(cfg.chat.temperature, Some(0.3));
        assert_eq!(cfg.chat.top_p, Some(0.9));
        assert_eq!(cfg.chat.repeat_penalty, Some(1.15));
        assert_eq!(cfg.paths.models_dir, Some(PathBuf::from("./models")));
    }

    #[test]
    fn parse_partial_config_leaves_rest_as_none() {
        let src = "[server]\nport = 9090\n";
        let cfg: ShioConfig = toml::from_str(src).unwrap();
        assert!(cfg.server.host.is_none());
        assert_eq!(cfg.server.port, Some(9090));
        assert!(cfg.chat.model.is_none());
        assert!(cfg.paths.models_dir.is_none());
    }

    #[test]
    fn parse_empty_toml_gives_all_nones() {
        let cfg: ShioConfig = toml::from_str("").unwrap();
        assert!(cfg.server.bin.is_none());
        assert!(cfg.server.host.is_none());
        assert!(cfg.server.port.is_none());
        assert!(cfg.chat.model.is_none());
    }

    #[test]
    fn load_missing_file_returns_default() {
        let cfg = ShioConfig::load_or_default(Path::new("/nonexistent/shio.toml"));
        assert!(cfg.server.host.is_none());
        assert!(cfg.chat.model.is_none());
    }

    #[test]
    fn load_valid_file_round_trip() {
        let path = std::env::temp_dir().join("shio_test_valid.toml");
        std::fs::write(&path, "[server]\nport = 9999\n").unwrap();
        let cfg = ShioConfig::load_or_default(&path);
        assert_eq!(cfg.server.port, Some(9999));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_invalid_toml_returns_error() {
        let dir = std::env::temp_dir();
        let path = dir.join("shio_test_invalid.toml");
        std::fs::write(&path, "[[not valid toml").unwrap();
        assert!(ShioConfig::load(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_lsp_servers_section() {
        let src = r#"
            [lsp.servers]
            rust = "rust-analyzer"
            python = "pylsp"
            ts = "typescript-language-server --stdio"
        "#;
        let cfg: ShioConfig = toml::from_str(src).unwrap();
        assert_eq!(
            cfg.lsp.servers.get("rust").map(String::as_str),
            Some("rust-analyzer")
        );
        assert_eq!(
            cfg.lsp.servers.get("python").map(String::as_str),
            Some("pylsp")
        );
        assert_eq!(
            cfg.lsp.servers.get("ts").map(String::as_str),
            Some("typescript-language-server --stdio")
        );
    }

    #[test]
    fn parse_empty_lsp_section_gives_empty_map() {
        let cfg: ShioConfig = toml::from_str("").unwrap();
        assert!(cfg.lsp.servers.is_empty());
    }

    // ── Skills section ───────────────────────────────────────────────────────

    #[test]
    fn parse_skills_section() {
        let src = r#"
            [skills.commit]
            description = "Write a conventional git commit message"
            prompt = "Write a conventional git commit message for the staged changes."

            [skills.review]
            description = "Review code for correctness and style"
            prompt = "Review this for correctness, edge cases, and style: {args}"
        "#;
        let cfg: ShioConfig = toml::from_str(src).unwrap();
        assert_eq!(cfg.skills.len(), 2);
        let commit = cfg.skills.get("commit").unwrap();
        assert_eq!(
            commit.description,
            "Write a conventional git commit message"
        );
        assert!(!commit.prompt.contains("{args}"));
        let review = cfg.skills.get("review").unwrap();
        assert!(review.prompt.contains("{args}"));
    }

    #[test]
    fn parse_empty_skills_gives_empty_map() {
        let cfg: ShioConfig = toml::from_str("").unwrap();
        assert!(cfg.skills.is_empty());
    }

    #[test]
    fn skills_missing_description_is_error() {
        let src = "[skills.bad]\nprompt = \"do something\"\n";
        assert!(toml::from_str::<ShioConfig>(src).is_err());
    }

    #[test]
    fn skills_missing_prompt_is_error() {
        let src = "[skills.bad]\ndescription = \"something\"\n";
        assert!(toml::from_str::<ShioConfig>(src).is_err());
    }
}
