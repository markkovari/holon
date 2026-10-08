//! Ask Jev (TypeSafe's System One) which agents a message concerns, directly over HTTPS.
//!
//! Jev answers typed questions with probabilities; it does not write text, so it is the router
//! here, not a chat model. The wire format is the `typesafe-provider` component's own codec,
//! included by path so there is one definition of it. The API key is read from the file named
//! by `HOLON_JEV_KEY_FILE` at each call; it is never stored, logged or put in the bridge config.

#[allow(dead_code)]
#[path = "../../components/typesafe-provider/src/codec.rs"]
mod codec;

use std::time::Duration;

use codec::{AnswerSpec, QuestionItem, QuestionSpec};

const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const MODEL: &str = "jev-latest";
/// Milli-probability at or above which a member counts as affected.
const THRESHOLD: u32 = 500;

/// The members (of `candidates`: name, description) the message concerns, or `None` when Jev
/// cannot be reached or answers unusably: the caller then falls back to its next rule.
pub fn affected(
    key_file: &str,
    message: &str,
    candidates: &[(String, String)],
) -> Option<Vec<String>> {
    let key = std::fs::read_to_string(key_file).ok()?;
    let items: Vec<QuestionItem> = candidates
        .iter()
        .map(|(name, about)| QuestionItem {
            id: name.clone(),
            instructions: format!(
                "Is the agent '{name}' ({about}) one that should act on this message?"
            ),
            kind: QuestionSpec::Gate {
                true_hint: "The message is about what this agent is for.".into(),
                false_hint: "The message is unrelated to this agent's purpose.".into(),
            },
        })
        .collect();
    let body = codec::evaluate_body(MODEL, message, &items);
    let client =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(8)).build().ok()?;
    let resp = client
        .post(ENDPOINT)
        .bearer_auth(key.trim())
        .header("content-type", "application/json")
        .body(body)
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let answers = codec::parse_evaluate(&resp.bytes().ok()?, &items).ok()?;
    Some(
        answers
            .into_iter()
            .filter_map(|(id, a)| match a {
                Ok(AnswerSpec::Gate(p)) if p >= THRESHOLD => Some(id),
                _ => None,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live: `HOLON_JEV_KEY_FILE=<file> cargo test jev_live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn jev_live_picks_the_member_a_message_is_about() {
        let key_file = std::env::var("HOLON_JEV_KEY_FILE").expect("HOLON_JEV_KEY_FILE");
        let cands = vec![
            (
                "rower".to_string(),
                "Answers questions about Mark's rowing training log.".to_string(),
            ),
            (
                "chef".to_string(),
                "Plans meals and nutrition: recipes, calories, groceries.".to_string(),
            ),
            ("sysadmin".to_string(), "Inspects servers, processes and logs.".to_string()),
        ];
        for (msg, want) in [
            ("how was my last 2k row?", "rower"),
            ("what should I cook after training?", "chef"),
            ("why is nginx down?", "sysadmin"),
        ] {
            let got = affected(&key_file, msg, &cands).expect("Jev answered");
            println!("{msg:45} -> {got:?}");
            assert!(got.contains(&want.to_string()), "{msg}: {got:?}");
        }
    }
}
