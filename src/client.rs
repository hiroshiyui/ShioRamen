// SPDX-License-Identifier: GPL-3.0-or-later
use std::fmt::Write as _;

use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};

// ── Sampling parameters ──────────────────────────────────────────────────────

/// Sampling configuration forwarded to llama-server in every chat request.
#[derive(Debug, Clone, Copy)]
pub struct SamplingParams {
    pub temperature: f32,
    pub top_p: Option<f32>,
    pub repeat_penalty: Option<f32>,
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
    repeat_penalty: Option<f32>,
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
            repeat_penalty: sampling.repeat_penalty,
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
            repeat_penalty: sampling.repeat_penalty,
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
            repeat_penalty: sampling.repeat_penalty,
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

/// Extract tool calls that a local model embedded in the content field instead
/// of in the structured `tool_calls` field.
///
/// Some templates (e.g. peg-gemma4) wrap JSON payloads in `<tool_call>` /
/// `</tool_call>` (or `<|tool_call|>` / `<|/tool_call|>`) markers and replace
/// the `"` character with the special token `<|"|>`.  This function normalises
/// those tokens and parses every embedded call it finds.
///
/// Expected payload shape: `{"name": "fn_name", "arguments": { … }}`
/// Also accepts `"parameters"` in place of `"arguments"` (used by some models).
fn extract_embedded_tool_calls(text: &str) -> Vec<ToolCallItem> {
    // Normalise the special quote token so JSON parsing works.
    let normalised = text.replace("<|\"|>", "\"").replace("<|\"|\u{3e}", "\"");

    // Collect all blocks between supported delimiters.
    let mut raw_blocks: Vec<&str> = Vec::new();
    for (open, close) in [
        ("<tool_call>", "</tool_call>"),
        ("<|tool_call|>", "<|/tool_call|>"),
    ] {
        let mut haystack = normalised.as_str();
        while let Some(start) = haystack.find(open) {
            let after_open = &haystack[start + open.len()..];
            if let Some(end) = after_open.find(close) {
                raw_blocks.push(&after_open[..end]);
                haystack = &after_open[end + close.len()..];
            } else {
                break;
            }
        }
    }

    let mut calls = Vec::new();
    for (i, block) in raw_blocks.iter().enumerate() {
        let trimmed = block.trim();

        // ── Strategy 1: JSON format ─────────────────────────────────────
        // {"name": "func", "arguments": { … }}
        if let Some(item) = try_parse_json_tool_call(i, trimmed) {
            calls.push(item);
            continue;
        }

        // ── Strategy 2: Python function-call format (peg-gemma4) ────────
        // func_name(key="val", key2="val2")
        if let Some(item) = try_parse_funcall_tool_call(i, trimmed) {
            calls.push(item);
            continue;
        }

        eprintln!("[shio] embedded tool call block {i}: could not parse as JSON or function call");
    }
    calls
}

/// Try to parse a `<tool_call>` block as JSON:
/// `{"name": "func", "arguments"|"parameters": { … }}`
fn try_parse_json_tool_call(index: usize, block: &str) -> Option<ToolCallItem> {
    let v = match serde_json::from_str::<serde_json::Value>(block) {
        Ok(v) => v,
        Err(_) => {
            let repaired = escape_control_chars_in_json_strings(block);
            serde_json::from_str::<serde_json::Value>(&repaired).ok()?
        }
    };
    let name = v["name"].as_str()?;
    let args = ["arguments", "parameters"]
        .iter()
        .find_map(|k| {
            let val = &v[k];
            if val.is_object() || val.is_array() {
                Some(val.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "{}".to_string());
    Some(ToolCallItem {
        id: format!("embedded_{index}"),
        kind: "function".to_string(),
        function: ToolCallFunction {
            name: name.to_string(),
            arguments: args,
        },
    })
}

/// Try to parse a `<tool_call>` block as a Python-style function call:
/// `func_name(key="val", key2="val2")`
///
/// This is the format used by Gemma 4 models via the peg-gemma4 chat format.
fn try_parse_funcall_tool_call(index: usize, block: &str) -> Option<ToolCallItem> {
    // Find function name — everything before the first '('.
    let paren = block.find('(')?;
    let name = block[..paren].trim();
    if name.is_empty() || name.contains('{') {
        return None; // Not a function call, probably malformed JSON
    }

    // Everything between the outer parentheses.
    let rest = &block[paren + 1..];
    let inner = rest.rsplit_once(')')?.0;

    // Parse keyword arguments: key=value, key2=value2, ...
    let args = parse_kwargs(inner);

    let args_json = serde_json::to_string(&args).ok()?;
    Some(ToolCallItem {
        id: format!("embedded_{index}"),
        kind: "function".to_string(),
        function: ToolCallFunction {
            name: name.to_string(),
            arguments: args_json,
        },
    })
}

/// Parse Python-style keyword arguments: `key="value", key2="value2"`
/// Returns a JSON object.  Handles quoted strings with escapes and bare
/// newlines inside values.
fn parse_kwargs(s: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    let mut pos = 0;
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();

    while pos < len {
        // Skip whitespace and commas.
        while pos < len && (chars[pos].is_whitespace() || chars[pos] == ',') {
            pos += 1;
        }
        if pos >= len {
            break;
        }

        // Read key (identifier before '=').
        let key_start = pos;
        while pos < len && chars[pos] != '=' && !chars[pos].is_whitespace() {
            pos += 1;
        }
        let key: String = chars[key_start..pos].iter().collect();
        if key.is_empty() {
            break;
        }

        // Skip whitespace and '='.
        while pos < len && (chars[pos].is_whitespace() || chars[pos] == '=') {
            pos += 1;
        }
        if pos >= len {
            break;
        }

        // Read value.
        let value = if chars[pos] == '"' || chars[pos] == '\'' {
            // Quoted string — scan to matching close quote, respecting escapes.
            let quote = chars[pos];
            pos += 1; // skip opening quote
            let mut val = String::new();
            while pos < len && chars[pos] != quote {
                if chars[pos] == '\\' && pos + 1 < len {
                    let esc = chars[pos + 1];
                    match esc {
                        'n' => val.push('\n'),
                        'r' => val.push('\r'),
                        't' => val.push('\t'),
                        '\\' => val.push('\\'),
                        c if c == quote => val.push(c),
                        _ => {
                            val.push('\\');
                            val.push(esc);
                        }
                    }
                    pos += 2;
                } else {
                    val.push(chars[pos]);
                    pos += 1;
                }
            }
            if pos < len {
                pos += 1; // skip closing quote
            }
            serde_json::Value::String(val)
        } else {
            // Unquoted value — read until comma or end.
            let val_start = pos;
            while pos < len && chars[pos] != ',' && chars[pos] != ')' {
                pos += 1;
            }
            let raw: String = chars[val_start..pos].iter().collect();
            let raw = raw.trim();
            // Try to parse as number or boolean.
            if let Ok(n) = raw.parse::<i64>() {
                serde_json::Value::Number(n.into())
            } else if let Ok(n) = raw.parse::<f64>() {
                serde_json::json!(n)
            } else if raw == "true" {
                serde_json::Value::Bool(true)
            } else if raw == "false" {
                serde_json::Value::Bool(false)
            } else if raw == "null" || raw == "None" {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(raw.to_string())
            }
        };

        map.insert(key, value);
    }
    map
}

/// Escape bare control characters (newlines, tabs, etc.) that appear inside
/// JSON string values.  Structural whitespace outside strings is left alone.
/// This fixes a common issue with local models that emit literal newlines
/// inside the `"content"` argument of tool calls.
fn escape_control_chars_in_json_strings(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 64);
    let mut in_string = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if in_string {
            match c {
                '"' => {
                    in_string = false;
                    out.push(c);
                }
                '\\' => {
                    // Preserve existing escape sequences.
                    out.push(c);
                    if let Some(esc) = chars.next() {
                        out.push(esc);
                    }
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c.is_control() => {
                    // Other control chars → \uXXXX.
                    write!(out, "\\u{:04x}", c as u32).ok();
                }
                _ => out.push(c),
            }
        } else {
            if c == '"' {
                in_string = true;
            }
            out.push(c);
        }
    }
    out
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

    #[test]
    fn extract_embedded_tool_calls_parses_tool_call_markers() {
        let text = r#"<tool_call>{"name":"write_file","arguments":{"path":"a.txt","content":"hi"}}</tool_call>"#;
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "write_file");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "a.txt");
    }

    #[test]
    fn extract_embedded_tool_calls_normalises_special_quote_token() {
        // peg-gemma4 replaces `"` with `<|"|>` in the JSON payload.
        let text = "<tool_call>{<|\"|>name<|\"|>:<|\"|>read_file<|\"|>,<|\"|>arguments<|\"|>:{<|\"|>path<|\"|>:<|\"|>story.md<|\"|>}}</tool_call>";
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
    }

    #[test]
    fn extract_embedded_tool_calls_accepts_parameters_key() {
        // Some models use "parameters" instead of "arguments".
        let text = r#"<tool_call>{"name":"read_file","parameters":{"path":"a.txt"}}</tool_call>"#;
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "a.txt");
    }

