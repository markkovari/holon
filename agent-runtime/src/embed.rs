//! Text embeddings from a local service (`embed/server.py`, EmbeddingGemma 2). Used to
//! recall memories by meaning rather than by shared words. Optional: with no service
//! configured, or one that is down, callers fall back to what they did before.

use std::time::Duration;

pub struct Embedder {
    url: String,
    dim: u32,
    http: reqwest::blocking::Client,
}

/// Two shortened vectors compare fine for ranking; the full ones are not needed.
const DEFAULT_DIM: u32 = 256;

impl Embedder {
    pub fn new(url: &str) -> Option<Self> {
        let url = url.trim().trim_end_matches('/');
        if url.is_empty() {
            return None;
        }
        let http =
            reqwest::blocking::Client::builder().timeout(Duration::from_secs(20)).build().ok()?;
        Some(Self { url: url.to_string(), dim: DEFAULT_DIM, http })
    }

    /// `kind`: "query" for what someone asked, "document" for what is searched.
    pub fn embed(&self, texts: &[String], kind: &str) -> Option<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Some(vec![]);
        }
        let r = self
            .http
            .post(format!("{}/embed", self.url))
            .json(&serde_json::json!({"texts": texts, "kind": kind, "dim": self.dim}))
            .send()
            .ok()?;
        if !r.status().is_success() {
            return None;
        }
        let v: serde_json::Value = r.json().ok()?;
        let out: Vec<Vec<f32>> = v["vectors"]
            .as_array()?
            .iter()
            .map(|row| {
                row.as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect())
            })
            .collect::<Option<_>>()?;
        (out.len() == texts.len()).then_some(out)
    }
}

/// Cosine similarity; the service normalises, but a stored vector may not be.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let (na, nb) =
        (a.iter().map(|x| x * x).sum::<f32>().sqrt(), b.iter().map(|x| x * x).sum::<f32>().sqrt());
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_is_one_for_equal_zero_for_orthogonal_and_safe_on_mismatch() {
        assert!((cosine(&[1.0, 2.0], &[2.0, 4.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
    }

    #[test]
    fn no_url_means_no_embedder() {
        assert!(Embedder::new("  ").is_none());
        assert!(Embedder::new("http://127.0.0.1:1").is_some());
    }
}
