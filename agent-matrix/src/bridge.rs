//! The bridge: every agent is a Matrix user, every project a Space.
//!
//! * **Agent → user.** `@agent-<name>:<server>`, registered through the
//!   appservice, with a direct room with the owner that the owner is placed in.
//! * **Project → Space** with a `general` room (where you talk to its agents) and
//!   a `feed` room (where agents report; read-only by convention).
//! * **Membership is mutual.** Inviting an agent's user to a project room adds it
//!   to the project in the runtime (and so grants it the project's store and
//!   topics); removing it takes that away. The runtime's registry is the source
//!   of truth, and each reconcile pass makes Matrix match it.
//! * **Who answers** in a project room: the agents you mention; if none, the
//!   project's lead; if there is no lead, nobody (so five agents do not all
//!   reply to every message).
//! * **Approvals** appear as a poll in the agent's room; your vote answers them.
//!
//! Only the owner can talk to the agents. Everyone else's messages are ignored.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::config::Config;
use crate::matrix::{Matrix, R};
use crate::runtime::{AgentInfo, ProjectInfo, Runtime};
use crate::state::{ProjectRooms, RoomKind, State};

const MAX_REPLY: usize = 30_000;

pub struct Bridge {
    pub cfg: Config,
    pub mx: Matrix,
    pub rt: Runtime,
    pub state: State,
    seen_events: Mutex<(HashSet<String>, VecDeque<String>)>,
    /// Serialises reconcile passes: one is enough, and two would race on room creation.
    reconciling: Mutex<()>,
}

fn log(msg: &str) {
    eprintln!("agent-matrix: {msg}");
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// Agents mentioned in a message: structured `m.mentions` first (what Element
/// sends when you pick a user), then `@name` / `@agent-name` in the text.
pub fn mentioned_agents(cfg: &Config, content: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |a: String| {
        if !out.contains(&a) {
            out.push(a);
        }
    };
    if let Some(ids) = content["m.mentions"]["user_ids"].as_array() {
        for id in ids.iter().filter_map(Value::as_str) {
            if let Some(a) = cfg.agent_of(id) {
                push(a);
            }
        }
    }
    for word in content["body"].as_str().unwrap_or_default().split_whitespace() {
        let w = word.trim_matches(|c: char| {
            !(c.is_alphanumeric() || matches!(c, '@' | '-' | '_' | ':' | '.'))
        });
        if let Some(name) = w.strip_prefix('@') {
            let name = name.trim_end_matches(':');
            let name = name.strip_prefix("agent-").unwrap_or(name);
            // `@agent-x:server` written out in full
            let name = name.split(':').next().unwrap_or(name);
            if !name.is_empty() {
                push(name.to_string());
            }
        }
    }
    out
}

/// Who answers a message in a project room.
pub fn project_targets(
    members: &[String],
    lead: Option<&str>,
    mentioned: &[String],
) -> Vec<String> {
    let addressed: Vec<String> =
        mentioned.iter().filter(|a| members.contains(a)).cloned().collect();
    if !addressed.is_empty() {
        return addressed;
    }
    lead.filter(|l| members.iter().any(|m| m == l)).map(|l| vec![l.to_string()]).unwrap_or_default()
}

/// A project name from a Space's display name: lowercase words joined by dashes, each
/// starting with a letter ("Nutrition & Meals 2" -> "nutrition-meals-n2"). Empty when the
/// name has nothing usable (all symbols, or no ASCII letters or digits).
pub fn slug(name: &str) -> String {
    let words: Vec<String> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| {
            let w = w.to_ascii_lowercase();
            if w.starts_with(|c: char| c.is_ascii_digit()) {
                format!("n{w}")
            } else {
                w
            }
        })
        .collect();
    let joined = words.join("-");
    joined.chars().take(40).collect::<String>().trim_end_matches('-').to_string()
}

