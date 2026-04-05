use anyhow::Result;
use rustyline::{error::ReadlineError, DefaultEditor};

use crate::client::{LlamaClient, Message};

const SYSTEM_PROMPT: &str = "\
You are ShioRamen, a sharp, focused local coding assistant running entirely offline. \
Be concise and accurate. Provide working code with minimal prose unless the user asks \
for explanation. Always fence code blocks with the correct language identifier.";

pub struct ChatSession {
    client: LlamaClient,
    messages: Vec<Message>,
    temperature: f32,
}

impl ChatSession {
    pub fn new(client: LlamaClient, temperature: f32) -> Self {
        Self {
            client,
            messages: vec![Message {
                role: "system".to_string(),
                content: SYSTEM_PROMPT.to_string(),
            }],
            temperature,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        let mut rl = DefaultEditor::new()?;

        println!("ShioRamen — local coding assistant");
        println!("Commands: /reset  clear history | /exit  quit");
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
                            self.messages.truncate(1); // keep system prompt
                            println!("[history cleared]");
                            continue;
                        }
                        _ => {}
                    }

                    self.messages.push(Message {
                        role: "user".to_string(),
                        content: input,
                    });

                    print_flush("shio> ");

                    match self.client.chat_stream(&self.messages, self.temperature).await {
                        Ok(response) => {
                            self.messages.push(Message {
                                role: "assistant".to_string(),
                                content: response,
                            });
                        }
                        Err(e) => {
                            eprintln!("\n[error] {e}");
                            self.messages.pop(); // discard the failed user turn
                        }
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    // Ctrl-C: cancel current line, stay in the loop.
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
}

fn print_flush(s: &str) {
    use std::io::Write;
    print!("{s}");
    std::io::stdout().flush().ok();
}
