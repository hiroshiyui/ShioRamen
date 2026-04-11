// SPDX-License-Identifier: GPL-3.0-or-later
//! In-process inference backend powered by `llama-cpp-2`.
//!
//! `InProcessEngine` loads a GGUF model into the current process and runs
//! token decoding directly — no `llama-server` subprocess, no HTTP hop.
//! It implements the same [`Engine`] trait as [`crate::client::LlamaClient`],
//! so every command (`chat`, `ask`, `edit`, TUI) switches between the two
//! at the `Arc<dyn Engine>` construction site and nothing else changes.
//!
//! ## Concurrency model
//!
//! `LlamaBackend` is a process-wide singleton held in a `OnceLock` — calling
//! `LlamaBackend::init()` twice is an error, so we route all access through
//! [`backend()`].  The loaded `LlamaModel` is kept in an `Arc` on the engine
//! struct and cloned into a `tokio::task::spawn_blocking` thread for every
//! generation request.  Each request creates a fresh `LlamaContext` inside
//! the blocking task and lets it die with the task; tokens are streamed back
//! to the async caller through an unbounded mpsc channel.
//!
//! Creating a fresh context per request is the simplest safe shape — it
//! avoids self-referential `LlamaContext<'a>` + `LlamaModel` structs and
//! sidesteps the `!Send` nature of the context.  The trade-off is that the
//! KV cache is *not* reused across turns: a multi-turn chat re-evaluates
//! the full prompt each turn.  That's acceptable for phase 2 validation;
//! phase 3 can introduce a pooled / persistent context if benchmarks show
//! it matters.
//!
//! ## Known limitations (phase 2)
//!
//! * **No tool calling.**  `chat_agent_stream` returns an explicit error —
//!   use `--no-tools` or switch to a remote llama-server via `--remote-url`.
//!   Phase 3 may add grammar-constrained or regex-extracted tool calls.
//! * **No mmproj / vision.**  Multimodal projectors are not wired up.
//! * **No per-turn abort.**  Pressing Esc in the TUI will drop the receiver,
//!   which causes the decode loop to see `send() -> Err` and exit at the
//!   next token boundary — so it *does* stop, just not instantly.  A proper
//!   abort callback lands in a later phase.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, anyhow};
use futures_util::future::BoxFuture;
use tokio::sync::mpsc;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaChatTemplate, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use crate::client::{
    AgentTurn, GenerationSettings, Message, SamplingParams, ServerProps, SlotInfo, ToolDef,
};
use crate::engine::{Engine, TokenSink};

// ── Global backend ───────────────────────────────────────────────────────────

/// Shared `LlamaBackend`.  `LlamaBackend::init()` is allowed exactly once per
/// process (it sets global state inside llama.cpp), so we serialize access
/// through a `OnceLock`.  The backend is intentionally never dropped — it
/// lives for the lifetime of the program.
static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();

fn backend() -> &'static LlamaBackend {
    BACKEND.get_or_init(|| LlamaBackend::init().expect("failed to initialize llama.cpp backend"))
}

// ── Engine config ────────────────────────────────────────────────────────────

/// Parameters needed to load a model and create contexts from it.
/// Populated from `shio.toml` + CLI flags at engine construction time.
#[derive(Debug, Clone)]
pub struct InProcessConfig {
    /// Path to the GGUF model file.
    pub model_path: PathBuf,
    /// Context window size in tokens.
    pub n_ctx: u32,
    /// Number of layers to offload to the GPU.  Capped at model depth by
    /// llama.cpp itself, so `u32::MAX` means "offload everything".
    pub n_gpu_layers: u32,
    /// Optional thread count for the decode loop.  `None` = llama.cpp default.
    pub n_threads: Option<i32>,
}

// ── Engine ───────────────────────────────────────────────────────────────────

/// In-process inference backend.  Construct with [`InProcessEngine::load`],
/// wrap in `Arc` and use as `DynEngine` just like `LlamaClient`.
pub struct InProcessEngine {
    model: Arc<LlamaModel>,
    chat_template: LlamaChatTemplate,
    model_display_name: String,
    n_ctx: u32,
    n_threads: Option<i32>,
}

