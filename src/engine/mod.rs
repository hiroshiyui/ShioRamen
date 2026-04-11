// SPDX-License-Identifier: GPL-3.0-or-later
//! Inference backend abstraction.
//!
//! The `Engine` trait decouples command code (chat, ask, edit, TUI) from the
//! concrete inference path.  Implementations:
//!
//! * [`crate::client::LlamaClient`] — HTTP → `llama-server` (always available).
//! * [`inprocess::InProcessEngine`] — FFI to `libllama` via the `llama-cpp-2`
//!   crate, running inference in-process with no subprocess or HTTP hop.
//!   Gated on the `inprocess` Cargo feature so default builds stay lean and
//!   don't require cmake or a C++ toolchain.
//!
//! Shipping both behind the same trait means command code (`chat`, `ask`,
//! `edit`, TUI) never cares which one is active — it just holds an
//! `Arc<dyn Engine>` and calls methods.

#[cfg(feature = "inprocess")]
pub mod inprocess;

use std::sync::Arc;

use anyhow::Result;
use futures_util::future::BoxFuture;

use crate::client::{AgentTurn, Message, SamplingParams, ServerProps, SlotInfo, ToolDef};

/// Per-token streaming callback.  Must be `Send` because the futures returned
/// from streaming methods are `Send` and capture the callback by mutable
/// reference for their lifetime.
pub type TokenSink<'a> = &'a mut (dyn FnMut(&str) + Send);

/// Shared handle to an inference backend.  Cheap to clone (Arc bump) and safe
/// to move into spawned tasks.
pub type DynEngine = Arc<dyn Engine>;

/// Inference backend.
///
/// Object-safe: method futures are erased to `BoxFuture` so callers can hold
/// `Arc<dyn Engine>` and swap implementations at runtime.  The callback-based
/// streaming shape matches the current TUI/ask wiring; a future phase may add
/// a `Stream<TokenEvent>` variant once the in-process engine lands.
pub trait Engine: Send + Sync {
    /// Streaming agentic turn with tool support.  Tokens are delivered via
    /// `on_token` as they arrive; tool-call deltas are accumulated internally
    /// and returned as `AgentTurn::ToolCalls` once the stream ends.  Falls
    /// back to `AgentTurn::Text` when the model produces plain text instead.
    fn chat_agent_stream<'a>(
        &'a self,
        messages: &'a [Message],
        sampling: SamplingParams,
        tools: &'a [ToolDef],
        on_token: TokenSink<'a>,
    ) -> BoxFuture<'a, Result<AgentTurn>>;

    /// Non-streaming completion: returns the full assembled response text.
    /// Used by `edit` (to post-process raw file content) and by the TUI's
    /// `/compact` path (to produce a conversation summary).
    fn chat_collect<'a>(
        &'a self,
        messages: &'a [Message],
        sampling: SamplingParams,
    ) -> BoxFuture<'a, Result<String>>;

    /// Streaming completion with a per-token callback.  Returns the fully
    /// assembled text once the stream ends.  No tool support — use
    /// `chat_agent_stream` when tools are in play.
    fn chat_stream_cb<'a>(
        &'a self,
        messages: &'a [Message],
        sampling: SamplingParams,
        on_token: TokenSink<'a>,
    ) -> BoxFuture<'a, Result<String>>;

    /// Fetch backend properties (model name, context size, generation
    /// defaults).  Drives the TUI `/model` command.
    fn props(&self) -> BoxFuture<'_, Result<ServerProps>>;

    /// Fetch KV-cache slot state.  Drives the TUI `/stats` command.
    fn slots(&self) -> BoxFuture<'_, Result<Vec<SlotInfo>>>;
}