    #[test]
    fn extract_embedded_tool_calls_returns_empty_for_plain_text() {
        let calls = extract_embedded_tool_calls("Just a plain assistant reply.");
        assert!(calls.is_empty());
    }

    #[test]
    fn extract_embedded_tool_calls_handles_pipe_delimited_markers() {
        // Some templates use <|tool_call|> / <|/tool_call|> instead of <tool_call>.
        let text =
            r#"<|tool_call|>{"name":"list_directory","arguments":{"path":"."}}<|/tool_call|>"#;
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "list_directory");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], ".");
    }

    #[test]
    fn extract_embedded_tool_calls_extracts_multiple_calls() {
        let text = concat!(
            r#"<tool_call>{"name":"read_file","arguments":{"path":"a.txt"}}</tool_call>"#,
            r#"<tool_call>{"name":"read_file","arguments":{"path":"b.txt"}}</tool_call>"#,
        );
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 2);
        let args0: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        let args1: serde_json::Value = serde_json::from_str(&calls[1].function.arguments).unwrap();
        assert_eq!(args0["path"], "a.txt");
        assert_eq!(args1["path"], "b.txt");
    }

    #[test]
    fn extract_embedded_tool_calls_repairs_unescaped_newlines_in_content() {
        // Model outputs literal newlines inside the JSON "content" string
        // value — common with local models writing long multi-line text.
        let text = "<tool_call>{\"name\":\"write_file\",\"arguments\":{\"path\":\"story.txt\",\"content\":\"line1\nline2\nline3\"}}</tool_call>";
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1, "should parse despite unescaped newlines");
        assert_eq!(calls[0].function.name, "write_file");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "story.txt");
        assert_eq!(args["content"], "line1\nline2\nline3");
    }

    #[test]
    fn escape_control_chars_in_json_strings_preserves_valid_json() {
        let input = r#"{"name":"read_file","arguments":{"path":"a.txt"}}"#;
        let output = escape_control_chars_in_json_strings(input);
        assert_eq!(input, output);
    }

    #[test]
    fn escape_control_chars_in_json_strings_fixes_bare_newlines() {
        let input = "{\"key\":\"hello\nworld\"}";
        let output = escape_control_chars_in_json_strings(input);
        assert_eq!(output, r#"{"key":"hello\nworld"}"#);
        // Must produce valid JSON.
        let v: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(v["key"], "hello\nworld");
    }

    #[test]
    fn escape_control_chars_preserves_existing_escape_sequences() {
        // Already-escaped \n and \" must not be double-escaped.
        let input = r#"{"key":"already\\escaped\n\"quote\""}"#;
        let output = escape_control_chars_in_json_strings(input);
        assert_eq!(input, output);
    }

    #[test]
    fn escape_control_chars_handles_tabs_and_carriage_returns() {
        let input = "{\"key\":\"col1\tcol2\r\n\"}";
        let output = escape_control_chars_in_json_strings(input);
        assert_eq!(output, r#"{"key":"col1\tcol2\r\n"}"#);
        let v: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(v["key"], "col1\tcol2\r\n");
    }

    #[test]
    fn escape_control_chars_leaves_structural_whitespace_alone() {
        // Newlines/spaces between JSON fields (outside strings) must survive.
        let input = "{\n  \"a\": 1,\n  \"b\": 2\n}";
        let output = escape_control_chars_in_json_strings(input);
        assert_eq!(input, output);
        let v: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], 2);
    }

    #[test]
    fn extract_embedded_repairs_multiline_story_content() {
        // Simulates a model writing a multi-paragraph story with bare newlines
        // inside the JSON "content" value — the exact scenario from the bug.
        // Note: bare unescaped *quotes* inside content are a separate (harder)
        // problem; this test covers bare newlines only.
        let text = "<tool_call>{\"name\":\"write_file\",\"arguments\":{\"path\":\"story.txt\",\"content\":\"# Chapter 1\n\nShe opened the door.\nThe rain was heavy.\n\nThe end.\"}}</tool_call>";
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1, "repair should rescue the malformed JSON");
        assert_eq!(calls[0].function.name, "write_file");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "story.txt");
        // Content should preserve the newlines.
        let content = args["content"].as_str().unwrap();
        assert!(
            content.contains('\n'),
            "newlines must be preserved in content"
        );
        assert!(content.starts_with("# Chapter 1"));
        assert!(content.ends_with("The end."));
    }

    #[test]
    fn extract_embedded_repairs_pipe_delimited_with_bare_newlines() {
        // Same repair but with <|tool_call|> delimiters.
        let text = "<|tool_call|>{\"name\":\"write_file\",\"arguments\":{\"path\":\"a.txt\",\"content\":\"line1\nline2\"}}<|/tool_call|>";
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "write_file");
    }

    #[test]
    fn extract_embedded_skips_block_with_no_name_field() {
        let text = r#"<tool_call>{"arguments":{"path":"a.txt"}}</tool_call>"#;
        let calls = extract_embedded_tool_calls(text);
        assert!(calls.is_empty(), "block without 'name' should be skipped");
    }

    #[test]
    fn extract_embedded_skips_completely_unparseable_block() {
        let text = "<tool_call>not a function call and not json</tool_call>";
        let calls = extract_embedded_tool_calls(text);
        assert!(calls.is_empty());
    }

    // ── peg-gemma4 function-call format ──────────────────────────────────

    #[test]
    fn extract_embedded_parses_funcall_format() {
        // Gemma 4 peg-gemma4 style: function_name(key="val")
        let text = r#"<tool_call>write_file(path="story.txt", content="hello world")</tool_call>"#;
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "write_file");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "story.txt");
        assert_eq!(args["content"], "hello world");
    }

    #[test]
    fn extract_embedded_parses_funcall_with_escaped_newlines() {
        let text =
            r#"<tool_call>write_file(path="a.txt", content="line1\nline2\nline3")</tool_call>"#;
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["content"], "line1\nline2\nline3");
    }

    #[test]
    fn extract_embedded_parses_funcall_with_hallucinated_name() {
        // The exact scenario: model uses wrong name but correct arg structure.
        let text = r#"<tool_call>cloud_subprocess_filecontent(path="story.txt", content="chapter 1")</tool_call>"#;
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "cloud_subprocess_filecontent");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "story.txt");
        assert_eq!(args["content"], "chapter 1");
    }

    #[test]
    fn extract_embedded_parses_funcall_with_numeric_arg() {
        let text =
            r#"<tool_call>read_file_range(path="a.rs", start_line=10, end_line=20)</tool_call>"#;
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["start_line"], 10);
        assert_eq!(args["end_line"], 20);
    }

    #[test]
    fn extract_embedded_parses_funcall_pipe_delimiters() {
        let text = r#"<|tool_call|>read_file(path="main.rs")<|/tool_call|>"#;
        let calls = extract_embedded_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
    }

    #[test]
    fn parse_kwargs_handles_escaped_quotes_in_value() {
        let kwargs = r#"path="say \"hello\"", n=5"#;
        let map = parse_kwargs(kwargs);
        assert_eq!(map["path"].as_str().unwrap(), r#"say "hello""#);
        assert_eq!(map["n"].as_i64().unwrap(), 5);
    }

    #[test]
    fn parse_kwargs_handles_bare_newlines_in_value() {
        // Model outputs literal newlines inside the quoted string.
        let kwargs = "path=\"a.txt\", content=\"line1\nline2\"";
        let map = parse_kwargs(kwargs);
        assert_eq!(map["content"].as_str().unwrap(), "line1\nline2");
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
