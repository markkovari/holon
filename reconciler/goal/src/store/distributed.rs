//! Distributed goal storage over SurrealDB and NATS JetStream (ADR-0082, ADR-0091).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::{now_secs, GoalRecord, GoalStatus, GoalStore};

/// Distributed GoalStore implementation over SurrealDB cluster and optional NATS event stream.
pub struct SurrealNatsGoalStore {
    url: String,
    namespace: String,
    database: String,
    user: String,
    password: Option<String>,
    nats_url: Option<String>,
    defined: AtomicBool,
}

impl SurrealNatsGoalStore {
    pub fn new(
        url: impl Into<String>,
        namespace: impl Into<String>,
        database: impl Into<String>,
        user: impl Into<String>,
        password: Option<String>,
        nats_url: Option<String>,
    ) -> Self {
        Self {
            url: url.into().trim_end_matches('/').to_string(),
            namespace: namespace.into(),
            database: database.into(),
            user: user.into(),
            password,
            nats_url,
            defined: AtomicBool::new(false),
        }
    }

    pub fn default_endpoint() -> &'static str {
        "http://127.0.0.1:8000"
    }

    fn ensure_defined(&self) -> Result<()> {
        if self.defined.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        let surql = format!(
            "DEFINE NAMESPACE IF NOT EXISTS {ns}; USE NS {ns};
             DEFINE DATABASE IF NOT EXISTS {db}; USE DB {db};
             DEFINE TABLE IF NOT EXISTS goal SCHEMAFULL;
             DEFINE FIELD IF NOT EXISTS project ON TABLE goal TYPE string;
             DEFINE FIELD IF NOT EXISTS title ON TABLE goal TYPE string;
             DEFINE FIELD IF NOT EXISTS text ON TABLE goal TYPE string;
             DEFINE FIELD IF NOT EXISTS status ON TABLE goal TYPE string;
             DEFINE FIELD IF NOT EXISTS priority ON TABLE goal TYPE int DEFAULT 100;
             DEFINE FIELD IF NOT EXISTS parent ON TABLE goal TYPE option<string>;
             DEFINE FIELD IF NOT EXISTS parts ON TABLE goal TYPE array DEFAULT [];
             DEFINE FIELD IF NOT EXISTS composition_checks ON TABLE goal TYPE array DEFAULT [];
             DEFINE FIELD IF NOT EXISTS created_at ON TABLE goal TYPE int;
             DEFINE FIELD IF NOT EXISTS updated_at ON TABLE goal TYPE int;
             DEFINE INDEX IF NOT EXISTS idx_goal_dispatch ON TABLE goal COLUMNS project, status, priority, created_at;",
            ns = self.namespace,
            db = self.database,
        );
        let _ = self.query_raw(&surql);
        Ok(())
    }

    fn query_raw(&self, surql: &str) -> Result<Vec<Value>> {
        let client =
            reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build()?;
        let mut req = client.post(format!("{}/sql", self.url));
        if let Some(ref p) = self.password {
            req = req.basic_auth(&self.user, Some(p));
        }
        let res = req
            .header("Accept", "application/json")
            .header("surreal-ns", &self.namespace)
            .header("surreal-db", &self.database)
            .body(surql.to_string())
            .send()
            .with_context(|| format!("calling SurrealDB at {}", self.url))?;

        if !res.status().is_success() {
            let text = res.text().unwrap_or_default();
            bail!("SurrealDB error: {}", text.trim());
        }

        let answers: Vec<Value> = res.json()?;
        Ok(answers)
    }

    fn dispatch_nats_event(&self, project: &str, goal_id: &str) {
        if let Some(ref nats_url) = self.nats_url {
            let nats_url = nats_url.clone();
            let subject = format!("comp.goals.dispatch.{project}");
            let payload = json!({
                "id": goal_id,
                "project": project,
                "at": now_secs(),
            })
            .to_string();

            // Fire and forget over async-nats in a background task
            tokio::spawn(async move {
                if let Ok(client) = async_nats::connect(&nats_url).await {
                    let _ = client.publish(subject, payload.into()).await;
                }
            });
        }
    }
}

impl GoalStore for SurrealNatsGoalStore {
    fn is_shared(&self) -> bool {
        true
    }