impl Bridge {
    pub fn new(cfg: Config) -> Arc<Self> {
        let mx = Matrix::new(&cfg.homeserver, &cfg.as_token, &cfg.admin_token);
        let rt = Runtime::new(&cfg.runtime_url, &cfg.runtime_token);
        let state = State::load(cfg.state_file.clone());
        Arc::new(Self {
            cfg,
            mx,
            rt,
            state,
            seen_events: Mutex::new(Default::default()),
            reconciling: Mutex::new(()),
        })
    }

    /// A note to the owner, in the control room.
    fn tell_owner(&self, text: &str) {
        if let Some(room) = self.state.read(|d| d.control_room.clone()) {
            let _ = self.mx.send_text(&room, &self.bridge_user(), text, None);
        }
    }

    /// Puts the owner into a room the bridge made, so there is no invite to find.
    /// If that fails the invite is still pending, so say why and carry on.
    fn place_owner(&self, room: &str) {
        if let Err(e) = self.mx.join_as_owner(room) {
            log(&format!(
                "could not place {} in {room} (an invite is pending instead): {e}",
                self.cfg.owner
            ));
        }
    }

    fn bridge_user(&self) -> String {
        self.cfg.bridge_user()
    }

    /// Registers the bridge's own user. Called once at start.
    pub fn ensure_bridge_user(&self) -> R<()> {
        self.mx.register("holon-bridge")?;
        self.mx.set_displayname(&self.bridge_user(), "holon")
    }

    // ---- reconcile: make Matrix match the runtime ---------------------------

    pub fn reconcile(&self) -> Result<(), String> {
        let _one = self.reconciling.lock().unwrap();
        let agents = self.rt.agents()?;
        let projects = self.rt.projects()?;
        self.ensure_control_room().map_err(|e| format!("control room: {e}"))?;
        for a in &agents {
            if let Err(e) = self.ensure_agent(a) {
                log(&format!("agent {}: {e}", a.name));
            }
        }
        for p in &projects {
            if let Err(e) = self.ensure_project(p) {
                log(&format!("project {}: {e}", p.name));
            }
        }
        Ok(())
    }

    fn ensure_control_room(&self) -> R<()> {
        if self.state.read(|d| d.control_room.is_some()) {
            return Ok(());
        }
        let room = self.mx.create_room(
            &self.bridge_user(),
            json!({"name": "holon", "topic": "Control room. Say !help.", "preset": "trusted_private_chat", "invite": [self.cfg.owner]}),
        )?;
        self.place_owner(&room);
        self.state.write(|d| d.control_room = Some(room.clone()));
        self.mx.send_text(
            &room,
            &self.bridge_user(),
            "Control room. Try !help — projects and agents are managed here.",
            None,
        )?;
        Ok(())
    }

    fn ensure_agent(&self, a: &AgentInfo) -> R<()> {
        let ghost = self.cfg.ghost(&a.name);
        if !self.state.read(|d| d.dms.contains_key(&a.name)) {
            self.mx.register(&format!("agent-{}", a.name))?;
            self.mx.set_displayname(&ghost, &a.name)?;
            let room = self.mx.create_room(
                &ghost,
                json!({
                    "name": a.name,
                    "topic": a.description,
                    "is_direct": true,
                    "preset": "trusted_private_chat",
                    "invite": [self.cfg.owner],
                }),
            )?;
            self.place_owner(&room);
            self.state.write(|d| d.dms.insert(a.name.clone(), room));
        }
        Ok(())
    }

