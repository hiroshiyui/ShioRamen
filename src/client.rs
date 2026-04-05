use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};

// ── Public message type ──────────────────────────────────────────────────────

/// A single message in the conversation. `content` is None when the assistant
/// responds with tool_calls instead of text. `tool_call_id` is set for tool-
/// result messages (role = "tool").
#[derive(Debug, Serialize, Clone)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
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
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(id.into()),
        }
    }

    fn text(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(content.into()),
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
    pub name: &'static str,
    pub description: &'static str,
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
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct CompletionMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ToolCallItem>>,
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
}

// ── Client ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct LlamaClient {
    http: Client,
    base_url: String,
}

impl LlamaClient {
    pub fn new(base_url: String) -> Self {
        Self { http: Client::new(), base_url }
    }

    /// One agentic turn: send messages (with tools), return either text or tool calls.
    /// Uses non-streaming so the full response can be inspected before acting.
    pub async fn chat_agent(
        &self,
        messages: &[Message],
        temperature: f32,
        tools: &[ToolDef],
    ) -> Result<AgentTurn> {
        let request = ChatRequest {
            model: "local",
            messages,
            stream: false,
            temperature,
            tools: Some(tools),
            tool_choice: Some("auto"),
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

        let choice = body
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("empty response from server"))?;

        if choice.finish_reason.as_deref() == Some("tool_calls") {
            let calls = choice.message.tool_calls.ok_or_else(|| {
                anyhow!("finish_reason=tool_calls but no tool_calls field in response")
            })?;
            return Ok(AgentTurn::ToolCalls(calls));
        }

        // A non-tool-call turn must have a non-empty text response.
        let text = choice
            .message
            .content
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("model returned no content and no tool calls"))?;
        Ok(AgentTurn::Text(text))
    }

    /// Non-streaming completion without printing; returns the full response.
    /// Used by `edit` to post-process model output.
    pub async fn chat_collect(&self, messages: &[Message], temperature: f32) -> Result<String> {
        let request = ChatRequest {
            model: "local",
            messages,
            stream: false,
            temperature,
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
    pub async fn chat_stream(&self, messages: &[Message], temperature: f32) -> Result<String> {
        let text = self.chat_stream_cb(messages, temperature, print_flush).await?;
        println!();
        Ok(text)
    }

    /// Streaming chat with a per-token callback instead of printing.
    /// The callback is called synchronously for each token as it arrives.
    /// Returns the fully assembled text.
    pub async fn chat_stream_cb<F>(
        &self,
        messages: &[Message],
        temperature: f32,
        mut on_token: F,
    ) -> Result<String>
    where
        F: FnMut(&str),
    {
        let request = ChatRequest {
            model: "local",
            messages,
            stream: true,
            temperature,
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
        let mut line_buf  = String::new();
        let mut full_text = String::new();

        while let Some(chunk) = byte_stream.next().await {
            line_buf.push_str(&String::from_utf8_lossy(&chunk?));
            while let Some(pos) = line_buf.find('\n') {
                let raw = line_buf[..pos].trim().to_string();
                line_buf = line_buf[pos + 1..].to_string();

                let Some(data) = raw.strip_prefix("data: ") else { continue };
                if data == "[DONE]" { break }

                let Ok(sc) = serde_json::from_str::<StreamChunk>(data) else { continue };
                if let Some(token) = sc.choices.first().and_then(|c| c.delta.content.as_deref()) {
                    on_token(token);
                    full_text.push_str(token);
                }
            }
        }

        Ok(full_text)
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
        assert_eq!(m.content.as_deref(), Some("be helpful"));
        assert!(m.tool_calls.is_none());
        assert!(m.tool_call_id.is_none());
    }

    #[test]
    fn user_message_serialises_correctly() {
        let m = Message::user("hello");
        assert_eq!(m.role, "user");
        assert_eq!(m.content.as_deref(), Some("hello"));
    }

    #[test]
    fn assistant_message_serialises_correctly() {
        let m = Message::assistant("hi there");
        assert_eq!(m.role, "assistant");
        assert_eq!(m.content.as_deref(), Some("hi there"));
    }

    #[test]
    fn tool_result_message_has_tool_call_id() {
        let m = Message::tool_result("call_abc", "file contents");
        assert_eq!(m.role, "tool");
        assert_eq!(m.content.as_deref(), Some("file contents"));
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
}
