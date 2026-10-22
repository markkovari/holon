use serde_json::Value;

use super::map::{AttemptRow, CatalogRows, KnowledgeRows, RunRow};
#[cfg(not(target_family = "wasm"))]
use super::map::{Kind, Map, Proof};
use std::path::Path;

pub const CATALOG_SQL: &str = "SELECT VALUE name FROM artifact;
SELECT in.name AS a, out.name AS b FROM imports;
SELECT in.name AS a, out.name AS b FROM exports;
SELECT name AS a, root AS b FROM app;
SELECT in.name AS a, out.name AS b FROM carries;";

pub const KNOWLEDGE_SQL: &str = "SELECT id_text, goal, outcome, winner, url, branches, started_at FROM run ORDER BY started_at DESC LIMIT 60;
SELECT attempt, data.verdict AS verdict FROM event WHERE kind = 'gate-verdict';
SELECT id_text, run, branch, round, outcome, score, tokens, paths FROM attempt;
SELECT name, added_by FROM capability;";

pub struct Store {
    pub url: String,
    ns: String,
    db: String,
    user: Option<(String, String)>,
}

impl Store {
    pub fn from_env() -> Self {
        let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
        let user = var("HOLON_SURREAL_USER", "root");
        Self {
            url: var("HOLON_SURREAL_URL", "http://127.0.0.1:8000"),
            ns: "comp".into(),
            db: "goalmemory".into(),
            user: (!user.is_empty()).then(|| (user, var("HOLON_SURREAL_PASS", "root"))),
        }
    }

    #[cfg(not(target_family = "wasm"))]
    fn post(&self, q: &str) -> Result<Value, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        let mut req = client
            .post(format!("{}/sql", self.url))
            .header("surreal-ns", &self.ns)
            .header("surreal-db", &self.db)
            .header("Accept", "application/json")
            .body(q.to_string());
        if let Some((u, p)) = &self.user {
            req = req.basic_auth(u, Some(p));
        }
        let res =
            req.send().map_err(|e| format!("the store at {} did not answer: {e}", self.url))?;
        let status = res.status();
        res.json().map_err(|e| format!("the store answered {status}: {e}"))
    }

    #[cfg(target_family = "wasm")]
    pub async fn post(&self, q: &str) -> Result<Value, String> {
        use wasm_bindgen::JsCast;
        let js = |e: wasm_bindgen::JsValue| format!("{e:?}");
        let window = web_sys::window().ok_or("no window")?;
        let init = web_sys::RequestInit::new();
        init.set_method("POST");
        init.set_body(&wasm_bindgen::JsValue::from_str(q));
        let req = web_sys::Request::new_with_str_and_init(&format!("{}/sql", self.url), &init)
            .map_err(js)?;
        let h = req.headers();
        for (k, v) in [
            ("surreal-ns", self.ns.as_str()),
            ("surreal-db", self.db.as_str()),
            ("Accept", "application/json"),
        ] {
            h.set(k, v).map_err(js)?;
        }
        if let Some((u, p)) = &self.user {
            let token = window.btoa(&format!("{u}:{p}")).map_err(js)?;
            h.set("Authorization", &format!("Basic {token}")).map_err(js)?;
        }
        let res = wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&req))
            .await
            .map_err(|e| format!("the store at {} did not answer: {e:?}", self.url))?;
        let res: web_sys::Response = res.dyn_into().map_err(js)?;
        let text =
            wasm_bindgen_futures::JsFuture::from(res.text().map_err(js)?).await.map_err(js)?;
        serde_json::from_str(&text.as_string().unwrap_or_default())
            .map_err(|e| format!("the store answered {}: {e}", res.status()))
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn catalog(&self) -> Result<CatalogRows, String> {
        catalog_of(&self.post(CATALOG_SQL)?)
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn knowledge(&self) -> Result<KnowledgeRows, String> {
        knowledge_of(&self.post(KNOWLEDGE_SQL)?)
    }
}

