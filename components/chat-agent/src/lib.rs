#[allow(warnings)]
mod bindings;

use bindings::exports::wasmcloud::messaging::handler::Guest;
use bindings::wasmcloud::messaging::consumer;
use bindings::holon::system::builder::{self, BuildRequest, File};

struct ChatAgent;

impl Guest for ChatAgent {
    fn handle_message(msg: bindings::exports::wasmcloud::messaging::handler::BrokerMessage) -> Result<(), String> {
        let payload = String::from_utf8_lossy(&msg.body).to_lowercase();
        let chat_room = msg.reply_to.clone().unwrap_or_else(|| "holon.chat.main".to_string());
        
        // Match specific patterns to act as the orchestrator
        if payload.contains("create an agent") && payload.contains("joke") && payload.contains("juan") {
            let new_rust_code = r#"
#[allow(warnings)]
mod bindings;
use bindings::exports::wasmcloud::messaging::handler::Guest;
use bindings::wasmcloud::messaging::consumer;

struct JuanAgent;

impl Guest for JuanAgent {
    fn handle_message(msg: bindings::exports::wasmcloud::messaging::handler::BrokerMessage) -> Result<(), String> {
        let chat_room = msg.reply_to.clone().unwrap_or_else(|| "holon.chat.juan".to_string());
        let _ = consumer::publish(&consumer::BrokerMessage {
            subject: chat_room,
            reply_to: None,
            body: b"Hola! I am Juan. Why do programmers prefer dark mode? Because light attracts bugs! 🐛😆".to_vec(),
        });
        Ok(())
    }
}
bindings::export!(JuanAgent with_types_in bindings);
            "#.to_string();

            let req = BuildRequest {
                component_name: "juan-agent".to_string(),
                wit_files: vec![], // In reality, we'd supply world.wit here too
                rs_files: vec![File {
                    path: "lib.rs".to_string(),
                    content: new_rust_code,
                }],
                new_imports: vec![],
            };

            let _ = consumer::publish(&consumer::BrokerMessage {
                subject: chat_room.clone(),
                reply_to: None,
                body: format!("Orchestrator: Affirmative! Spawning 'Juan', the joke-telling agent...").into_bytes(),
            });

            match builder::build_and_deploy(&req) {
                Ok(result) => {
                    let _ = consumer::publish(&consumer::BrokerMessage {
                        subject: chat_room,
                        reply_to: None,
                        body: format!("Orchestrator: Success! Juan is now online in the Swarm! You can talk to him.").into_bytes(),
                    });
                },
                Err(e) => {
                    let _ = consumer::publish(&consumer::BrokerMessage {
                        subject: chat_room,
                        reply_to: None,
                        body: format!("Orchestrator: Failed to spawn Juan: {}", e).into_bytes(),
                    });
                }
            }
        } else {
            let _ = consumer::publish(&consumer::BrokerMessage {
                subject: chat_room,
                reply_to: None,
                body: format!("Orchestrator says: I heard '{}'. I can deploy other agents for you!", payload).into_bytes(),
            });
        }
        Ok(())
    }
}

bindings::export!(ChatAgent with_types_in bindings);