impl InProcessEngine {
    /// Load a GGUF model and prepare it for inference.  Blocks the calling
    /// thread until the model is fully memory-mapped and GPU-offloaded, so
    /// it's fine to call from `fn main` or `tokio::task::spawn_blocking` —
    /// just not from inside an async task you care about responsiveness on.
    pub fn load(cfg: InProcessConfig) -> Result<Self> {
        let backend_ref = backend();

        let model_params = LlamaModelParams::default().with_n_gpu_layers(cfg.n_gpu_layers);
        // `load_from_file` wants a non-mutable `&LlamaModelParams` so the
        // pin is only needed if we ever append kv_overrides.  We don't,
        // but the upstream example pins anyway — keeping the shape matches
        // the documented usage and costs nothing.
        let model_params = pin!(model_params);

        let model = LlamaModel::load_from_file(backend_ref, &cfg.model_path, &model_params)
            .with_context(|| format!("failed to load model from {}", cfg.model_path.display()))?;

        // Prefer the template baked into the GGUF.  If the model author
        // forgot to embed one we surface the error immediately rather than
        // silently falling back to ChatML, because the wrong template
        // produces garbage output.
        let chat_template = model
            .chat_template(None)
            .context("model has no chat template embedded in GGUF metadata")?;

        let model_display_name = cfg
            .model_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("local")
            .to_string();

        Ok(Self {
            model: Arc::new(model),
            chat_template,
            model_display_name,
            n_ctx: cfg.n_ctx,
            n_threads: cfg.n_threads,
        })
    }

    /// Materialize the chat template on the async side (cheap) so the
    /// blocking task only has to deal with an owned `String` prompt.
    fn build_prompt(&self, messages: &[Message]) -> Result<String> {
        let chat: Vec<LlamaChatMessage> = messages
            .iter()
            .filter_map(|m| {
                // Phase 2 is text-only: drop any image parts, drop tool-call
                // messages (no content).  Phase 3 will plumb multimodal +
                // tool-call rendering through a custom template pass.
                let text = m.content.as_ref().and_then(|c| c.as_text())?;
                LlamaChatMessage::new(m.role.clone(), text.to_string()).ok()
            })
            .collect();

        if chat.is_empty() {
            return Err(anyhow!("no renderable messages for chat template"));
        }

        self.model
            .apply_chat_template(&self.chat_template, &chat, /* add_ass */ true)
            .map_err(|e| anyhow!("apply_chat_template: {e}"))
    }

    /// Core streaming generate.  Returns the full assembled text and feeds
    /// tokens to `on_token` as they arrive.
    async fn generate(
        &self,
        messages: &[Message],
        sampling: SamplingParams,
        on_token: TokenSink<'_>,
    ) -> Result<String> {
        let prompt = self.build_prompt(messages)?;

        // Owned data to move into the blocking task.
        let model = self.model.clone();
        let n_ctx = self.n_ctx;
        let n_threads = self.n_threads;
        let temperature = sampling.temperature;
        let top_p = sampling.top_p;
        let repeat_penalty = sampling.repeat_penalty;

        let (tx, mut rx) = mpsc::unbounded_channel::<DecodeEvent>();

        let handle = tokio::task::spawn_blocking(move || {
            blocking_decode(
                &model,
                prompt,
                n_ctx,
                n_threads,
                temperature,
                top_p,
                repeat_penalty,
                tx,
            )
        });

        // Drain the channel on the async side.  `on_token` lives here with
        // its caller lifetime intact — it never crosses the thread boundary.
        let mut full = String::new();
        while let Some(evt) = rx.recv().await {
            match evt {
                DecodeEvent::Token(piece) => {
                    on_token(&piece);
                    full.push_str(&piece);
                }
            }
        }

        // Surface decode errors after the channel closes.  `join` errors
        // bubble up as engine errors.
        handle
            .await
            .map_err(|e| anyhow!("blocking decode task panicked: {e}"))??;

        if full.is_empty() {
            return Err(anyhow!("model returned no tokens"));
        }
        Ok(full)
    }
}

/// Internal decode-loop event.  Extended in later phases to carry usage
/// stats, abort signals, etc.
enum DecodeEvent {
    Token(String),
}

