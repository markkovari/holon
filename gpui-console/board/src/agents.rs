use std::collections::BTreeMap;

use super::tree::{AgentTree, Kind, Status};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentInfo {
    pub name: String,
    pub description: String,
    pub model: String,
    pub paused: bool,
    pub triggers: Vec<String>,
    pub capabilities: Vec<String>,
    pub projects: Vec<String>,
    pub api_keys: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemoryItem {
    pub at: u64,
    pub text: String,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BatchQueueItem {
    pub id: String,
    pub prompt: String,
    pub status: String,
    #[serde(default)]
    pub response: Option<String>,
    pub timestamp_ms: u64,
    #[serde(default)]
    pub model: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Exchange {
    pub run: String,
    pub asked: String,
    pub answer: Option<String>,
    pub status: Status,
    pub tools: Vec<String>,
    pub started_ms: u64,
    pub elapsed_ms: u64,
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub notes: Vec<String>,
}

pub fn conversation(tree: &AgentTree, agent: &str, limit: usize) -> Vec<Exchange> {
    let now = super::view::now_ms();
    let mut runs: Vec<Exchange> = tree
        .all()
        .filter(|n| n.kind == Kind::Run && n.name == agent)
        .map(|n| Exchange {
            run: n.id.clone(),
            asked: n.task.clone(),
            answer: n.result.clone().filter(|_| n.status.is_over()),
            status: n.status,
            tools: n.tools.clone(),
            started_ms: n.started_ms,
            elapsed_ms: n.elapsed_ms(now),
            model: n.model.clone(),
            tokens_in: n.tokens_in,
            tokens_out: n.tokens_out,
            notes: n.notes.clone(),
        })
        .collect();
    runs.sort_by_key(|e| e.started_ms);
    let skip = runs.len().saturating_sub(limit);
    runs.split_off(skip)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Element {
    pub server: String,
    pub control: Option<String>,
    pub dms: BTreeMap<String, String>,
    pub projects: BTreeMap<String, String>,
}

pub fn link(id: &str, server: &str) -> String {
    if id.starts_with('!') {
        format!("https://matrix.to/#/{id}?via={server}")
    } else {
        format!("https://matrix.to/#/{id}")
    }
}

impl Element {
    pub fn on(server: &str) -> Self {
        Element { server: server.to_string(), ..Default::default() }
    }

    pub fn from_files(config: &serde_json::Value, state: &serde_json::Value) -> Option<Self> {
        let server = config["server_name"].as_str()?.to_string();
        let map = |v: &serde_json::Value| -> BTreeMap<String, String> {
            v.as_object()
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default()
        };
        let projects = state["projects"]
            .as_object()
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v["general"].as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        Some(Element {
            server,
            control: state["control_room"].as_str().map(String::from),
            dms: map(&state["dms"]),
            projects,
        })
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn load() -> Option<Self> {
        let dir = std::env::var("HOLON_MATRIX_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|_| {
                std::env::var("HOME").map(|h| std::path::Path::new(&h).join(".holon-matrix"))
            })
            .ok()?;
        // `holon matrix init` writes bridge.conf; the bridge component keeps its rooms and
        // answers them, with the hs_token, at /internal/rooms.
        let conf = std::fs::read_to_string(dir.join("bridge.conf")).ok()?;
        let get = |k: &str| {
            conf.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix(" = ").map(String::from))
        };
        let url = std::env::var("HOLON_MATRIX_URL").unwrap_or("http://127.0.0.1:9009".into());
        let rooms = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .ok()?
            .get(format!("{url}/internal/rooms"))
            .bearer_auth(get("matrix-hs-token")?)
            .send()
            .ok()?
            .json::<serde_json::Value>()
            .ok()
            .unwrap_or_default();
        Element::from_files(&serde_json::json!({"server_name": get("matrix-server-name")?}), &rooms)
    }

    pub fn agent(&self, agent: &str, projects: &[String]) -> String {
        let room =
            self.dms.get(agent).or_else(|| projects.iter().find_map(|p| self.projects.get(p)));
        match room {
            Some(r) => link(r, &self.server),
            None => link(&format!("@agent-{agent}:{}", self.server), &self.server),
        }
    }

    pub fn control_room(&self) -> Option<String> {
        self.control.as_deref().map(|r| link(r, &self.server))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_agent_is_reached_in_its_own_room_its_projects_or_as_its_user() {
        let e = Element::from_files(
            &json!({"server_name": "malna.ts.net", "homeserver": "http://x"}),
            &json!({
                "control_room": "!ctl:malna.ts.net",
                "dms": {"rower": "!dm:malna.ts.net"},
                "projects": {"rowing": {"space": "!sp:m", "general": "!gen:malna.ts.net", "feed": "!f:m"}},
            }),
        )
        .unwrap();
        assert_eq!(e.agent("rower", &[]), "https://matrix.to/#/!dm:malna.ts.net?via=malna.ts.net");
        assert_eq!(
            e.agent("cox", &["rowing".into()]),
            "https://matrix.to/#/!gen:malna.ts.net?via=malna.ts.net"
        );
        assert_eq!(
            e.agent("new", &["nowhere".into()]),
            "https://matrix.to/#/@agent-new:malna.ts.net"
        );
        assert_eq!(
            e.control_room().unwrap(),
            "https://matrix.to/#/!ctl:malna.ts.net?via=malna.ts.net"
        );
        assert!(Element::from_files(&json!({}), &json!({})).is_none());
        assert_eq!(Element::on("s.net").agent("a", &[]), "https://matrix.to/#/@agent-a:s.net");
    }

    #[test]
    fn a_conversation_is_an_agents_runs_oldest_first_and_only_the_last_few() {
        use agent_wire::Event;
        let mut t = AgentTree::default();
        let start = |run: &str, agent: &str, input: &str| Event::RunStarted {
            agent: agent.into(),
            run_id: run.into(),
            trigger: "ui".into(),
            input: input.into(),
            trace_id: String::new(),
            span_id: String::new(),
            parent_span_id: None,
            model: String::new(),
            chain: vec![],
        };
        t.apply(&start("r1", "lead", "first"), 1_000);
        t.apply(&start("r2", "other", "not lead's"), 2_000);
        t.apply(&start("r3", "lead", "second"), 3_000);
        t.apply(&start("r4", "lead", "third"), 4_000);
        let c = conversation(&t, "lead", 2);
        assert_eq!(c.iter().map(|e| e.asked.as_str()).collect::<Vec<_>>(), ["second", "third"]);
        assert_eq!(
            (c[1].answer.clone(), c[1].status),
            (None, Status::Running),
            "no answer while it runs"
        );
    }
}
