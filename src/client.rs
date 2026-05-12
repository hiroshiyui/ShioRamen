// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};

mod tool_parse;
use tool_parse::extract_embedded_tool_calls;

// ── Sampling parameters ──────────────────────────────────────────────────────

/// Sampling configuration forwarded to llama-server in every chat request.
#[derive(Debug, Clone, Copy)]
pub struct SamplingParams {
    pub temperature: f32,
    pub top_p: Option<f32>,
    pub min_p: Option<f32>,
    pub repeat_penalty: Option<f32>,
    /// Number of prompt tokens to retain when the context is shifted.
    /// `-1` keeps the entire initial prompt (system + first user turn).
    pub n_keep: Option<i32>,
}

// ── Message content (text or multimodal) ─────────────────────────────────────

/// Message content: either a plain string or an array of content parts (text +
/// images) following the OpenAI multimodal format.
///
/// Serialises as a bare `"string"` when text-only, or as
/// `[{"type":"text","text":"…"}, {"type":"image_url","image_url":{"url":"data:…"}}]`
/// when images are attached.  Deserialises both forms for session compatibility.
#[derive(Debug, Clone)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl Serialize for MessageContent {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Text(s) => serializer.serialize_str(s),
            Self::Parts(parts) => parts.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for MessageContent {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        use serde::de;
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(s) => Ok(Self::Text(s)),
            serde_json::Value::Array(_) => {
                let parts: Vec<ContentPart> =
                    serde_json::from_value(value).map_err(de::Error::custom)?;
                Ok(Self::Parts(parts))
            }
            _ => Err(de::Error::custom("content must be a string or array")),
        }
    }
}

#[allow(dead_code)]
impl MessageContent {
    /// Return the text portion, ignoring any image parts.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            Self::Parts(parts) => parts.iter().find_map(|p| {
                if let ContentPart::Text { text } = p {
                    Some(text.as_str())
                } else {
                    None
                }
            }),
        }
    }
}

/// A single content part inside a multimodal message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

/// URL wrapper for image content (typically a `data:` base64 URI).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
}

// ── Public message type ──────────────────────────────────────────────────────

/// A single message in the conversation. `content` is None when the assistant
/// responds with tool_calls instead of text. `tool_call_id` is set for tool-
/// result messages (role = "tool").
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self::text("system", content)
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self::text("user", content)
    }
    pub fn user_with_images(text: String, images: Vec<String>) -> Self {
        let mut parts: Vec<ContentPart> = images
            .into_iter()
            .map(|data_url| ContentPart::ImageUrl {
                image_url: ImageUrl { url: data_url },
            })
            .collect();
        parts.push(ContentPart::Text { text });
        Self {
            role: "user".into(),
            content: Some(MessageContent::Parts(parts)),
            tool_calls: None,
            tool_call_id: None,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::text("assistant", content)
    }
    pub fn assistant_tool_calls(calls: Vec<ToolCallItem>) -> Self {
        Self {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(calls),
            tool_call_id: None,
        }
    }
    pub fn tool_result(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: Some(MessageContent::Text(content.into())),
            tool_calls: None,
            tool_call_id: Some(id.into()),
        }
    }

    /// Convenience accessor: returns the text content of the message (if any),
    /// ignoring image parts in multimodal messages.
    #[allow(dead_code)]
    pub fn text_content(&self) -> Option<&str> {
        self.content.as_ref().and_then(|c| c.as_text())
    }

    fn text(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(MessageContent::Text(content.into())),
            tool_calls: None,
            tool_call_id: None,
        }
    }
}

// ── Tool call types (shared with tools.rs) ───────────────────────────────────

/// A tool call emitted by the assistant.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolCallItem {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolCallFunction,
}

/// The function name + JSON-encoded arguments inside a ToolCallItem.
#[allow(clippy::derived_hash_with_manual_eq)]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

/// A tool definition sent to the model so it knows what it can call.
#[derive(Debug, Serialize, Clone)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: FunctionSpec,
}

#[derive(Debug, Serialize, Clone)]
pub struct FunctionSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// The result of one agentic turn: either a final text response or tool calls
/// that the caller must execute before continuing.
pub enum AgentTurn {
    Text(String),
    ToolCalls(Vec<ToolCallItem>),
}

// ── Wire types ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repeat_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    n_keep: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [ToolDef]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
}

// Non-streaming response
#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<CompletionChoice>,
}

#[derive(Deserialize)]
struct CompletionChoice {
    message: CompletionMessage,
}

#[derive(Deserialize)]
struct CompletionMessage {
    content: Option<String>,
}

// Streaming response
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
    tool_calls: Option<Vec<DeltaToolCall>>,
}