    fn ensure_project(&self, p: &ProjectInfo) -> R<()> {
        let bridge = self.bridge_user();
        let owner = self.cfg.owner.clone();
        let server = self.cfg.server_name.clone();
        let existing = self.state.read(|d| d.projects.get(&p.name).cloned());
        // A Space the owner made themselves: its rooms are whatever the owner put in it.
        if let Some(r) = existing.as_ref().filter(|r| r.general.is_empty() && r.feed.is_empty()) {
            return self.sync_adopted(p, &r.space);
        }
        let rooms = match existing {
            Some(r) => r,
            None => {
                let make = |name: String, topic: &str, space: bool| -> R<String> {
                    let mut body = json!({"name": name, "topic": topic, "preset": "trusted_private_chat", "invite": [owner]});
                    if space {
                        body["creation_content"] = json!({"type": "m.space"});
                    }
                    let room = self.mx.create_room(&bridge, body)?;
                    self.place_owner(&room);
                    Ok(room)
                };
                let space = make(p.name.clone(), &p.description, true)?;
                let general = make(
                    format!("{} · general", p.name),
                    "Talk to the project's agents. Mention one to address it.",
                    false,
                )?;
                let feed =
                    make(format!("{} · feed", p.name), "What the project's agents report.", false)?;
                for child in [&general, &feed] {
                    self.mx.set_state(
                        &space,
                        &bridge,
                        "m.space.child",
                        child,
                        json!({"via": [server]}),
                    )?;
                    self.mx.set_state(
                        child,
                        &bridge,
                        "m.space.parent",
                        &space,
                        json!({"via": [server], "canonical": true}),
                    )?;
                }
                let r = ProjectRooms { space, general, feed };
                self.state.write(|d| d.projects.insert(p.name.clone(), r.clone()));
                r
            }
        };
        // Membership: the runtime's list is the truth; the rooms follow it.
        for room in [&rooms.general, &rooms.feed] {
            let joined = self.mx.joined(room, &bridge)?;
            for agent in &p.agents {
                let ghost = self.cfg.ghost(agent);
                if !joined.contains(&ghost) {
                    self.mx.register(&format!("agent-{agent}"))?;
                    self.mx.invite(room, &bridge, &ghost)?;
                    self.mx.join(room, &ghost)?;
                }
            }
            for user in &joined {
                if let Some(agent) = self.cfg.agent_of(user) {
                    if !p.agents.contains(&agent) {
                        let _ = self.mx.kick(room, &bridge, user, "no longer in this project");
                    }
                }
            }
        }
        Ok(())
    }

    /// Keeps an adopted Space and every room in it matching the runtime's member list:
    /// member agents are invited in (as the owner: the bridge is not in these rooms),
    /// agents that are no longer members are removed.
    fn sync_adopted(&self, p: &ProjectInfo, space: &str) -> R<()> {
        let st = self.mx.room_state_as_owner(space)?;
        let children: Vec<String> = st
            .iter()
            .filter(|e| e["type"] == "m.space.child" && e["content"].get("via").is_some())
            .filter_map(|e| e["state_key"].as_str().filter(|k| !k.is_empty()).map(String::from))
            .collect();
        self.state.write(|d| {
            d.rooms.retain(|_, project| project != &p.name);
            for c in &children {
                d.rooms.insert(c.clone(), p.name.clone());
            }
        });
        for room in std::iter::once(space.to_string()).chain(children) {
            // a child the owner has since left cannot be managed; skip it
            let Ok(joined) = self.mx.joined_as_owner(&room) else { continue };
            for agent in &p.agents {
                let ghost = self.cfg.ghost(agent);
                if !joined.contains(&ghost) {
                    self.mx.register(&format!("agent-{agent}"))?;
                    self.mx.invite_as_owner(&room, &ghost)?;
                    self.mx.join(&room, &ghost)?;
                }
            }
            for user in &joined {
                if let Some(agent) = self.cfg.agent_of(user) {
                    if !p.agents.contains(&agent) {
                        let _ = self.mx.kick_as_owner(&room, user, "no longer in this project");
                    }
                }
            }
        }
        Ok(())
    }

