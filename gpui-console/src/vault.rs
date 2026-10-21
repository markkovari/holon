//! The console's side of agent API keys: its platform's vault, and the fetch token its agents read it with.

use std::time::Duration;

use agent_board::keys::KeyRow;
use serde_json::{json, Value};

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().unwrap()
}

fn answer(r: reqwest::Result<reqwest::blocking::Response>) -> Result<Value, String> {
    let r = r.map_err(|e| format!("The vault did not answer: {e}"))?;
    let ok = r.status().is_success();
    let v: Value = r.json().unwrap_or(Value::Null);
    if ok {
        Ok(v)
    } else {
        Err(v["error"].as_str().unwrap_or("The vault refused.").to_string())
    }
}

pub fn list(base: &str, session: &str) -> Result<Vec<KeyRow>, String> {
    let v = answer(client().get(format!("{base}/api/secrets")).bearer_auth(session).send())?;
    let mut rows: Vec<KeyRow> = v["secrets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| KeyRow {
            name: s["name"].as_str().unwrap_or_default().to_string(),
            reference: s["ref"].as_str().unwrap_or_default().to_string(),
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(rows)
}

pub fn put(base: &str, session: &str, name: &str, value: &str) -> Result<(), String> {
    answer(
        client()
            .post(format!("{base}/api/secrets"))
            .bearer_auth(session)
            .json(&json!({ "name": name, "value": value }))
            .send(),
    )
    .map(|_| ())
}

pub fn remove(base: &str, session: &str, name: &str) -> Result<(), String> {
    answer(client().delete(format!("{base}/api/secrets/{name}")).bearer_auth(session).send())
        .map(|_| ())
}

/// A fresh token for exactly the keys there are now, so a key added a moment ago is readable and a removed one is not.
pub fn connect(base: &str, session: &str, org: &str, applier: &str) -> Result<(), String> {
    let refs: Vec<String> = list(base, session)?.into_iter().map(|k| k.reference).collect();
    let v = answer(
        client()
            .post(format!("{base}/api/internal/fetch-token"))
            .header("x-platform-secret", applier)
            .json(&json!({ "instance": "console-agents", "refs": refs, "ttl": 30 * 86_400 }))
            .send(),
    )?;
    let token = v["token"].as_str().ok_or("The platform minted no token.")?.to_string();
    let vault = agent_runtime::keys::Vault { url: base.to_string(), org: org.to_string(), token };
    vault.save(&agent_runtime::keys::Vault::file())
}
