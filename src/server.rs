use anyhow::{Context, Result};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;

use crate::config::Config;

pub struct ServerProcess {
    child: Option<Child>,
    pub url: String,
}

impl ServerProcess {
    /// Connect to an already-running server without spawning.
    pub fn external(url: String) -> Self {
        Self { child: None, url }
    }

    /// Spawn llama-server and wait until the health endpoint responds.
    pub async fn spawn(config: &Config) -> Result<Self> {
        let url = config.server_url();

        // Re-use an already running server on the same address.
        if health_check(&url).await {
            println!("  Reusing server already running at {url}");
            return Ok(Self { child: None, url });
        }

        println!("  Launching llama-server ({})...", config.server_bin.display());

        let child = Command::new(&config.server_bin)
            .args([
                "--model",        config.model_path.to_str().unwrap(),
                "--host",         &config.host,
                "--port",         &config.port.to_string(),
                "--n-gpu-layers", &config.n_gpu_layers.to_string(),
                "--ctx-size",     &config.ctx_size.to_string(),
                "--log-disable",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("Failed to spawn {:?}", config.server_bin))?;

        // Poll until ready (max 120 s).
        for elapsed in 1..=120 {
            sleep(Duration::from_secs(1)).await;
            if health_check(&url).await {
                println!("  Server ready after {elapsed}s");
                return Ok(Self { child: Some(child), url });
            }
        }

        anyhow::bail!("llama-server did not become healthy within 120 seconds")
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
        }
    }
}

async fn health_check(url: &str) -> bool {
    reqwest::get(format!("{url}/health"))
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}
