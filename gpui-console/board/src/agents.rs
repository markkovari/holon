//! The conversational agents: who each one is, what was said to it and what it answered, and where it
//! is in Element.
//!
//! `agent-matrix` makes every agent a Matrix user and gives it a room: its own in the Agents Space when it
//! is in no project, its project's `general` when it is in one (agent-matrix/README.md). What the bridge
//! knows is in its own files (`~/.holon-matrix/config.json`, `state.json`), and a room in them is a
//! `matrix.to` link that Element, Element X and any other client open; without them an agent is still
//! its user, `@agent-<name>:<server>`, when the server is known. Pure but for reading those two files.

use std::collections::BTreeMap;

use super::tree::{AgentTree, Kind, Status};

/// An agent, as the runtime's spec says it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentInfo {
    pub name: String,
    pub description: String,
    /// `provider/model`.
    pub model: String,
    pub paused: bool,
    /// `schedule: @daily`, `event: rowing.*`, ...
    pub triggers: Vec<String>,
    /// The tools it may use.
    pub capabilities: Vec<String>,
    pub projects: Vec<String>,
}

/// One thing said to an agent, and what it answered.
#[derive(Clone, Debug, PartialEq)]
pub struct Exchange {
    /// The run, which is a session when nothing called it.
    pub run: String,
    pub asked: String,
    /// `None` while it is still going.
    pub answer: Option<String>,
    pub status: Status,
    pub tools: Vec<String>,
    pub started_ms: u64,
}

/// What was said to `agent` lately, oldest first, as the board has seen it (its runs since launch, and
/// what the runtime kept from before).
pub fn conversation(tree: &AgentTree, agent: &str, limit: usize) -> Vec<Exchange> {
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
        })
        .collect();
    runs.sort_by_key(|e| e.started_ms);
    let skip = runs.len().saturating_sub(limit);
    runs.split_off(skip)
}

/// Where the agents are in Matrix, as the bridge recorded it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Element {
    pub server: String,
    /// The `holon` control room.
    pub control: Option<String>,
    /// agent -> its own room.
    pub dms: BTreeMap<String, String>,
    /// project -> its `general` room.
    pub projects: BTreeMap<String, String>,
}

/// A Matrix id (a room's `!…` or a user's `@…`) as a link any client opens.
pub fn link(id: &str, server: &str) -> String {
    if id.starts_with('!') {
        format!("https://matrix.to/#/{id}?via={server}")
    } else {
        format!("https://matrix.to/#/{id}")
    }
}

impl Element {
    /// Just a server: what a page is told, or what is known before the bridge has made rooms.
    pub fn on(server: &str) -> Self {
        Element { server: server.to_string(), ..Default::default() }
    }

    /// The bridge's own record, from its config and state (agent-matrix/src/config.rs, state.rs).
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

    /// The bridge's files where it keeps them: `HOLON_MATRIX_DIR`, or `~/.holon-matrix`. Natively only;
    /// a page is told its server instead (`?matrix=`).
    #[cfg(not(target_family = "wasm"))]
    pub fn load() -> Option<Self> {
        let dir = std::env::var("HOLON_MATRIX_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|_| {
                std::env::var("HOME").map(|h| std::path::Path::new(&h).join(".holon-matrix"))
            })
            .ok()?;
        let read = |f: &str| -> Option<serde_json::Value> {
            serde_json::from_str(&std::fs::read_to_string(dir.join(f)).ok()?).ok()
        };
        Element::from_files(&read("config.json")?, &read("state.json").unwrap_or_default())
    }

    /// Where to talk to `agent`: its own room, else the `general` of the first project it is in that has
    /// one, else its user.
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
        // a bridge that has not been set up is no Element at all
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
