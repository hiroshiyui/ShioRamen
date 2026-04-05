use anyhow::{Context, Result};
use serde::Deserialize;
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
}

#[derive(Debug, Deserialize, Default)]
pub struct ServerSection {
    pub bin:           Option<PathBuf>,
    pub host:          Option<String>,
    pub port:          Option<u16>,
    pub ngl:           Option<i32>,
    pub ctx:           Option<u32>,
    /// KV cache quantization type for keys (e.g. "q4_0", "q8_0", "f16")
    pub cache_type_k:  Option<String>,
    /// KV cache quantization type for values
    pub cache_type_v:  Option<String>,
    /// Enable flash attention
    pub flash_attn:    Option<bool>,
    /// Enable continuous batching
    pub cont_batching: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ChatSection {
    pub model:       Option<PathBuf>,
    pub temperature: Option<f32>,
}

#[derive(Debug, Deserialize, Default)]
pub struct PathsSection {
    pub models_dir: Option<PathBuf>,
}

impl ShioConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("Cannot read config file: {}", path.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("Failed to parse {}", path.display()))
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

pub const DEFAULT_SERVER_BIN:  &str = "./bin/llama-server";
pub const DEFAULT_HOST:        &str = "127.0.0.1";
pub const DEFAULT_PORT:        u16  = 8080;
pub const DEFAULT_NGL:         i32  = 99;
pub const DEFAULT_CTX:         u32  = 8192;
pub const DEFAULT_TEMP:        f32  = 0.7;
pub const DEFAULT_MODELS_DIR:  &str = "./models";

// ── Runtime config (passed to ServerProcess) ─────────────────────────────────

#[derive(Debug, Clone)]
pub struct Config {
    pub model_path:    PathBuf,
    pub server_bin:    PathBuf,
    pub host:          String,
    pub port:          u16,
    pub n_gpu_layers:  i32,
    pub ctx_size:      u32,
    pub cache_type_k:  Option<String>,
    pub cache_type_v:  Option<String>,
    pub flash_attn:    bool,
    pub cont_batching: bool,
}

impl Config {
    pub fn server_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}
