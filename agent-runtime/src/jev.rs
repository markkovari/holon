//! Jev (TypeSafe's System One) over HTTPS, for questions about a picture.
//!
//! Jev does not write text: it answers typed questions (a choice, a score, a yes/no) with
//! probabilities. Images are sent in a top-level `assets` map and named where they are used, and
//! a request with one needs the `TypeSafe-Danger-Zone-Images` header (the feature is a preview;
//! docs.typesafe.ai/images). The key is read from the file named by `HOLON_JEV_KEY_FILE` at each
//! call; it is never kept, logged, or put in a spec.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

pub const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
pub const MODEL: &str = "jev-latest";

/// What the food tools need from Jev: one request in, the `answers` map out.
pub trait Jev: Sync {
    fn ask(&self, request: &Value) -> Result<Value, String>;
}

pub struct JevClient {
    key_file: PathBuf,
    endpoint: String,
}

impl JevClient {
    pub fn new(key_file: impl Into<PathBuf>) -> Self {
        Self { key_file: key_file.into(), endpoint: ENDPOINT.into() }
    }

    /// From `HOLON_JEV_KEY_FILE`, when it is set and names a file.
    pub fn from_env() -> Option<Self> {
        let f = std::env::var("HOLON_JEV_KEY_FILE").ok().filter(|f| !f.is_empty())?;
        let f = PathBuf::from(f);
        f.is_file().then(|| Self::new(f))
    }

    pub fn with_endpoint(mut self, endpoint: &str) -> Self {
        self.endpoint = endpoint.into();
        self
    }
}

impl Jev for JevClient {
    fn ask(&self, request: &Value) -> Result<Value, String> {
        let key = std::fs::read_to_string(&self.key_file)
            .map_err(|_| "the Jev key file cannot be read".to_string())?;
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .post(&self.endpoint)
            .bearer_auth(key.trim())
            .header("TypeSafe-Danger-Zone-Images", "true")
            .json(request)
            .send()
            .map_err(|e| format!("Jev is unreachable: {}", e.without_url()))?;
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        if !status.is_success() {
            let snippet: String = body.chars().take(300).collect();
            return Err(format!("Jev answered {status}: {snippet}"));
        }
        let v: Value = serde_json::from_str(&body).map_err(|e| format!("Jev reply: {e}"))?;
        v.get("answers").cloned().ok_or_else(|| "Jev reply has no answers".into())
    }
}

/// An `assets` entry for a JPEG or PNG.
pub fn asset(media_type: &str, bytes: &[u8]) -> Value {
    use base64::Engine;
    json!({
        "type": "image",
        "format": if media_type == "image/png" { "png" } else { "jpg" },
        "data_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

/// A yes/no answer as a probability in 0..=1.
pub fn noul(answers: &Value, id: &str) -> Option<f64> {
    answers.get(id)?.get("noul")?.as_f64()
}

/// A choice answer: the option picked and its probability.
pub fn choice(answers: &Value, id: &str) -> Option<(String, f64)> {
    let a = answers.get(id)?;
    let picked = a.get("choice")?.as_str()?.to_string();
    let p = a
        .get("probabilities")
        .and_then(|p| p.get(&picked))
        .and_then(Value::as_f64)
        .or_else(|| a.get("confidence").and_then(Value::as_f64))
        .unwrap_or(0.0);
    Some((picked, p))
}

/// A score answer: the probability-weighted level number (fractional), 0 for the lowest level.
pub fn score(answers: &Value, id: &str) -> Option<f64> {
    answers.get(id)?.get("score")?.as_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_read_by_their_kind() {
        let a = json!({
            "yes": {"type": "noul", "noul": 0.93},
            "pick": {"type": "choice", "choice": "pasta", "probabilities": {"pasta": 0.7, "soup": 0.3}, "confidence": 0.7},
            "how": {"type": "score", "score": 3.4}
        });
        assert_eq!(noul(&a, "yes"), Some(0.93));
        assert_eq!(choice(&a, "pick"), Some(("pasta".into(), 0.7)));
        assert_eq!(score(&a, "how"), Some(3.4));
        assert_eq!(noul(&a, "missing"), None);
    }

    #[test]
    fn a_photo_is_an_asset_with_its_format() {
        let a = asset("image/png", &[1, 2, 3]);
        assert_eq!((a["format"].as_str(), a["data_base64"].as_str()), (Some("png"), Some("AQID")));
        assert_eq!(asset("image/jpeg", &[1])["format"], "jpg");
    }
}