    /// `agent` was invited to `room` (which it has joined) and the room is not one the
    /// bridge knows. If it is a Space, the Space becomes a project named after it, with
    /// `agent` as its first member. Returns whether it was adopted.
    fn adopt_space(&self, room: &str, agent: &str) -> bool {
        let ghost = self.cfg.ghost(agent);
        let Ok(st) = self.mx.room_state(room, &ghost) else { return false };
        let is_space =
            st.iter().any(|e| e["type"] == "m.room.create" && e["content"]["type"] == "m.space");
        if !is_space {
            return false;
        }
        let field = |ty: &str, key: &str| {
            st.iter()
                .find(|e| e["type"] == ty)
                .and_then(|e| e["content"][key].as_str())
                .unwrap_or_default()
                .to_string()
        };
        let (name, topic) = (field("m.room.name", "name"), field("m.room.topic", "topic"));
        let project = slug(&name);
        if project.is_empty() {
            self.tell_owner(&format!("I can't make a project from the Space \"{name}\": its name needs some letters or digits (a-z, 0-9). Rename it and invite {agent} again."));
            return true;
        }
        if self.state.read(|d| d.projects.get(&project).is_some_and(|r| r.space != room)) {
            self.tell_owner(&format!("There is already a project called {project} (from a different Space). Rename this Space and invite {agent} again."));
            return true;
        }
        let known =
            self.rt.projects().map(|ps| ps.iter().any(|x| x.name == project)).unwrap_or(false);
        let result = if known {
            self.rt.add_to_project(&project, agent)
        } else {
            self.rt.put_project(&ProjectInfo {
                name: project.clone(),
                description: topic,
                agents: vec![agent.to_string()],
                lead: None,
            })
        };
        match result {
            Ok(()) => {
                self.state.write(|d| {
                    d.projects.entry(project.clone()).or_insert(ProjectRooms {
                        space: room.to_string(),
                        general: String::new(),
                        feed: String::new(),
                    });
                });
                self.tell_owner(&format!(
                    "Adopted your Space \"{name}\" as the project {project}; {agent} is in it. Invite more agents to the Space or to any room in it; mention one to address it (or set a lead: !project lead {project} <agent>)."
                ));
            }
            Err(e) => self.tell_owner(&format!("Could not make {project} a project: {e}")),
        }
        true
    }

    /// The project whose Space is a parent of `room`, if any (by its `m.space.parent` state).
    fn parent_project(&self, room: &str, as_user: &str) -> Option<String> {
        let st = self.mx.room_state(room, as_user).ok()?;
        let parents: Vec<String> = st
            .iter()
            .filter(|e| e["type"] == "m.space.parent")
            .filter_map(|e| e["state_key"].as_str().map(String::from))
            .collect();
        self.state.read(|d| {
            d.projects.iter().find(|(_, r)| parents.contains(&r.space)).map(|(p, _)| p.clone())
        })
    }

    // ---- events from Synapse ---------------------------------------------

    /// True the first time an event id is seen (Synapse can deliver one twice).
    fn first_time(&self, event_id: &str) -> bool {
        if event_id.is_empty() {
            return true;
        }
        let mut g = self.seen_events.lock().unwrap();
        if !g.0.insert(event_id.to_string()) {
            return false;
        }
        g.1.push_back(event_id.to_string());
        while g.1.len() > 5000 {
            if let Some(old) = g.1.pop_front() {
                g.0.remove(&old);
            }
        }
        true
    }

    pub fn handle_events(self: &Arc<Self>, events: &[Value]) {
        for ev in events {
            if !self.first_time(ev["event_id"].as_str().unwrap_or_default()) {
                continue;
            }
            self.on_event(ev);
        }
    }

    fn on_event(self: &Arc<Self>, ev: &Value) {
        let sender = ev["sender"].as_str().unwrap_or_default();
        // Our own voices are not input: the bridge, and every agent's user.
        if sender == self.bridge_user() || self.cfg.agent_of(sender).is_some() {
            return;
        }
        match ev["type"].as_str().unwrap_or_default() {
            "m.room.message" if sender == self.cfg.owner => self.on_message(ev),
            "m.room.member" => self.on_member(ev),
            "org.matrix.msc3381.poll.response" | "m.poll.response" if sender == self.cfg.owner => {
                self.on_poll_response(ev)
            }
            _ => {}
        }
    }

