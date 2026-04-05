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

#[cfg(test)]
mod tests {
    use super::*;

    // ── Config::server_url ───────────────────────────────────────────────────

    #[test]
    fn server_url_combines_host_and_port() {
        let cfg = Config {
            model_path:    PathBuf::from("model.gguf"),
            server_bin:    PathBuf::from("./bin/llama-server"),
            host:          "127.0.0.1".to_string(),
            port:          8080,
            n_gpu_layers:  99,
            ctx_size:      8192,
            cache_type_k:  None,
            cache_type_v:  None,
            flash_attn:    false,
            cont_batching: false,
        };
        assert_eq!(cfg.server_url(), "http://127.0.0.1:8080");
    }

    #[test]
    fn server_url_non_default_port() {
        let cfg = Config {
            model_path:    PathBuf::new(),
            server_bin:    PathBuf::new(),
            host:          "0.0.0.0".to_string(),
            port:          9090,
            n_gpu_layers:  0,
            ctx_size:      0,
            cache_type_k:  None,
            cache_type_v:  None,
            flash_attn:    false,
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
            model       = "./models/model.gguf"
            temperature = 0.3

            [paths]
            models_dir = "./models"
        "#;
        let cfg: ShioConfig = toml::from_str(src).unwrap();
        assert_eq!(cfg.server.host.as_deref(),        Some("0.0.0.0"));
        assert_eq!(cfg.server.port,                   Some(9090));
        assert_eq!(cfg.server.ngl,                    Some(42));
        assert_eq!(cfg.server.ctx,                    Some(32768));
        assert_eq!(cfg.server.cache_type_k.as_deref(),Some("q4_0"));
        assert_eq!(cfg.server.flash_attn,             Some(true));
        assert_eq!(cfg.server.cont_batching,          Some(true));
        assert_eq!(cfg.chat.temperature,              Some(0.3));
        assert_eq!(cfg.paths.models_dir,              Some(PathBuf::from("./models")));
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
    fn load_invalid_toml_returns_error() {
        let dir = std::env::temp_dir();
        let path = dir.join("shio_test_invalid.toml");
        std::fs::write(&path, "[[not valid toml").unwrap();
        assert!(ShioConfig::load(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