    fn create_goal(&self, goal: &GoalRecord) -> Result<String> {
        self.ensure_defined()?;
        let json_data = serde_json::to_string(goal)?;
        let surql =
            format!("CREATE type::thing('goal', '{}') CONTENT {};", clean_id(&goal.id), json_data);
        let _ = self.query_raw(&surql)?;

        if goal.status == GoalStatus::Queued {
            self.dispatch_nats_event(&goal.project, &goal.id);
        }

        Ok(goal.id.clone())
    }

    fn get_goal(&self, id: &str) -> Result<Option<GoalRecord>> {
        self.ensure_defined()?;
        let surql = format!("SELECT * FROM type::thing('goal', '{}');", clean_id(id));
        let answers = self.query_raw(&surql)?;

        let Some(first) = answers.into_iter().next() else {
            return Ok(None);
        };
        let Some(rows) = first.get("result").and_then(|v| v.as_array()) else {
            return Ok(None);
        };
        if let Some(record_val) = rows.first() {
            let record: GoalRecord = serde_json::from_value(record_val.clone())?;
            Ok(Some(record))
        } else {
            Ok(None)
        }
    }

    fn list_goals(&self, project: &str, status: Option<GoalStatus>) -> Result<Vec<GoalRecord>> {
        self.ensure_defined()?;
        let surql = if let Some(s) = status {
            format!(
                "SELECT * FROM goal WHERE project = '{}' AND status = '{}' ORDER BY priority DESC, created_at ASC;",
                clean_id(project),
                s.as_str()
            )
        } else {
            format!(
                "SELECT * FROM goal WHERE project = '{}' ORDER BY priority DESC, created_at ASC;",
                clean_id(project)
            )
        };

        let answers = self.query_raw(&surql)?;
        let mut out = Vec::new();
        if let Some(first) = answers.into_iter().next() {
            if let Some(rows) = first.get("result").and_then(|v| v.as_array()) {
                for r in rows {
                    if let Ok(record) = serde_json::from_value::<GoalRecord>(r.clone()) {
                        out.push(record);
                    }
                }
            }
        }
        Ok(out)
    }

    fn transition_status(&self, id: &str, from: GoalStatus, to: GoalStatus) -> Result<bool> {
        if !from.can_transition_to(to) {
            bail!("illegal goal transition from `{from}` to `{to}`");
        }
        self.ensure_defined()?;
        let now = now_secs();
        let surql = format!(
            "UPDATE type::thing('goal', '{id}') SET status = '{to}', updated_at = {now} WHERE status = '{from}' RETURN BEFORE;",
            id = clean_id(id),
            to = to.as_str(),
            from = from.as_str(),
        );
        let answers = self.query_raw(&surql)?;
        let changed = answers
            .first()
            .and_then(|v| v.get("result"))
            .and_then(|r| r.as_array())
            .map(|arr| !arr.is_empty())
            .unwrap_or(false);

        if changed && to == GoalStatus::Running {
            if let Ok(Some(g)) = self.get_goal(id) {
                self.dispatch_nats_event(&g.project, id);
            }
        }

        Ok(changed)
    }

    fn claim_next_queued(&self, project: &str) -> Result<Option<GoalRecord>> {
        self.ensure_defined()?;
        let now = now_secs();
        let surql = format!(
            "LET $next = (SELECT id FROM goal WHERE project = '{p}' AND status = 'queued' ORDER BY priority DESC, created_at ASC LIMIT 1);\
             UPDATE $next[0].id SET status = 'running', updated_at = {now} RETURN AFTER;",
            p = clean_id(project),
        );
        let answers = self.query_raw(&surql)?;
        // Return the updated record
        for ans in answers {
            if let Some(rows) = ans.get("result").and_then(|v| v.as_array()) {
                if let Some(first) = rows.first() {
                    if let Ok(record) = serde_json::from_value::<GoalRecord>(first.clone()) {
                        self.dispatch_nats_event(project, &record.id);
                        return Ok(Some(record));
                    }
                }
            }
        }
        Ok(None)
    }

    fn delete_goal(&self, id: &str) -> Result<bool> {
        self.ensure_defined()?;
        let surql = format!("DELETE type::thing('goal', '{}') RETURN BEFORE;", clean_id(id));
        let answers = self.query_raw(&surql)?;
        let changed = answers
            .first()
            .and_then(|v| v.get("result"))
            .and_then(|r| r.as_array())
            .map(|arr| !arr.is_empty())
            .unwrap_or(false);
        Ok(changed)
    }
}

fn clean_id(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}
