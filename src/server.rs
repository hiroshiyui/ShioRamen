// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Context, Result};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
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
            eprintln!("  Reusing server already running at {url}");
            return Ok(Self { child: None, url });
        }

        eprintln!(
            "  Launching llama-server ({})...",
            config.server_bin.display()
        );

        // Extract the model path as a &str before building the command.
        // model_path.to_str() returns None for non-UTF-8 paths — handle that
        // instead of panicking.
        let model_str = config.model_path.to_str().ok_or_else(|| {
            anyhow::anyhow!(
                "model path is not valid UTF-8: {}",
                config.model_path.display()
            )
        })?;

        let mut cmd = Command::new(&config.server_bin);
        cmd.args([
            "--model",
            model_str,
            "--host",
            &config.host,
            "--port",
            &config.port.to_string(),
            "--n-gpu-layers",
            &config.n_gpu_layers.to_string(),
            "--ctx-size",
            &config.ctx_size.to_string(),
        ]);
        if let Some(ref ct) = config.cache_type_k {
            cmd.args(["--cache-type-k", ct]);
        }
        if let Some(ref ct) = config.cache_type_v {
            cmd.args(["--cache-type-v", ct]);
        }
        if config.flash_attn {
            cmd.args(["--flash-attn", "on"]);
        }
        if config.cont_batching {
            cmd.arg("--cont-batching");
        }
        if let Some(ref mmproj) = config.mmproj {
            let mmproj_str = mmproj.to_str().ok_or_else(|| {
                anyhow::anyhow!("mmproj path is not valid UTF-8: {}", mmproj.display())
            })?;
            cmd.args(["--mmproj", mmproj_str]);
        }
        let mut child = cmd
            .stdout(Stdio::null()) // HTTP request logs — keep silent during chat
            .stderr(Stdio::inherit()) // model loading, GPU layers, startup progress
            .spawn()
            .with_context(|| format!("Failed to spawn {:?}", config.server_bin))?;

        // Poll until ready (max 120 s).
        for elapsed in 1..=120 {
            sleep(Duration::from_secs(1)).await;
            if health_check(&url).await {
                eprintln!("  Server ready after {elapsed}s");
                return Ok(Self {
                    child: Some(child),
                    url,
                });
            }
        }

        // Kill the child before bailing — std::process::Child::drop() does NOT
        // terminate the process, only closes stdio handles, so we must kill explicitly.
        // wait() after kill() reaps the zombie so the OS can reclaim its entry.
        let _ = child.kill();
        let _ = child.wait();
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
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    let client = CLIENT.get_or_init(reqwest::Client::new);
    client
        .get(format!("{url}/health"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_stores_url_and_has_no_child() {
        let s = ServerProcess::external("http://127.0.0.1:8080".to_string());
        assert_eq!(s.url, "http://127.0.0.1:8080");
        drop(s); // Drop must not attempt to kill any process
    }
}
