//! Prompt coalescing and response unpacking for multi-task batch execution.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchItem {
    pub id: String,
    pub prompt: String,
    pub system: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskInput<'a> {
    id: &'a str,
    task: &'a str,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskOutput {
    pub id: String,
    pub text: String,
}

pub fn system_prompt() -> &'static str {
    "You are an optimized multi-task batch processing engine. \
You will receive an array of tasks. You MUST execute each task independently and return ONLY a valid JSON array matching this schema: \
[{\"id\": \"<task_id>\", \"text\": \"<answer>\"}]. \
Never output markdown code blocks (no ```json or ```). Output pure JSON only."
}

/// Coalesces multiple batch items into a single multi-task payload.
pub fn build_coalesced_prompt(items: &[BatchItem]) -> String {
    let inputs: Vec<TaskInput> = items
        .iter()
        .map(|item| TaskInput {
            id: &item.id,
            task: &item.prompt,
        })
        .collect();
    serde_json::to_string(&inputs).unwrap_or_else(|_| "[]".to_string())
}

/// Unpacks the model's response into individual task answers.
pub fn parse_coalesced_response(raw_text: &str) -> Result<Vec<TaskOutput>, String> {
    let clean = raw_text.trim();
    let json_str = if clean.starts_with("```") {
        let stripped = clean.trim_start_matches("```");
        let stripped = stripped.strip_prefix("json").unwrap_or(stripped);
        stripped.trim().trim_end_matches("```").trim()
    } else {
        clean
    };

    let outputs: Vec<TaskOutput> = serde_json::from_str(json_str)
        .map_err(|e| format!("failed to parse batch response array: {e} | text: {json_str}"))?;
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_and_parse_coalesced_prompts() {
        let items = vec![
            BatchItem {
                id: "task-101".into(),
                prompt: "Translate 'Hello' to Spanish".into(),
                system: None,
            },
            BatchItem {
                id: "task-102".into(),
                prompt: "Calculate 15 * 8".into(),
                system: None,
            },
        ];

        let prompt = build_coalesced_prompt(&items);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&prompt).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["id"], "task-101");
        assert_eq!(parsed[0]["task"], "Translate 'Hello' to Spanish");
        assert_eq!(parsed[1]["id"], "task-102");
        assert_eq!(parsed[1]["task"], "Calculate 15 * 8");

        // Simulate pure JSON response
        let raw_json_resp = r#"[
            {"id": "task-101", "text": "Hola"},
            {"id": "task-102", "text": "120"}
        ]"#;
        let outputs = parse_coalesced_response(raw_json_resp).unwrap();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].id, "task-101");
        assert_eq!(outputs[0].text, "Hola");
        assert_eq!(outputs[1].id, "task-102");
        assert_eq!(outputs[1].text, "120");

        // Simulate fenced JSON response
        let fenced_resp = format!("```json\n{}\n```", raw_json_resp);
        let fenced_outputs = parse_coalesced_response(&fenced_resp).unwrap();
        assert_eq!(fenced_outputs, outputs);
    }
}
