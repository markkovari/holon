//! mail-parse — extract sender, subject, and text bodies from raw MIME emails.

#[allow(warnings)]
mod bindings;

use crate::bindings::exports::mail::parse::parser::{Guest, Email, ParseError};

struct Component;

impl Guest for Component {
    fn parse(raw: Vec<u8>) -> Result<Email, ParseError> {
        let content = String::from_utf8_lossy(&raw);
        
        // Simple mock MIME parser
        let mut sender = String::new();
        let mut subject = String::new();
        let mut in_reply_to = None;
        let mut text = String::new();
        
        let mut in_body = false;
        
        for line in content.lines() {
            if in_body {
                text.push_str(line);
                text.push('\n');
            } else {
                if line.is_empty() {
                    in_body = true;
                    continue;
                }
                
                let lower = line.to_lowercase();
                if lower.starts_with("from:") {
                    sender = line[5..].trim().to_string();
                } else if lower.starts_with("subject:") {
                    subject = line[8..].trim().to_string();
                } else if lower.starts_with("in-reply-to:") {
                    in_reply_to = Some(line[12..].trim().to_string());
                }
            }
        }
        
        if sender.is_empty() {
            return Err(ParseError::Malformed("Missing From header".to_string()));
        }
        
        Ok(Email {
            sender,
            subject,
            text,
            html: None,
            in_reply_to,
        })
    }
}

bindings::export!(Component with_types_in bindings);