/// Pure blocking decode loop.  Runs on `tokio::task::spawn_blocking`.
/// Creates a fresh `LlamaContext` scoped to this call; the context dies
/// when the function returns.
#[allow(clippy::too_many_arguments)]
fn blocking_decode(
    model: &LlamaModel,
    prompt: String,
    n_ctx: u32,
    n_threads: Option<i32>,
    temperature: f32,
    top_p: Option<f32>,
    repeat_penalty: Option<f32>,
    tx: mpsc::UnboundedSender<DecodeEvent>,
) -> Result<()> {
    let backend_ref = backend();

    let mut ctx_params = LlamaContextParams::default().with_n_ctx(NonZeroU32::new(n_ctx));
    if let Some(t) = n_threads {
        ctx_params = ctx_params.with_n_threads(t).with_n_threads_batch(t);
    }

    let mut ctx = model
        .new_context(backend_ref, ctx_params)
        .context("failed to create llama context")?;

    let tokens = model
        .str_to_token(&prompt, AddBos::Always)
        .map_err(|e| anyhow!("tokenize prompt: {e}"))?;

    let n_ctx_i = ctx.n_ctx() as i32;
    if tokens.len() as i32 >= n_ctx_i {
        return Err(anyhow!(
            "prompt is {} tokens but context window is only {} — raise `ctx` or shorten the prompt",
            tokens.len(),
            n_ctx_i
        ));
    }

    // Seed the KV cache with the full prompt in one decode call.
    let mut batch = LlamaBatch::new(512, 1);
    let last_index = (tokens.len() - 1) as i32;
    for (i, token) in (0_i32..).zip(tokens.into_iter()) {
        let is_last = i == last_index;
        batch
            .add(token, i, &[0], is_last)
            .map_err(|e| anyhow!("batch.add: {e}"))?;
    }
    ctx.decode(&mut batch)
        .context("initial prompt decode failed")?;

    // Build the sampler chain.  Order matters: penalties → top-p → temp/dist,
    // matching llama-server's default.  If temperature is 0 we go greedy.
    let mut chain: Vec<LlamaSampler> = Vec::new();
    if let Some(rp) = repeat_penalty {
        // 64 = penalty_last_n, matches llama-server default.
        chain.push(LlamaSampler::penalties(64, rp, 0.0, 0.0));
    }
    if let Some(tp) = top_p {
        chain.push(LlamaSampler::top_p(tp, 1));
    }
    if temperature > 0.0 {
        chain.push(LlamaSampler::temp(temperature));
        chain.push(LlamaSampler::dist(1234));
    } else {
        chain.push(LlamaSampler::greedy());
    }
    let mut sampler = LlamaSampler::chain_simple(chain);

    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut n_cur = batch.n_tokens();

    loop {
        if n_cur >= n_ctx_i {
            // Hit the context window — stop cleanly instead of letting
            // llama.cpp error out on the next decode.
            break;
        }

        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(token);

        if model.is_eog_token(token) {
            break;
        }

        let piece = model
            .token_to_piece(token, &mut decoder, true, None)
            .map_err(|e| anyhow!("token_to_piece: {e}"))?;

        if tx.send(DecodeEvent::Token(piece)).is_err() {
            // Receiver dropped (caller aborted).  Stop at the next token
            // boundary — this is the phase-2 abort shim described in the
            // module docs.
            break;
        }

        batch.clear();
        batch
            .add(token, n_cur, &[0], true)
            .map_err(|e| anyhow!("batch.add: {e}"))?;
        n_cur += 1;

        ctx.decode(&mut batch).context("decode step failed")?;
    }

    Ok(())
}

// ── Engine trait impl ────────────────────────────────────────────────────────

impl Engine for InProcessEngine {
    fn chat_agent_stream<'a>(
        &'a self,
        _messages: &'a [Message],
        _sampling: SamplingParams,
        _tools: &'a [ToolDef],
        _on_token: TokenSink<'a>,
    ) -> BoxFuture<'a, Result<AgentTurn>> {
        Box::pin(async move {
            Err(anyhow!(
                "tool use is not yet supported by the in-process engine — \
                 re-run with `--no-tools`, or point `--remote-url` at a \
                 running llama-server"
            ))
        })
    }

    fn chat_collect<'a>(
        &'a self,
        messages: &'a [Message],
        sampling: SamplingParams,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let mut sink = |_s: &str| {};
            self.generate(messages, sampling, &mut sink).await
        })
    }

    fn chat_stream_cb<'a>(
        &'a self,
        messages: &'a [Message],
        sampling: SamplingParams,
        on_token: TokenSink<'a>,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move { self.generate(messages, sampling, on_token).await })
    }

    fn props(&self) -> BoxFuture<'_, Result<ServerProps>> {
        // Synthesize props from engine state so the TUI `/model` command
        // keeps working transparently.  Sampling defaults are zero because
        // they're per-request, not per-engine.
        let model = self.model_display_name.clone();
        let n_ctx = self.n_ctx;
        Box::pin(async move {
            Ok(ServerProps {
                total_slots: 1,
                default_generation_settings: GenerationSettings {
                    model,
                    n_ctx,
                    temperature: 0.0,
                    top_p: 0.0,
                    repeat_penalty: 0.0,
                },
            })
        })
    }

    fn slots(&self) -> BoxFuture<'_, Result<Vec<SlotInfo>>> {
        // Single synthetic slot — the in-process engine doesn't expose
        // per-slot KV-cache state, so `/stats` shows "0/n_ctx idle".  A
        // future revision can track real `n_past` by threading it out of
        // `blocking_decode`.
        let n_ctx = self.n_ctx;
        Box::pin(async move {
            Ok(vec![SlotInfo {
                id: 0,
                is_processing: false,
                n_ctx,
                n_past: 0,
            }])
        })
    }
}