/// A single tool-call delta within an SSE chunk.
/// `index` identifies which tool call in the batch is being built.
/// The first chunk for a given index carries `id` and `function.name`;
/// subsequent chunks append to `function.arguments`.
#[derive(Deserialize)]
struct DeltaToolCall {
    index: usize,
    id: Option<String>,
    function: Option<DeltaFunction>,
}

#[derive(Deserialize)]
struct DeltaFunction {
    name: Option<String>,
    arguments: Option<String>,
}

/// Accumulator for building a `ToolCallItem` from streaming deltas.
#[derive(Default)]
struct ToolCallBuilder {
    id: String,
    name: String,
    arguments: String,
}

// ── Client ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct LlamaClient {
    http: Client,
    base_url: String,
}

impl LlamaClient {
    pub fn new(base_url: String) -> Self {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self { http, base_url }
    }

    /// Streaming agentic turn: tokens are delivered via `on_token` as they arrive,
    /// and tool-call deltas are accumulated internally.  Returns `AgentTurn::Text`
    /// or `AgentTurn::ToolCalls` once the stream ends.
    ///
    /// Falls back to `chat_agent` (non-streaming) if the streaming response cannot
    /// be parsed, so embedded-tool-call models still work.
    pub async fn chat_agent_stream<F>(
        &self,
        messages: &[Message],
        sampling: SamplingParams,
        tools: &[ToolDef],
        mut on_token: F,
    ) -> Result<AgentTurn>
    where
        F: FnMut(&str),
    {
        let request = ChatRequest {
            model: "local",
            messages,
            stream: true,
            temperature: sampling.temperature,
            top_p: sampling.top_p,
            min_p: sampling.min_p,
            repeat_penalty: sampling.repeat_penalty,
            n_keep: sampling.n_keep,
            tools: Some(tools),
            tool_choice: Some("auto"),
        };

        let response = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let mut byte_stream = response.bytes_stream();
        let mut byte_buf: Vec<u8> = Vec::new();
        let mut line_buf = String::new();
        let mut full_text = String::new();

        // Accumulate tool-call deltas keyed by index.
        let mut tool_builders: Vec<ToolCallBuilder> = Vec::new();

        let mut done = false;
        while !done {
            let Some(chunk) = byte_stream.next().await else {
                break;
            };
            byte_buf.extend_from_slice(&chunk?);
            let split_at = byte_buf
                .iter()
                .rposition(|&b| b == b'\n')
                .map(|i| i + 1)
                .unwrap_or(0);
            if split_at == 0 {
                continue;
            }
            let to_decode = byte_buf.drain(..split_at).collect::<Vec<_>>();
            line_buf.push_str(&String::from_utf8_lossy(&to_decode));
            while let Some(pos) = line_buf.find('\n') {
                let raw = line_buf[..pos].trim().to_string();
                line_buf.drain(..=pos);

                let Some(data) = raw.strip_prefix("data: ") else {
                    continue;
                };
                if data == "[DONE]" {
                    done = true;
                    break;
                }

                let Ok(sc) = serde_json::from_str::<StreamChunk>(data) else {
                    continue;
                };
                let Some(choice) = sc.choices.first() else {
                    continue;
                };

                // Text content → stream to caller.
                if let Some(token) = choice.delta.content.as_deref()
                    && !token.is_empty()
                {
                    on_token(token);
                    full_text.push_str(token);
                }

                // Tool-call deltas → accumulate.
                if let Some(tc_deltas) = &choice.delta.tool_calls {
                    for dtc in tc_deltas {
                        // Grow the builder vec if needed.
                        while tool_builders.len() <= dtc.index {
                            tool_builders.push(ToolCallBuilder::default());
                        }
                        let builder = &mut tool_builders[dtc.index];
                        if let Some(id) = &dtc.id {
                            builder.id = id.clone();
                        }
                        if let Some(func) = &dtc.function {
                            if let Some(name) = &func.name {
                                builder.name.push_str(name);
                            }
                            if let Some(args) = &func.arguments {
                                builder.arguments.push_str(args);
                            }
                        }
                    }
                }
            }
        }

        // If we accumulated any tool calls, return them.
        if !tool_builders.is_empty() && tool_builders.iter().any(|b| !b.name.is_empty()) {
            let calls: Vec<ToolCallItem> = tool_builders
                .into_iter()
                .enumerate()
                .filter(|(_, b)| !b.name.is_empty())
                .map(|(i, b)| ToolCallItem {
                    id: if b.id.is_empty() {
                        format!("call_{i}")
                    } else {
                        b.id
                    },
                    kind: "function".into(),
                    function: ToolCallFunction {
                        name: b.name,
                        arguments: b.arguments,
                    },
                })
                .collect();
            return Ok(AgentTurn::ToolCalls(calls));
        }

        // No tool calls — treat as text.
        if full_text.is_empty() {
            anyhow::bail!("model returned no content and no tool calls");
        }

        // Check for embedded tool calls (peg-gemma4 style).
        let embedded = extract_embedded_tool_calls(&full_text);
        if !embedded.is_empty() {
            return Ok(AgentTurn::ToolCalls(embedded));
        }

        Ok(AgentTurn::Text(full_text))
    }

