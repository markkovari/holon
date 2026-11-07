//! Jev (TypeSafe's System One) routes a message to agents; its key is read per call, never stored or logged.

use agent_wire::jev::{self as codec, AnswerSpec, QuestionItem, QuestionSpec};

const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const MODEL: &str = "jev-latest";
/// In milli-probability.
const THRESHOLD: u32 = 500;

pub fn affected(
    http: &dyn crate::http::Http,
    key: &str,
    message: &str,
    candidates: &[(String, String)],
) -> Option<Vec<String>> {
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
    let auth = crate::http::bearer(key.trim());
    let headers = [("authorization", auth.as_str()), ("content-type", "application/json")];
    let resp = http.send("POST", ENDPOINT, &headers, Some(body.as_bytes())).ok()?;
    if !resp.ok() {
        return None;
    }
    let answers = codec::parse_evaluate(&resp.body, &items).ok()?;
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
#[path = "../tests/support/reqwest_http.rs"]
mod reqwest_http;

#[cfg(test)]
mod tests {
    use super::*;

    /// Live: `HOLON_JEV_KEY_FILE=<file> cargo test jev_live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn jev_live_picks_the_member_a_message_is_about() {
        let key_file = std::env::var("HOLON_JEV_KEY_FILE").expect("HOLON_JEV_KEY_FILE");
        let key = std::fs::read_to_string(key_file).expect("the key file");
        let http = super::reqwest_http::Reqwest::new();
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
            let got = affected(&http, &key, msg, &cands).expect("Jev answered");
            println!("{msg:45} -> {got:?}");
            assert!(got.contains(&want.to_string()), "{msg}: {got:?}");
        }
    }
}
