use anyhow::Result;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};

// ── Public message type shared with the chat module ────────────────────────

#[derive(Debug, Serialize, Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
}

// ── Wire types (request / SSE chunk) ────────────────────────────────────────

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    temperature: f32,
}

#[derive(Deserialize)]
struct StreamChunk {
    choices: Vec<ChunkChoice>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    delta: Delta,
}

#[derive(Deserialize)]
struct Delta {
    content: Option<String>,
}

// ── Client ──────────────────────────────────────────────────────────────────

pub struct LlamaClient {
    http: Client,
    base_url: String,
}

impl LlamaClient {
    pub fn new(base_url: String) -> Self {
        Self {
            http: Client::new(),
            base_url,
        }
    }

    /// Stream a chat completion to stdout, token by token.
    /// Returns the fully assembled assistant response.
    pub async fn chat_stream(&self, messages: &[Message], temperature: f32) -> Result<String> {
        let request = ChatRequest {
            model: "local",
            messages,
            stream: true,
            temperature,
        };

        let response = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let mut byte_stream = response.bytes_stream();
        let mut line_buf = String::new();
        let mut full_text = String::new();

        while let Some(chunk) = byte_stream.next().await {
            line_buf.push_str(&String::from_utf8_lossy(&chunk?));

            // Consume all complete lines from the buffer.
            while let Some(pos) = line_buf.find('\n') {
                let raw = line_buf[..pos].trim().to_string();
                line_buf = line_buf[pos + 1..].to_string();

                let Some(data) = raw.strip_prefix("data: ") else {
                    continue;
                };
                if data == "[DONE]" {
                    break;
                }

                let Ok(sc) = serde_json::from_str::<StreamChunk>(data) else {
                    continue;
                };
                if let Some(token) = sc.choices.first().and_then(|c| c.delta.content.as_deref()) {
                    print_flush(token);
                    full_text.push_str(token);
                }
            }
        }

        println!(); // final newline after the streamed response
        Ok(full_text)
    }
}

fn print_flush(s: &str) {
    use std::io::Write;
    print!("{s}");
    std::io::stdout().flush().ok();
}