    /// Non-streaming completion without printing; returns the full response.
    /// Used by `edit` to post-process model output.
    pub async fn chat_collect(
        &self,
        messages: &[Message],
        sampling: SamplingParams,
    ) -> Result<String> {
        let request = ChatRequest {
            model: "local",
            messages,
            stream: false,
            temperature: sampling.temperature,
            top_p: sampling.top_p,
            min_p: sampling.min_p,
            repeat_penalty: sampling.repeat_penalty,
            n_keep: sampling.n_keep,
            tools: None,
            tool_choice: None,
        };
        let body: ChatResponse = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        body.choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| anyhow!("empty response from server"))
    }

    /// Streaming chat; prints tokens to stdout as they arrive.
    /// Returns the fully assembled text.
    pub async fn chat_stream(
        &self,
        messages: &[Message],
        sampling: SamplingParams,
    ) -> Result<String> {
        let text = self.chat_stream_cb(messages, sampling, print_flush).await?;
        println!();
        Ok(text)
    }

    /// Streaming chat with a per-token callback instead of printing.
    /// The callback is called synchronously for each token as it arrives.
    /// Returns the fully assembled text.
    pub async fn chat_stream_cb<F>(
        &self,
        messages: &[Message],
        sampling: SamplingParams,
        mut on_token: F,
    ) -> Result<String>
    where
        F: FnMut(&str),
    {
        let request = ChatRequest {
            model: "local",
            messages,
            stream: true,
            temperature: sampling.temperature,
            top_p: sampling.top_p,
            min_p: sampling.min_p,
            repeat_penalty: sampling.repeat_penalty,
            n_keep: sampling.n_keep,
            tools: None,
            tool_choice: None,
        };

        let response = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        let mut byte_stream = response.bytes_stream();
        let mut byte_buf: Vec<u8> = Vec::new();
        let mut line_buf = String::new();
        let mut full_text = String::new();

        let mut done = false;
        while !done {
            let Some(chunk) = byte_stream.next().await else {
                break;
            };
            byte_buf.extend_from_slice(&chunk?);
            // Decode only up to the last newline so we never split a multi-byte
            // UTF-8 sequence across chunks.
            let split_at = byte_buf
                .iter()
                .rposition(|&b| b == b'\n')
                .map(|i| i + 1)
                .unwrap_or(0);
            if split_at == 0 {
                continue;
            }
            let to_decode = byte_buf.drain(..split_at).collect::<Vec<_>>();
            line_buf.push_str(&String::from_utf8_lossy(&to_decode));
            while let Some(pos) = line_buf.find('\n') {
                let raw = line_buf[..pos].trim().to_string();
                line_buf.drain(..=pos);

                let Some(data) = raw.strip_prefix("data: ") else {
                    continue;
                };
                if data == "[DONE]" {
                    done = true;
                    break;
                }

                let Ok(sc) = serde_json::from_str::<StreamChunk>(data) else {
                    continue;
                };
                if let Some(token) = sc.choices.first().and_then(|c| c.delta.content.as_deref()) {
                    on_token(token);
                    full_text.push_str(token);
                }
            }
        }

        Ok(full_text)
    }
}

// ── Slot info ────────────────────────────────────────────────────────────────

/// A single KV-cache slot as returned by `GET /slots`.
#[derive(Debug, Deserialize)]
pub struct SlotInfo {
    pub id: u32,
    pub is_processing: bool,
    pub n_ctx: u32,
    /// Tokens consumed in the current turn; absent when the slot is idle.
    #[serde(default)]
    pub n_past: u32,
}

/// Server properties as returned by `GET /props`.
#[derive(Debug, Deserialize)]
pub struct ServerProps {
    #[serde(default)]
    pub total_slots: u32,
    #[serde(default)]
    pub default_generation_settings: GenerationSettings,
}

/// The `default_generation_settings` object inside `/props`.
#[derive(Debug, Default, Deserialize)]
pub struct GenerationSettings {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub n_ctx: u32,
    #[serde(default)]
    pub temperature: f64,
    #[serde(default)]
    pub top_p: f64,
    #[serde(default)]
    pub repeat_penalty: f64,
}