    fn on_member(&self, ev: &Value) {
        let (Some(room), Some(sender), Some(target)) =
            (ev["room_id"].as_str(), ev["sender"].as_str(), ev["state_key"].as_str())
        else {
            return;
        };
        let Some(agent) = self.cfg.agent_of(target) else { return };
        if sender != self.cfg.owner {
            return;
        }
        let project_of = |kind: Option<RoomKind>| match kind {
            Some(RoomKind::ProjectGeneral(p))
            | Some(RoomKind::ProjectFeed(p))
            | Some(RoomKind::ProjectSpace(p))
            | Some(RoomKind::ProjectRoom(p)) => Some(p),
            _ => None,
        };
        match ev["content"]["membership"].as_str().unwrap_or_default() {
            // The owner invited an agent. In a project's room or Space it joins the project;
            // in a Space the bridge does not know yet, that Space BECOMES a project; in any
            // other room it just joins (and answers when addressed).
            "invite" => {
                if let Some(p) = project_of(self.state.kind_of(room)) {
                    match self.rt.add_to_project(&p, &agent) {
                        Ok(()) => log(&format!("{agent} added to project {p}")),
                        Err(e) => {
                            self.tell_owner(&format!("Could not add {agent} to {p}: {e}"));
                            return;
                        }
                    }
                    if let Err(e) = self.mx.join(room, target) {
                        log(&format!("{agent} could not join {room}: {e}"));
                    }
                    return;
                }
                if let Err(e) = self.mx.join(room, target) {
                    log(&format!("{agent} could not join {room}: {e}"));
                    return;
                }
                if self.adopt_space(room, &agent) {
                    return;
                }
                // a room inside an adopted Space: inviting an agent to it joins the project too
                if let Some(p) = self.parent_project(room, target) {
                    match self.rt.add_to_project(&p, &agent) {
                        Ok(()) => {
                            self.state.write(|d| d.rooms.insert(room.to_string(), p.clone()));
                            log(&format!("{agent} added to project {p} (via a room in its Space)"));
                        }
                        Err(e) => self.tell_owner(&format!("Could not add {agent} to {p}: {e}")),
                    }
                }
            }
            // The owner removed an agent: in a project room or Space it leaves the project.
            "leave" => {
                if let Some(p) = project_of(self.state.kind_of(room)) {
                    match self.rt.remove_from_project(&p, &agent) {
                        Ok(()) => log(&format!("{agent} removed from project {p}")),
                        Err(e) => log(&format!("removing {agent} from {p}: {e}")),
                    }
                }
            }
            _ => {}
        }
    }

