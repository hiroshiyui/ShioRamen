use anyhow::Result;
use rustyline::{error::ReadlineError, DefaultEditor};

use crate::client::{AgentTurn, LlamaClient, Message, ToolDef};
use crate::context;
use crate::tools::{all_tools, ToolExecutor};

pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are ShioRamen, a sharp, focused local coding assistant running entirely offline. \
Be concise and accurate. Provide working code with minimal prose unless the user asks \
for explanation. Always fence code blocks with the correct language identifier.";

pub struct ChatSession {
    client: LlamaClient,
    messages: Vec<Message>,
    temperature: f32,
    /// When Some, the agentic loop is active and these tools are offered to the model.
    executor: Option<ToolExecutor>,
}

impl ChatSession {
    pub fn new(
        client: LlamaClient,
        temperature: f32,
        system_prompt: String,
        executor: Option<ToolExecutor>,
    ) -> Self {
        Self {
            client,
            messages: vec![Message::system(system_prompt)],
            temperature,
            executor,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        let mut rl = DefaultEditor::new()?;

        println!("ShioRamen — local coding assistant");
        if self.executor.is_some() {
            println!("Commands: /reset  clear history | /include <path>  load file(s) | /tools  list tools | /exit  quit");
            println!("Tool use: ON");
        } else {
            println!("Commands: /reset  clear history | /include <path>  load file(s) | /exit  quit");
        }
        println!();

        loop {
            match rl.readline("you> ") {
                Ok(line) => {
                    let input = line.trim().to_string();
                    if input.is_empty() {
                        continue;
                    }
                    rl.add_history_entry(&input)?;

                    match input.as_str() {
                        "/exit" | "/quit" => {
                            println!("Sayonara!");
                            break;
                        }
                        "/reset" => {
                            self.messages.truncate(1);
                            println!("[history cleared]");
                            continue;
                        }
                        "/tools" => {
                            let tools = all_tools();
                            println!("Available tools ({}):", tools.len());
                            for t in &tools {
                                println!("  • {} — {}", t.function.name, t.function.description);
                            }
                            continue;
                        }
                        _ if input.starts_with("/include ") => {
                            let path_str = input["/include ".len()..].trim();
                            self.cmd_include(path_str);
                            continue;
                        }
                        _ => {}
                    }

                    self.messages.push(Message::user(input));

                    let result = match &self.executor {
                        Some(_) => self.run_agent_turn().await,
                        None    => self.run_stream_turn().await,
                    };

                    if let Err(e) = result {
                        eprintln!("\n[error] {e}");
                        self.messages.pop(); // discard the failed user turn
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    println!();
                    continue;
                }
                Err(ReadlineError::Eof) => {
                    println!("Sayonara!");
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }

        Ok(())
    }

    // ── Turn implementations ──────────────────────────────────────────────────

    async fn run_stream_turn(&mut self) -> Result<()> {
        print_flush("shio> ");
        let response = self.client.chat_stream(&self.messages, self.temperature).await?;
        self.messages.push(Message::assistant(response));
        Ok(())
    }

    async fn run_agent_turn(&mut self) -> Result<()> {
        let executor = self.executor.as_ref().expect("called without executor");
        let tools: Vec<ToolDef> = all_tools();

        // Agentic loop: keep calling the model until it produces a text response.
        loop {
            eprint!("shio> ");
            flush_stderr();

            match self.client.chat_agent(&self.messages, self.temperature, &tools).await? {
                AgentTurn::Text(text) => {
                    // Clear the "shio> " prompt and print the response.
                    println!("{text}");
                    self.messages.push(Message::assistant(&text));
                    return Ok(());
                }

                AgentTurn::ToolCalls(calls) => {
                    eprintln!(); // newline after "shio> "
                    // Record the assistant's tool-call turn.
                    self.messages.push(Message::assistant_tool_calls(calls.clone()));

                    // Execute each tool and push the result.
                    for call in &calls {
                        let result = executor.execute(call);
                        self.messages.push(Message::tool_result(&call.id, result));
                    }
                    // Loop: give the model another turn with tool results in context.
                }
            }
        }
    }

    // ── /include command ─────────────────────────────────────────────────────

    fn cmd_include(&mut self, path_str: &str) {
        let path = std::path::Path::new(path_str);
        match context::collect(path) {
            Err(e) => eprintln!("[error] {e}"),
            Ok(files) if files.is_empty() => {
                println!("[no source files found in {path_str}]");
            }
            Ok(files) => {
                let count   = files.len();
                let listing: Vec<String> = files
                    .iter()
                    .map(|(p, c)| format!("  {} ({} B)", p.display(), c.len()))
                    .collect();
                let content = context::format_as_blocks(&files);
                self.messages.push(Message::user(content));
                self.messages.push(Message::assistant(
                    format!("Understood. I've loaded {count} file(s) and am ready for your questions."),
                ));
                println!("[included {count} file(s) from {path_str}]");
                for line in &listing {
                    println!("{line}");
                }
            }
        }
    }
}

fn print_flush(s: &str) {
    use std::io::Write;
    print!("{s}");
    std::io::stdout().flush().ok();
}

fn flush_stderr() {
    use std::io::Write;
    std::io::stderr().flush().ok();
}