impl LlamaClient {
    /// Fetch server properties (model name, context size, generation defaults).
    pub async fn props(&self) -> Result<ServerProps> {
        let resp = self
            .http
            .get(format!("{}/props", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json::<ServerProps>()
            .await?;
        Ok(resp)
    }

    /// Fetch the list of KV-cache slots from the server.
    pub async fn slots(&self) -> Result<Vec<SlotInfo>> {
        let resp = self
            .http
            .get(format!("{}/slots", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json::<Vec<SlotInfo>>()
            .await?;
        Ok(resp)
    }
}

fn print_flush(s: &str) {
    use std::io::Write;
    print!("{s}");
    std::io::stdout().flush().ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Message constructors ──────────────────────────────────────────────────

    #[test]
    fn system_message_serialises_correctly() {
        let m = Message::system("be helpful");
        assert_eq!(m.role, "system");
        assert_eq!(m.text_content(), Some("be helpful"));
        assert!(m.tool_calls.is_none());
        assert!(m.tool_call_id.is_none());
    }

    #[test]
    fn user_message_serialises_correctly() {
        let m = Message::user("hello");
        assert_eq!(m.role, "user");
        assert_eq!(m.text_content(), Some("hello"));
    }

    #[test]
    fn assistant_message_serialises_correctly() {
        let m = Message::assistant("hi there");
        assert_eq!(m.role, "assistant");
        assert_eq!(m.text_content(), Some("hi there"));
    }

    #[test]
    fn tool_result_message_has_tool_call_id() {
        let m = Message::tool_result("call_abc", "file contents");
        assert_eq!(m.role, "tool");
        assert_eq!(m.text_content(), Some("file contents"));
        assert_eq!(m.tool_call_id.as_deref(), Some("call_abc"));
        assert!(m.tool_calls.is_none());
    }

    #[test]
    fn assistant_tool_calls_has_no_content() {
        let call = ToolCallItem {
            id: "call_1".into(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: "read_file".into(),
                arguments: r#"{"path":"foo.rs"}"#.into(),
            },
        };
        let m = Message::assistant_tool_calls(vec![call]);
        assert_eq!(m.role, "assistant");
        assert!(m.content.is_none());
        assert!(m.tool_calls.is_some());
    }

    #[test]
    fn optional_fields_are_absent_from_json_when_none() {
        let m = Message::user("hello");
        let json = serde_json::to_string(&m).unwrap();
        // skip_serializing_if = "Option::is_none" must suppress these keys
        assert!(!json.contains("tool_calls"));
        assert!(!json.contains("tool_call_id"));
    }

    #[test]
    fn tool_call_id_present_in_json_when_set() {
        let m = Message::tool_result("call_abc", "result");
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("call_abc"));
        assert!(json.contains("result"));
    }

    // ── SlotInfo ──────────────────────────────────────────────────────────────

    #[test]
    fn slot_info_deserialises_idle_slot() {
        // Actual llama-server response: is_processing + n_ctx, no n_past when idle.
        let json = r#"{"id":0,"is_processing":false,"n_ctx":8192,"speculative":false}"#;
        let slot: SlotInfo = serde_json::from_str(json).unwrap();
        assert_eq!(slot.id, 0);
        assert!(!slot.is_processing);
        assert_eq!(slot.n_ctx, 8192);
        assert_eq!(slot.n_past, 0); // defaults to 0 when absent
    }

    #[test]
    fn slot_info_deserialises_busy_slot() {
        let json = r#"{"id":3,"is_processing":true,"n_ctx":32768,"n_past":32665}"#;
        let slot: SlotInfo = serde_json::from_str(json).unwrap();
        assert_eq!(slot.id, 3);
        assert!(slot.is_processing);
        assert_eq!(slot.n_ctx, 32768);
        assert_eq!(slot.n_past, 32665);
    }

    #[test]
    fn slot_info_deserialises_array() {
        let json = r#"[
            {"id":0,"is_processing":false,"n_ctx":8192},
            {"id":1,"is_processing":true,"n_ctx":8192,"n_past":4096}
        ]"#;
        let slots: Vec<SlotInfo> = serde_json::from_str(json).unwrap();
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].n_past, 0);
        assert_eq!(slots[1].n_past, 4096);
    }

    #[test]
    fn slot_info_ignores_extra_fields() {
        // llama-server returns many more fields; we only care about our four.
        let json = r#"{"id":0,"is_processing":false,"n_ctx":4096,"n_past":100,"n_predict":200,"speculative":false}"#;
        let slot: SlotInfo = serde_json::from_str(json).unwrap();
        assert_eq!(slot.n_ctx, 4096);
        assert_eq!(slot.n_past, 100);
    }
}