    fn on_message(self: &Arc<Self>, ev: &Value) {
        let (Some(room), Some(event_id)) = (ev["room_id"].as_str(), ev["event_id"].as_str()) else {
            return;
        };
        let content = &ev["content"];
        if content["msgtype"].as_str() != Some("m.text") {
            return;
        }
        let body = content["body"].as_str().unwrap_or_default().trim().to_string();
        if body.is_empty() {
            return;
        }
        let agents: Vec<String> = match self.state.kind_of(room) {
            Some(RoomKind::Control) => {
                let me = self.clone();
                let (room, bridge) = (room.to_string(), self.bridge_user());
                std::thread::spawn(move || {
                    if let Some(reply) = me.command(&body) {
                        let _ = me.mx.send_text(&room, &bridge, &reply, None);
                    }
                });
                return;
            }
            Some(RoomKind::Dm(a)) => vec![a],
            Some(RoomKind::ProjectFeed(_)) | Some(RoomKind::ProjectSpace(_)) => return,
            Some(RoomKind::ProjectGeneral(p)) | Some(RoomKind::ProjectRoom(p)) => {
                let Some(info) =
                    self.rt.projects().ok().and_then(|ps| ps.into_iter().find(|x| x.name == p))
                else {
                    return;
                };
                project_targets(
                    &info.agents,
                    info.lead.as_deref(),
                    &mentioned_agents(&self.cfg, content),
                )
            }
            // A room the owner made by hand: whichever agents are in it and addressed.
            None => {
                let in_room: Vec<String> = self
                    .mx
                    .joined(room, &self.bridge_user())
                    .or_else(|_| self.mx.joined(room, &self.cfg.owner))
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|u| self.cfg.agent_of(u))
                    .collect();
                let mentioned: Vec<String> = mentioned_agents(&self.cfg, content)
                    .into_iter()
                    .filter(|a| in_room.contains(a))
                    .collect();
                if !mentioned.is_empty() {
                    mentioned
                } else if in_room.len() == 1 {
                    in_room
                } else {
                    return;
                }
            }
        };
        for agent in agents {
            let (me, room, event_id, body) =
                (self.clone(), room.to_string(), event_id.to_string(), body.clone());
            std::thread::spawn(move || me.answer(&agent, &room, &event_id, &body));
        }
    }

    /// Runs `agent` on `text` and posts its answer, as the agent, in `room`.
    fn answer(&self, agent: &str, room: &str, reply_to: &str, text: &str) {
        let ghost = self.cfg.ghost(agent);
        // The agent may not be in this room yet (a hand-made one it was invited to).
        let _ = self.mx.join(room, &ghost);
        self.mx.typing(room, &ghost, true);
        let (ok, answer) = self.rt.run(agent, text, None);
        self.mx.typing(room, &ghost, false);
        let body = if ok {
            if answer.trim().is_empty() {
                "(no answer)".to_string()
            } else {
                clip(&answer, MAX_REPLY)
            }
        } else {
            format!("⚠️ {}", clip(&answer, 2_000))
        };
        if let Err(e) = self.mx.send_text(room, &ghost, &body, Some(reply_to)) {
            log(&format!("{agent}: could not post the answer: {e}"));
        }
    }

    // ---- approvals as polls ---------------------------------------------------

    /// Posts a poll for every pending approval that has none yet, and forgets the
    /// polls of approvals that are no longer pending (answered elsewhere, or timed out).
    pub fn poll_approvals(&self) {
        let Ok(pending) = self.rt.approvals() else { return };
        let have: HashSet<u64> = self.state.read(|d| d.polls.values().copied().collect());
        for a in &pending {
            if have.contains(&a.id) {
                continue;
            }
            let Some(room) = self.state.read(|d| d.dms.get(&a.agent).cloned()) else { continue };
            let via = if a.chain.is_empty() {
                String::new()
            } else {
                format!(" (woken via {})", a.chain.join(" › "))
            };
            let q = format!(
                "{} wants to run {} {}{via}. Approve?",
                a.agent,
                a.tool,
                clip(&a.args.to_string(), 300)
            );
            let content = json!({
                "org.matrix.msc1767.text": format!("{q}\n1. Approve\n2. Deny"),
                "org.matrix.msc3381.poll.start": {
                    "kind": "org.matrix.msc3381.poll.disclosed",
                    "max_selections": 1,
                    "question": {"org.matrix.msc1767.text": q},
                    "answers": [
                        {"id": "approve", "org.matrix.msc1767.text": "Approve"},
                        {"id": "deny", "org.matrix.msc1767.text": "Deny"},
                    ],
                },
            });
            match self.mx.send_event(
                &room,
                &self.cfg.ghost(&a.agent),
                "org.matrix.msc3381.poll.start",
                content,
            ) {
                Ok(event_id) => {
                    self.state.write(|d| d.polls.insert(event_id, a.id));
                }
                Err(e) => log(&format!("approval {}: could not post the poll: {e}", a.id)),
            }
        }
        let live: HashSet<u64> = pending.iter().map(|a| a.id).collect();
        self.state.write(|d| d.polls.retain(|_, id| live.contains(id)));
    }

    fn on_poll_response(&self, ev: &Value) {
        let content = &ev["content"];
        let Some(poll) = content["m.relates_to"]["event_id"].as_str() else { return };
        let Some(approval) = self.state.read(|d| d.polls.get(poll).copied()) else { return };
        let answer = content["org.matrix.msc3381.poll.response"]["answers"][0]
            .as_str()
            .or_else(|| content["m.selections"][0].as_str())
            .unwrap_or_default();
        let approve = match answer {
            "approve" => true,
            "deny" => false,
            _ => return,
        };
        match self.rt.resolve(approval, approve) {
            Ok(()) => {
                self.state.write(|d| d.polls.remove(poll));
                log(&format!(
                    "approval {approval}: {}",
                    if approve { "approved" } else { "denied" }
                ));
            }
            Err(e) => log(&format!("approval {approval}: {e}")),
        }
    }

    // ---- background loops ---------------------------------------------------

    /// Starts the reconcile and approval loops on their own threads.
    pub fn spawn_loops(self: &Arc<Self>) {
        let me = self.clone();
        std::thread::spawn(move || loop {
            if let Err(e) = me.reconcile() {
                log(&format!("reconcile: {e}"));
            }
            std::thread::sleep(Duration::from_secs(5));
        });
        let me = self.clone();
        std::thread::spawn(move || loop {
            me.poll_approvals();
            std::thread::sleep(Duration::from_secs(2));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            homeserver: "http://x".into(),
            server_name: "s.net".into(),
            as_token: "a".into(),
            hs_token: "h".into(),
            listen: "0.0.0.0:1".into(),
            runtime_url: "http://r".into(),
            runtime_token: "t".into(),
            owner: "@mark:s.net".into(),
            admin_token: "x".into(),
            state_file: "/tmp/none.json".into(),
        }
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn structured_mentions_and_text_mentions_both_address_an_agent() {
        let c = cfg();
        let m = json!({"body": "rower: how was it?", "m.mentions": {"user_ids": ["@agent-rower:s.net", "@mark:s.net"]}});
        assert_eq!(mentioned_agents(&c, &m), ["rower"]);
        let t = json!({"body": "hey @coach and @agent-rower:s.net, thoughts?"});
        assert_eq!(mentioned_agents(&c, &t), ["coach", "rower"]);
        assert!(
            mentioned_agents(&c, &json!({"body": "no mention here, mail me at a@b.c"})).is_empty()
        );
        // the same agent twice is addressed once
        let d =
            json!({"body": "@rower @rower", "m.mentions": {"user_ids": ["@agent-rower:s.net"]}});
        assert_eq!(mentioned_agents(&c, &d), ["rower"]);
    }

    #[test]
    fn in_a_project_the_mentioned_members_answer_else_the_lead_else_nobody() {
        let members = names(&["rower", "coach", "chef"]);
        assert_eq!(project_targets(&members, Some("coach"), &names(&["rower"])), ["rower"]);
        assert_eq!(
            project_targets(&members, Some("coach"), &names(&["rower", "chef"])),
            ["rower", "chef"]
        );
        assert_eq!(
            project_targets(&members, Some("coach"), &[]),
            ["coach"],
            "no mention: the lead"
        );
        assert!(project_targets(&members, None, &[]).is_empty(), "no mention, no lead: nobody");
        // someone who is not in the project is not addressed by mentioning them
        assert_eq!(project_targets(&members, Some("coach"), &names(&["stranger"])), ["coach"]);
        // a lead that is no longer a member does not answer
        assert!(project_targets(&names(&["rower"]), Some("gone"), &[]).is_empty());
    }

    #[test]
    fn a_spaces_name_becomes_a_valid_project_name() {
        assert_eq!(slug("Nutrition"), "nutrition");
        assert_eq!(slug("Nutrition & Meals 2"), "nutrition-meals-n2");
        assert_eq!(slug("  Rowing — 2026 plan! "), "rowing-n2026-plan");
        assert_eq!(slug("2026"), "n2026");
        assert_eq!(slug("!!!"), "");
        assert_eq!(slug(""), "");
        assert!(slug(&"word ".repeat(30)).len() <= 40);
        assert!(!slug(&"word ".repeat(30)).ends_with('-'));
        // whatever it returns is a name the runtime accepts: lowercase words, each starting with a letter
        for n in ["Nutrition & Meals 2", "9 lives", "a--b", "Z"] {
            let s = slug(n);
            assert!(
                s.split('-').all(|w| w.chars().next().is_some_and(|c| c.is_ascii_lowercase())),
                "{n} -> {s}"
            );
        }
    }
}