fn each(body: &Value) -> Result<Vec<Result<Vec<Value>, String>>, String> {
    let list = body.as_array().ok_or_else(|| format!("the store said {body}"))?;
    Ok(list
        .iter()
        .map(|s| match s["status"].as_str() {
            Some("OK") => Ok(s["result"].as_array().cloned().unwrap_or_default()),
            _ => Err(s["result"].as_str().unwrap_or("a statement failed").to_string()),
        })
        .collect())
}

// a missing namespace or table is an empty store, not a failure
fn rows(r: Option<&Result<Vec<Value>, String>>) -> Result<Vec<Value>, String> {
    match r {
        Some(Ok(v)) => Ok(v.clone()),
        Some(Err(e)) if e.contains("does not exist") => Ok(Vec::new()),
        Some(Err(e)) => Err(e.clone()),
        None => Ok(Vec::new()),
    }
}

fn text(v: &Value, k: &str) -> String {
    match &v[k] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub fn catalog_of(body: &Value) -> Result<CatalogRows, String> {
    let r = each(body)?;
    let pairs = |i: usize| -> Result<Vec<(String, String)>, String> {
        Ok(rows(r.get(i))?
            .iter()
            .filter_map(|v| Some((v["a"].as_str()?.to_string(), v["b"].as_str()?.to_string())))
            .collect())
    };
    Ok(CatalogRows {
        artifacts: rows(r.first())?.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        imports: pairs(1)?,
        exports: pairs(2)?,
        apps: pairs(3)?,
        carries: pairs(4)?,
    })
}

pub fn knowledge_of(body: &Value) -> Result<KnowledgeRows, String> {
    let r = each(body)?;
    let num = |v: &Value, k: &str| v[k].as_u64().unwrap_or(0);
    let runs = rows(r.first())?
        .iter()
        .map(|v| RunRow {
            id: text(v, "id_text"),
            goal: text(v, "goal"),
            outcome: text(v, "outcome"),
            winner: text(v, "winner"),
            url: text(v, "url"),
            branches: num(v, "branches") as u32,
            started: text(v, "started_at"),
        })
        .collect();
    let verdicts = rows(r.get(1))?;
    let failing_of = |attempt: &str| -> Vec<(String, String)> {
        let Some(v) = verdicts.iter().find(|v| v["attempt"].as_str() == Some(attempt)) else {
            return Vec::new();
        };
        match &v["verdict"] {
            Value::Array(a) => a.iter().map(|c| (text(c, "id"), text(c, "detail"))).collect(),
            o => o["failing"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| Some((x.as_str()?.to_string(), String::new())))
                        .collect()
                })
                .unwrap_or_default(),
        }
    };
    let attempts = rows(r.get(2))?
        .iter()
        .map(|v| AttemptRow {
            id: text(v, "id_text"),
            run: text(v, "run"),
            branch: text(v, "branch"),
            round: num(v, "round") as u32,
            outcome: text(v, "outcome"),
            score: num(v, "score") as u32,
            tokens: num(v, "tokens"),
            paths: v["paths"]
                .as_array()
                .map(|a| a.iter().filter_map(|p| p.as_str().map(String::from)).collect())
                .unwrap_or_default(),
            failing: failing_of(&text(v, "id_text")),
        })
        .collect();
    let capabilities =
        rows(r.get(3))?.iter().map(|v| (text(v, "name"), text(v, "added_by"))).collect();
    Ok(KnowledgeRows { runs, attempts, capabilities })
}

