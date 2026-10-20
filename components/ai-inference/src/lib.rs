//! `ai:assist` — domain-level AI verbs over a provider-agnostic LLM.

#[allow(warnings)]
mod bindings;

use bindings::exports::ai::inference::inference::{AssistError, Guest, LabelScore, Length};
use bindings::llm::inference::inference::{self as inference, InferError, Options};

struct Component;

/// All-defaults `Options`: 0 / empty means "let the provider decide".
fn default_options() -> Options {
    Options { model: String::new(), temperature: 0, max_tokens: 0, stop: Vec::new(), seed: 0 }
}

fn infer_failed(e: InferError) -> AssistError {
    AssistError::InferenceFailed(format!("{e:?}"))
}

impl Guest for Component {
    fn summarize(text: String, len: Length, focus: String) -> Result<String, AssistError> {
        let mut system = String::from("Summarize the following text.");
        if !focus.is_empty() {
            system.push_str(&format!(" Focus on: {focus}."));
        }
        let hint = match len {
            Length::Brief => " Summarize in one sentence.",
            Length::Normal => " Summarize in a short paragraph.",
            Length::Detailed => " Summarize in detail.",
        };
        system.push_str(hint);

        let completion =
            inference::complete(&text, &system, &default_options()).map_err(infer_failed)?;
        Ok(completion.text.trim().to_string())
    }

    fn classify(text: String, labels: Vec<String>) -> Result<LabelScore, AssistError> {
        if labels.is_empty() {
            return Err(AssistError::InvalidRequest("no labels".to_string()));
        }
        // "Classify" + a "labels:" line: the mock replies with the first label.
        let system = format!(
            "Classify the text into exactly one label. labels: {}. Reply with only the label.",
            labels.join(", ")
        );

        let completion =
            inference::complete(&text, &system, &default_options()).map_err(infer_failed)?;
        let reply = completion.text.trim().to_string();
        let reply_lower = reply.to_lowercase();

        if let Some(label) = labels.iter().find(|l| l.to_lowercase() == reply_lower) {
            return Ok(LabelScore { label: label.clone(), confidence: 1000 });
        }
        if let Some(label) = labels.iter().find(|l| reply_lower.contains(&l.to_lowercase())) {
            return Ok(LabelScore { label: label.clone(), confidence: 700 });
        }
        Err(AssistError::UnexpectedOutput(reply))
    }

    fn extract(text: String, fields: Vec<String>) -> Result<Vec<(String, String)>, AssistError> {
        if fields.is_empty() {
            return Err(AssistError::InvalidRequest("no fields".to_string()));
        }
        // "Extract" + "fields:" makes the mock return JSON; the list ends its line with a period so the mock cuts it there.
        let system = format!(
            "Extract the requested fields and reply with only a JSON object. fields: {}.",
            fields.join(", ")
        );

        let completion =
            inference::complete(&text, &system, &default_options()).map_err(infer_failed)?;
        let reply = completion.text.trim().to_string();

        let parsed: serde_json::Value = match serde_json::from_str(&reply) {
            Ok(v) => v,
            Err(_) => return Err(AssistError::UnexpectedOutput(reply)),
        };
        let obj = match parsed.as_object() {
            Some(o) => o,
            None => return Err(AssistError::UnexpectedOutput(reply)),
        };

        let mut out = Vec::with_capacity(fields.len());
        for field in &fields {
            let value = match obj.get(field) {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            out.push((field.clone(), value));
        }
        Ok(out)
    }

    fn generate(prompt: String, context: String) -> Result<String, AssistError> {
        let system = if context.is_empty() {
            String::new()
        } else {
            format!("Use this context:\n{context}")
        };
        let completion =
            inference::complete(&prompt, &system, &default_options()).map_err(infer_failed)?;
        Ok(completion.text.trim().to_string())
    }

    fn rewrite(text: String, style: String) -> Result<String, AssistError> {
        let system = format!("Rewrite the text in this style: {style}.");
        let completion =
            inference::complete(&text, &system, &default_options()).map_err(infer_failed)?;
        Ok(completion.text.trim().to_string())
    }

    fn embed(text: String) -> Result<Vec<f32>, AssistError> {
        inference::embed(&text, &default_options()).map_err(infer_failed)
    }
}

bindings::export!(Component with_types_in bindings);
