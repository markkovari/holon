//! mail-parse — extract sender, subject, and text bodies from raw MIME emails.

#[allow(warnings)]
mod bindings;

use crate::bindings::exports::mail::parse::parser::{Guest, Email, ParseError};
use mailparse::*;

struct Component;

fn get_header(parsed: &ParsedMail, name: &str) -> Option<String> {
    parsed.headers.iter().find(|h| h.get_key().eq_ignore_ascii_case(name)).map(|h| h.get_value())
}

fn extract_parts(parsed: &ParsedMail, text: &mut String, html: &mut Option<String>) {
    if parsed.ctype.mimetype == "text/plain" {
        if text.is_empty() {
            *text = parsed.get_body().unwrap_or_default();
        }
    } else if parsed.ctype.mimetype == "text/html"
        && html.is_none() {
            *html = Some(parsed.get_body().unwrap_or_default());
        }
    for subpart in &parsed.subparts {
        extract_parts(subpart, text, html);
    }
}

impl Guest for Component {
    fn parse(raw: Vec<u8>) -> Result<Email, ParseError> {
        let parsed = parse_mail(&raw)
            .map_err(|e| ParseError::Malformed(e.to_string()))?;
            
        let sender = get_header(&parsed, "From")
            .ok_or_else(|| ParseError::Malformed("Missing From header".to_string()))?;
            
        let subject = get_header(&parsed, "Subject").unwrap_or_default();
        let in_reply_to = get_header(&parsed, "In-Reply-To");
        
        let mut text = String::new();
        let mut html = None;
        extract_parts(&parsed, &mut text, &mut html);
        
        Ok(Email {
            sender,
            subject,
            text,
            html,
            in_reply_to,
        })
    }
}

bindings::export!(Component with_types_in bindings);