#[cfg(not(target_family = "wasm"))]
pub fn evidence(root: &Path, map: &mut Map) {
    let is = |p: &str| root.join(p).is_file();
    for f in &mut map.frames {
        for item in &mut f.items {
            let Some(name) = item.key.strip_prefix("component:").map(str::to_string) else {
                continue;
            };
            let app = (item.kind == Kind::App).then(|| item.label.clone());
            let mut proof = Vec::new();
            let mut media =
                vec![format!("components/{name}/demo.gif"), format!("docs/gifs/{name}.gif")];
            if let (Some(a), Kind::App) = (&app, item.kind) {
                media.insert(0, format!("docs/media/{a}.gif"));
            }
            proof.extend(media.into_iter().filter(|p| is(p)).map(Proof::Media));
            let files: Vec<(String, &str)> = match (&app, item.kind) {
                (Some(a), Kind::App) => vec![
                    (format!("tools/screencast/{a}.mjs"), "how its film is made"),
                    (
                        format!("docs/apps/{}.md", a.to_uppercase()),
                        "what it is, and how it was checked",
                    ),
                ],
                _ => vec![],
            };
            let gate = [
                (
                    format!("components/{name}/gate.sh"),
                    "its gate: builds it, composes it, runs the scenario",
                ),
                (format!("components/{name}/scenario.hurl"), "the scenario its gate runs"),
            ];
            proof.extend(
                files
                    .into_iter()
                    .chain(gate)
                    .filter(|(p, _)| is(p))
                    .map(|(path, what)| Proof::File { path, what: what.to_string() }),
            );
            item.proof.splice(0..0, proof);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_reply_is_read_statement_by_statement_and_a_missing_table_is_no_rows() {
        let reply = json!([
            {"status": "OK", "result": ["shop", "store"]},
            {"status": "OK", "result": [{"a": "shop", "b": "records:store/store@0.2.0"}]},
            {"status": "ERR", "result": "The table 'exports' does not exist"},
            {"status": "OK", "result": []},
            {"status": "OK", "result": [{"a": "shop", "b": "store"}]},
        ]);
        let c = catalog_of(&reply).unwrap();
        assert_eq!(c.artifacts, ["shop", "store"]);
        assert_eq!(c.imports, [("shop".to_string(), "records:store/store@0.2.0".to_string())]);
        assert!(c.exports.is_empty());
        assert_eq!(c.carries.len(), 1);
        let broken =
            json!([{"status": "ERR", "result": "There was a problem with authentication"}]);
        assert!(catalog_of(&broken).is_err(), "anything else is the store's to say");
        assert!(catalog_of(&json!({"code": 401})).is_err());
        let fresh = json!([{"status": "ERR", "result": "The namespace 'comp' does not exist"}]);
        assert_eq!(catalog_of(&fresh).unwrap(), CatalogRows::default());
    }

    #[test]
    fn a_verdict_is_read_as_goalrun_writes_it_and_as_the_seeder_does() {
        let reply = json!([
            {"status": "OK", "result": [{"id_text": "7", "goal": "g", "outcome": "merged", "winner": "a", "url": "u", "branches": 2, "started_at": "t"}]},
            {"status": "OK", "result": [
                {"attempt": "7/g1/a", "verdict": [{"id": "gate", "detail": "exit 1"}]},
                {"attempt": "7/g1/b", "verdict": {"failing": ["test_pages"]}}]},
            {"status": "OK", "result": [
                {"id_text": "7/g1/a", "run": "7", "branch": "a", "round": 1, "outcome": "failed", "score": 500, "paths": ["x"]},
                {"id_text": "7/g1/b", "run": "7", "branch": "b", "round": 1, "outcome": "failed", "score": 400}]},
            {"status": "ERR", "result": "The table 'capability' does not exist"},
        ]);
        let k = knowledge_of(&reply).unwrap();
        assert_eq!(k.runs[0].branches, 2);
        assert_eq!(k.attempts[0].failing, [("gate".to_string(), "exit 1".to_string())]);
        assert_eq!(k.attempts[1].failing, [("test_pages".to_string(), String::new())]);
        assert!(k.capabilities.is_empty());
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod live {
    #[test]
    #[ignore]
    fn the_store_reads_back_what_capgraph_and_the_seeder_wrote() {
        let s = super::Store::from_env();
        let c = s.catalog().unwrap();
        let k = s.knowledge().unwrap();
        eprintln!(
            "artifacts {} imports {} apps {} runs {} attempts {}",
            c.artifacts.len(),
            c.imports.len(),
            c.apps.len(),
            k.runs.len(),
            k.attempts.len()
        );
        assert!(!c.artifacts.is_empty());
    }
}
