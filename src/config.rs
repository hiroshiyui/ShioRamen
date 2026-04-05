use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub model_path: PathBuf,
    pub server_bin: PathBuf,
    pub host: String,
    pub port: u16,
    pub n_gpu_layers: i32,
    pub ctx_size: u32,
}

impl Config {
    pub fn server_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model_path: PathBuf::new(),
            server_bin: PathBuf::from("./bin/llama-server"),
            host: "127.0.0.1".to_string(),
            port: 8080,
            n_gpu_layers: 99,
            ctx_size: 8192,
        }
    }
}
