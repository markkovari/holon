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
use crate::retire::Verdict;
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
    /// How many passes each agent the bridge keeps a room for has been missing from the runtime.
    absence: Mutex<crate::retire::Absence>,
    /// Whether the owner has been told that a mass disappearance was left alone.
    refusal_told: Mutex<bool>,
}

fn log(msg: &str) {
    eprintln!("agent-matrix: {msg}");
}

pub fn clip(s: &str, n: usize) -> String {
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

/// The top-ranked member, but only when it beats the runner-up by a clear margin. Embedding
/// scores of unrelated text sit high, so a bare top-1 is a guess; a near tie goes to the lead.
pub fn clear_winner(ranked: &[(String, f32)]) -> Option<String> {
    const MARGIN: f32 = 0.04;
    match ranked {
        [(a, x), (_, y), ..] if x - y >= MARGIN => Some(a.clone()),
        [(a, _)] => Some(a.clone()),
        _ => None,
    }
}

/// The recent messages as `name: text` lines, oldest first, at most `budget` characters
/// (the newest are kept), each message trimmed; the message `skip` and empty ones left out.
/// `msgs` is the Matrix `/messages` chunk, newest first.
pub fn history_lines(cfg: &Config, msgs: &[Value], skip: &str, budget: usize) -> String {
    const PER_MESSAGE: usize = 400;
    let mut picked: Vec<String> = Vec::new();
    let mut used = 0;
    for m in msgs {
        if m["event_id"].as_str() == Some(skip) {
            continue;
        }
        // A photo is its caption (or nothing) and not its file name: "image.png" tells the agent
        // nothing and reads as a message that failed to arrive.
        let photo_line;
        let body = if m["content"]["msgtype"].as_str() == Some("m.image") {
            let caption = photo_caption(&m["content"]);
            photo_line = if caption.is_empty() {
                "[a photo]".to_string()
            } else {
                format!("[a photo] {caption}")
            };
            photo_line.as_str()
        } else {
            m["content"]["body"].as_str().unwrap_or_default().trim()
        };
        if body.is_empty() || m["content"]["m.relates_to"]["rel_type"].as_str() == Some("m.replace")
        {
            continue;
        }
        let sender = m["sender"].as_str().unwrap_or_default();
        let who = match cfg.agent_of(sender) {
            Some(a) => a,
            None => sender.trim_start_matches('@').split(':').next().unwrap_or(sender).to_string(),
        };
        let line = format!("{who}: {}", clip(body, PER_MESSAGE));
        if used + line.len() > budget && !picked.is_empty() {
            break;
        }
        used += line.len();
        picked.push(line);
    }
    picked.reverse();
    picked.join("\n")
}

/// Who answers a message in a project room: the agents mentioned, else the lead, else a
/// lone member, else nobody.
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
    if let Some(l) = lead.filter(|l| members.iter().any(|m| m == l)) {
        return vec![l.to_string()];
    }
    // A project of one is what a direct room was: its only member answers.
    match members {
        [only] => vec![only.clone()],
        _ => Vec::new(),
    }
}

/// What an approval would do, in words, for the tools whose name alone says too little
/// (`agent_delete` reads like "remove it from the list" and destroys the run history).
/// `None` for any other tool: the poll then shows the call as it is.
pub fn describe_approval(tool: &str, args: &Value) -> Option<String> {
    let name = args["name"].as_str().unwrap_or("?");
    Some(match tool {
        "agent_delete" => format!(
            "delete the agent `{name}`: its settings, schedules, run history, memory and \
             workspace move to the trash and can be restored for 30 days, then they are gone. \
             To only stop a schedule, deny this and ask for the schedule to be removed; to stop \
             it for now, ask for it to be paused."
        ),
        "agent_restore" => format!(
            "restore the deleted agent `{name}` from the trash, exactly as it was. It starts \
             running on its schedules again."
        ),
        "agent_remove_schedule" => {
            let which = match args["schedule"].as_str().filter(|s| !s.is_empty()) {
                Some(s) => format!("the schedule `{s}`"),
                None => "ALL schedules".to_string(),
            };
            let which = match args["project"].as_str().filter(|p| !p.is_empty()) {
                Some(p) => format!("{which} that work for the project `{p}`"),
                None => which,
            };
            format!(
                "remove {which} from the agent `{name}`. The agent, its run history, its memory \
                 and its other triggers stay."
            )
        }
        "agent_save" => {
            let mut fields: Vec<&str> = args
                .as_object()
                .map(|o| o.keys().map(String::as_str).filter(|k| *k != "name").collect())
                .unwrap_or_default();
            fields.sort_unstable();
            let replaces = if fields.contains(&"schedule") {
                " Giving a schedule replaces all of its current triggers with that one."
            } else {
                ""
            };
            let joins = match args["project"].as_str().filter(|p| !p.is_empty()) {
                Some(p) => format!(
                    " It is ADDED TO the project `{p}`, which grants it that project's shared \
                     store and topics, and its schedule works for it: results are posted in \
                     that project's room."
                ),
                None => String::new(),
            };
            format!(
                "create or change the agent `{name}` (fields: {}).{replaces}{joins}",
                fields.join(", ")
            )
        }
        _ => return None,
    })
}

/// A photo is still the subject for this long: past it, a text message is about something else.
const PHOTO_FOLLOW_UP_MS: u64 = 15 * 60 * 1000;

/// The photo a text message is probably about: the newest message from `owner` in `msgs` (newest
/// first), when it is a photo, is recent, and `agent` was given it. Returns its file name in that
/// agent's inbox and its age in minutes. Only the newest photo is considered: if that one did not
/// reach this agent, an older one is not dug up.
pub fn recent_photo(
    msgs: &[Value],
    owner: &str,
    skip: &str,
    now_ms: u64,
    photos: &[crate::state::PhotoRef],
    agent: &str,
) -> Option<(String, u64)> {
    let m = msgs.iter().find(|m| {
        m["sender"].as_str() == Some(owner)
            && m["content"]["msgtype"].as_str() == Some("m.image")
            && m["event_id"].as_str() != Some(skip)
    })?;
    let age = now_ms.saturating_sub(m["origin_server_ts"].as_u64()?);
    if age > PHOTO_FOLLOW_UP_MS {
        return None;
    }
    let id = m["event_id"].as_str()?;
    let file = photos.iter().rev().find(|p| p.agent == agent && p.event == id)?.file.clone();
    Some((file, age / 60_000))
}

/// The words a person wrote with a photo: Element puts the file name in `body` unless there is
/// a caption, in which case `filename` holds the name and `body` the caption.
pub fn photo_caption(content: &Value) -> String {
    let body = content["body"].as_str().unwrap_or_default().trim();
    match content["filename"].as_str() {
        Some(f) if f == body => String::new(),
        Some(_) => body.to_string(),
        None if looks_like_file_name(body) => String::new(),
        None => body.to_string(),
    }
}

fn looks_like_file_name(s: &str) -> bool {
    !s.contains(' ')
        && s.rsplit_once('.').is_some_and(|(_, e)| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "webp" | "gif" | "heic" | "heif"
            )
        })
}

/// The name to keep a photo under: its own, with an extension the runtime accepts (from its
/// type when the name has none, as a camera roll upload sometimes does).
pub fn photo_name(content: &Value) -> String {
    let given = content["filename"]
        .as_str()
        .or(content["body"].as_str())
        .unwrap_or("photo")
        .trim()
        .to_string();
    let name = if given.is_empty() || given.contains(' ') && !looks_like_file_name(&given) {
        "photo".to_string()
    } else {
        given
    };
    if looks_like_file_name(&name) {
        return name;
    }
    let ext = match content["info"]["mimetype"].as_str().unwrap_or_default() {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/heic" | "image/heif" => "heic",
        _ => "jpg",
    };
    format!("{name}.{ext}")
}

/// What to say aloud of a written answer: the start of it, cut at a sentence end, short
/// enough to listen to. The full text is always in the room.
pub fn spoken_excerpt(answer: &str) -> String {
    const LIMIT: usize = 700;
    let a = answer.trim();
    if a.chars().count() <= LIMIT {
        return a.to_string();
    }
    let head: String = a.chars().take(LIMIT).collect();
    let cut =
        head.rfind(['.', '!', '?', '\n']).filter(|i| *i > LIMIT / 3).map_or(head.len(), |i| i + 1);
    format!("{} The rest is in the message.", head[..cut].trim())
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
            absence: Mutex::default(),
            refusal_told: Mutex::new(false),
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
        // Before `ensure_agent`: a restored agent gets its old room back and not a new one.
        self.retire_gone_agents(&agents);
        for a in &agents {
            let in_project = projects.iter().any(|p| p.agents.contains(&a.name));
            if let Err(e) = self.ensure_agent(a, in_project) {
                log(&format!("agent {}: {e}", a.name));
            }
        }
        for p in &projects {
            // The runtime says whether a project is archived; the rooms follow it.
            if p.archived {
                self.archive_rooms(p);
                continue;
            }
            self.restore_rooms(p);
            if let Err(e) = self.ensure_project(p) {
                log(&format!("project {}: {e}", p.name));
            }
        }
        Ok(())
    }

    // ---- an agent that was deleted (ADR-0105, section 3) ----------------------

    /// Retires the rooms of agents the runtime no longer lists, brings back the room of one
    /// that was restored, and lets go of the room of one that left the trash.
    fn retire_gone_agents(&self, active: &[AgentInfo]) {
        let live: HashSet<String> = active.iter().map(|a| a.name.clone()).collect();
        for (name, room) in self.state.read(|d| d.retired.clone()) {
            if live.contains(&name) {
                self.revive(&name, &room);
            }
        }
        let known: Vec<String> = self.state.read(|d| d.dms.keys().cloned().collect());
        let verdict = self.absence.lock().unwrap().observe(&known, &live);
        let waiting: Vec<(String, String)> = self
            .state
            .read(|d| d.retired.clone())
            .into_iter()
            .filter(|(n, _)| !live.contains(n))
            .collect();
        if waiting.is_empty() && matches!(verdict, Verdict::Nothing) {
            *self.refusal_told.lock().unwrap() = false;
            return;
        }
        // What the trash holds decides what a farewell can promise. Without it, try again.
        let Ok(trash) = self.rt.trash() else { return };
        let in_trash = |n: &str| trash.iter().any(|t| t.name == n);
        for (name, room) in waiting {
            if !in_trash(&name) {
                let _ = self.mx.leave(&room, &self.cfg.ghost(&name));
                self.state.write(|d| d.retired.remove(&name));
            }
        }
        match verdict {
            Verdict::Nothing => *self.refusal_told.lock().unwrap() = false,
            Verdict::Retire(gone) => {
                *self.refusal_told.lock().unwrap() = false;
                for name in gone {
                    self.retire(&name, in_trash(&name));
                }
            }
            Verdict::Refuse(gone) => {
                if !std::mem::replace(&mut *self.refusal_told.lock().unwrap(), true) {
                    self.tell_owner(&format!(
                        "{} of my {} agents ({}) are missing from the runtime at once. That looks \
                         like a runtime started against the wrong state directory, not a set of \
                         deletions, so I am leaving their rooms alone. If they really were \
                         deleted, retire them one by one: !agent retire <agent>.",
                        gone.len(),
                        self.state.read(|d| d.dms.len()),
                        gone.join(", ")
                    ));
                }
            }
        }
    }

    /// Ends an agent's direct room: it says so, its name shows it, and the owner leaves.
    /// A deleted agent still in the trash keeps its user in the room, so that restoring it
    /// can invite the owner back to the same room. Otherwise its user leaves too.
    fn retire(&self, name: &str, in_trash: bool) {
        let Some(room) = self.state.read(|d| d.dms.get(name).cloned()) else { return };
        let ghost = self.cfg.ghost(name);
        let note = if in_trash {
            format!(
                "{name} was deleted. It can be restored for 30 days: send `!agent restore {name}` \
                 in the holon room."
            )
        } else {
            format!("{name} was deleted.")
        };
        let _ = self.mx.send_text(&room, &ghost, &note, None);
        let _ = self.mx.set_displayname(&ghost, &format!("{name} (deleted)"));
        if let Err(e) = self.mx.leave_and_forget_as_owner(&room) {
            log(&format!("{name}: could not leave its room: {e}"));
        }
        self.set_agents_space_child(&room, false);
        if in_trash {
            self.state.write(|d| {
                d.dms.remove(name);
                d.retired.insert(name.to_string(), room);
            });
        } else {
            let _ = self.mx.leave(&room, &ghost);
            self.state.write(|d| d.dms.remove(name));
        }
        self.tell_owner(&format!(
            "{name} was deleted, so its room is gone from your list{}.",
            if in_trash { "; !agent restore brings both back" } else { "" }
        ));
    }

    /// Lists `room` in the Agents Space, or takes it out (an empty child event removes it),
    /// so a retired agent's room is not left behind as an entry nobody can open.
    fn set_agents_space_child(&self, room: &str, listed: bool) {
        let Some(space) = self.state.read(|d| d.agents_space.clone()) else { return };
        let content = if listed { json!({"via": [self.cfg.server_name]}) } else { json!({}) };
        let _ = self.mx.set_state(&space, &self.bridge_user(), "m.space.child", room, content);
    }

    /// A restored agent: the same room, the same user, the owner invited back.
    fn revive(&self, name: &str, room: &str) {
        let ghost = self.cfg.ghost(name);
        let _ = self.mx.set_displayname(&ghost, name);
        if let Err(e) = self.mx.invite(room, &ghost, &self.cfg.owner) {
            log(&format!("{name}: could not invite you back: {e}"));
        }
        self.place_owner(room);
        self.set_agents_space_child(room, true);
        let _ = self.mx.send_text(room, &ghost, &format!("{name} is back."), None);
        self.state.write(|d| {
            d.retired.remove(name);
            d.dms.insert(name.to_string(), room.to_string());
        });
        self.tell_owner(&format!("{name} was restored; its old room is back in your list."));
    }

    /// `!agent retire <agent>`: retire the room of an agent the runtime does not list, for
    /// when the automatic pass refused to.
    pub fn retire_now(&self, name: &str) -> String {
        match self.rt.agents() {
            Err(e) => return format!("could not read the agents: {e}"),
            Ok(a) if a.iter().any(|x| x.name == name) => {
                return format!("{name} still exists in the runtime; delete it there first.")
            }
            Ok(_) => {}
        }
        if self.state.read(|d| !d.dms.contains_key(name)) {
            return format!("I have no room for {name}.");
        }
        let in_trash = self.rt.trash().map(|t| t.iter().any(|a| a.name == name)).unwrap_or(false);
        self.retire(name, in_trash);
        format!("Retired {name}.")
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

    /// The Space that holds the room of every agent that is in no project.
    fn ensure_agents_space(&self) -> R<String> {
        if let Some(s) = self.state.read(|d| d.agents_space.clone()) {
            return Ok(s);
        }
        let space = self.mx.create_room(
            &self.bridge_user(),
            json!({
                "name": "Agents",
                "topic": "Agents that are in no project. Add one to a project to move it.",
                "preset": "trusted_private_chat",
                "invite": [self.cfg.owner],
                "creation_content": {"type": "m.space"},
            }),
        )?;
        self.place_owner(&space);
        self.state.write(|d| d.agents_space = Some(space.clone()));
        Ok(space)
    }

    /// An agent that is in no project gets a room in the Agents Space. One that is in a
    /// project needs none: it is in that project's rooms. An agent that already has a
    /// room (a direct room from before the Agents Space) keeps it.
    fn ensure_agent(&self, a: &AgentInfo, in_project: bool) -> R<()> {
        if in_project || self.state.read(|d| d.dms.contains_key(&a.name)) {
            return Ok(());
        }
        let (bridge, ghost) = (self.bridge_user(), self.cfg.ghost(&a.name));
        let space = self.ensure_agents_space()?;
        self.mx.register(&format!("agent-{}", a.name))?;
        self.mx.set_displayname(&ghost, &a.name)?;
        let room = self.mx.create_room(
            &ghost,
            json!({
                "name": a.name,
                "topic": a.description,
                "preset": "trusted_private_chat",
                "invite": [self.cfg.owner],
            }),
        )?;
        let via = json!({"via": [self.cfg.server_name]});
        self.mx.set_state(&space, &bridge, "m.space.child", &room, via.clone())?;
        self.mx.set_state(
            &room,
            &ghost,
            "m.space.parent",
            &space,
            json!({"via": [self.cfg.server_name], "canonical": true}),
        )?;
        self.place_owner(&room);
        self.state.write(|d| d.dms.insert(a.name.clone(), room));
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
        let make = |name: String, topic: &str, space: bool| -> R<String> {
            let mut body = json!({"name": name, "topic": topic, "preset": "trusted_private_chat", "invite": [owner]});
            if space {
                body["creation_content"] = json!({"type": "m.space"});
            }
            let room = self.mx.create_room(&bridge, body)?;
            self.place_owner(&room);
            Ok(room)
        };
        let link = |space: &str, child: &str| -> R<()> {
            self.mx.set_state(space, &bridge, "m.space.child", child, json!({"via": [server]}))?;
            self.mx.set_state(
                child,
                &bridge,
                "m.space.parent",
                space,
                json!({"via": [server], "canonical": true}),
            )
        };
        let make_feed = |space: &str| -> R<String> {
            let feed =
                make(format!("{} · feed", p.name), "What the project's agents report.", false)?;
            link(space, &feed)?;
            Ok(feed)
        };
        let mut rooms = match existing {
            Some(r) => r,
            None => {
                // A project of none or one agent is one room: with no one else to keep a
                // report apart from, a feed would only be a second place to look. The
                // feed appears when a second agent joins.
                let space = make(p.name.clone(), &p.description, true)?;
                let general = make(
                    format!("{} · general", p.name),
                    "Talk to the project's agents. Mention one to address it.",
                    false,
                )?;
                link(&space, &general)?;
                let feed = if p.agents.len() >= 2 { make_feed(&space)? } else { String::new() };
                let r = ProjectRooms { space, general, feed };
                self.state.write(|d| d.projects.insert(p.name.clone(), r.clone()));
                r
            }
        };
        if rooms.feed.is_empty() && p.agents.len() >= 2 {
            rooms.feed = make_feed(&rooms.space)?;
            let r = rooms.clone();
            self.state.write(|d| d.projects.insert(p.name.clone(), r));
        }
        // Membership: the runtime's list is the truth; the rooms follow it.
        for room in [&rooms.general, &rooms.feed].into_iter().filter(|r| !r.is_empty()) {
            let joined = self.mx.joined(room, &bridge)?;
            for agent in &p.agents {
                let ghost = self.cfg.ghost(agent);
                if !joined.contains(&ghost) {
                    self.mx.register(&format!("agent-{agent}"))?;
                    let _ = self.mx.set_displayname(&ghost, agent);
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
                ..Default::default()
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
        // A voice message (or any audio) is heard, a photo is kept for the agent, text is read.
        // Nothing else is input.
        let msgtype = content["msgtype"].as_str();
        let heard = msgtype == Some("m.audio");
        let photo = msgtype == Some("m.image");
        if !heard && !photo && msgtype != Some("m.text") {
            return;
        }
        let body = if heard {
            String::new()
        } else if photo {
            photo_caption(content)
        } else {
            content["body"].as_str().unwrap_or_default().trim().to_string()
        };
        if !heard && !photo && body.is_empty() {
            return;
        }
        let agents: Vec<String> = match self.state.kind_of(room) {
            // commands are typed; a voice message or a photo here is ignored
            Some(RoomKind::Control) if heard || photo => return,
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
            Some(RoomKind::ProjectSpace(_)) => return,
            Some(RoomKind::ProjectGeneral(p))
            | Some(RoomKind::ProjectRoom(p))
            | Some(RoomKind::ProjectFeed(p)) => {
                let Some(info) =
                    self.rt.projects().ok().and_then(|ps| ps.into_iter().find(|x| x.name == p))
                else {
                    return;
                };
                let mentioned = mentioned_agents(&self.cfg, content);
                let named = mentioned.iter().any(|a| info.agents.contains(a));
                // Nobody named: when an embedding service is configured and one member
                // fits clearly better than the rest, that member answers; else the lead.
                // Jev first when a key file is configured; else (or when it cannot say) the
                // embedding margin.
                let jev_key = std::env::var("HOLON_JEV_KEY_FILE").unwrap_or_default();
                let by_jev = if named || jev_key.is_empty() || info.agents.len() < 2 {
                    None
                } else {
                    let known = self.rt.agents().unwrap_or_default();
                    let cands: Vec<(String, String)> = info
                        .agents
                        .iter()
                        .map(|a| {
                            let d = known.iter().find(|k| &k.name == a);
                            (a.clone(), d.map(|k| k.description.clone()).unwrap_or_default())
                        })
                        .collect();
                    crate::jev::affected(&jev_key, &body, &cands).filter(|v| !v.is_empty())
                };
                let judged = if by_jev.is_some() {
                    None
                } else {
                    // the runtime owns the embedding service (and starts it when needed)
                    match (!named && info.agents.len() > 1).then_some(()) {
                        Some(()) => {
                            let known = self.rt.agents().unwrap_or_default();
                            let cands: Vec<(String, String)> = info
                                .agents
                                .iter()
                                .map(|a| {
                                    let d = known.iter().find(|k| &k.name == a);
                                    (
                                        a.clone(),
                                        d.map(|k| k.description.clone()).unwrap_or_default(),
                                    )
                                })
                                .collect();
                            self.rt.rank(&body, &cands).and_then(|r| clear_winner(&r))
                        }
                        None => None,
                    }
                };
                by_jev.or_else(|| judged.map(|a| vec![a])).unwrap_or_else(|| {
                    project_targets(&info.agents, info.lead.as_deref(), &mentioned)
                })
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
            let (me, room, event_id, body, content) = (
                self.clone(),
                room.to_string(),
                event_id.to_string(),
                body.clone(),
                content.clone(),
            );
            if heard {
                std::thread::spawn(move || me.answer_voice(&agent, &room, &event_id, &content));
            } else if photo {
                std::thread::spawn(move || {
                    me.answer_photo(&agent, &room, &event_id, &body, &content)
                });
            } else {
                std::thread::spawn(move || me.answer(&agent, &room, &event_id, &body));
            }
        }
    }

    /// A photo to an agent: download it, put it in the agent's inbox in the runtime, and run the
    /// agent with a note saying a photo arrived and what it is called there, so its photo tool
    /// can look at it. The answer is posted as the agent, like any other.
    fn answer_photo(
        &self,
        agent: &str,
        room: &str,
        reply_to: &str,
        caption: &str,
        content: &Value,
    ) {
        let ghost = self.cfg.ghost(agent);
        let _ = self.mx.join(room, &ghost);
        let note = |text: &str| {
            let _ = self.mx.send_text(room, &ghost, text, Some(reply_to));
        };
        let Some(mxc) = content["url"].as_str() else {
            return note("I can only look at photos in unencrypted rooms (this one is encrypted, or the message has no image).");
        };
        self.mx.typing(room, &ghost, true);
        let name = photo_name(content);
        let kept = self
            .mx
            .download(mxc, &ghost)
            .map_err(|e| format!("I could not fetch that photo: {e}"))
            .and_then(|bytes| {
                self.rt
                    .upload_inbox(agent, &name, bytes)
                    .map_err(|e| format!("I could not keep that photo: {}", clip(&e, 300)))
            });
        let file = match kept {
            Ok(f) => {
                // remember which message this was, so a follow-up text can bring it back
                self.state.write(|d| {
                    d.photos.push(crate::state::PhotoRef {
                        agent: agent.to_string(),
                        event: reply_to.to_string(),
                        file: f.clone(),
                    });
                    let extra = d.photos.len().saturating_sub(100);
                    d.photos.drain(..extra);
                });
                f
            }
            Err(e) => {
                self.mx.typing(room, &ghost, false);
                return note(&format!("⚠️ {e}"));
            }
        };
        let said = if caption.is_empty() {
            String::new()
        } else {
            format!(" with the caption: {caption}")
        };
        // The photo goes to the agent's own model with the message (it is attached), and stays in
        // its inbox; a model that cannot see is told so by the runtime.
        let text = format!(
            "Mark sent a photo{said}. It is attached to this message and saved in your inbox as `{file}`."
        );
        let (ok, answer) = self.rt.run(
            agent,
            &self.task(room, reply_to, &text),
            &self.why(room),
            None,
            self.project_of(room).as_deref(),
            std::slice::from_ref(&file),
        );
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
        if let Err(e) = self.mx.send_markdown(room, &ghost, &body, Some(reply_to)) {
            log(&format!("{agent}: could not post the answer: {e}"));
        }
    }

    /// A voice message to an agent: download it, transcribe it, show what was heard, run
    /// the agent on the transcript, and answer in text AND in voice (voice in, voice out).
    fn answer_voice(&self, agent: &str, room: &str, reply_to: &str, content: &Value) {
        // The agent says what it heard, and speaks its own errors: the bridge's user is not
        // a member of an agent's direct room, but the agent is.
        let ghost = self.cfg.ghost(agent);
        let _ = self.mx.join(room, &ghost);
        let note = |text: &str| {
            let _ = self.mx.send_text(room, &ghost, text, Some(reply_to));
        };
        let Some(mxc) = content["url"].as_str() else {
            note("I can only hear voice messages in unencrypted rooms (this one is encrypted, or the message has no audio).");
            return;
        };
        let audio = match self.mx.download(mxc, &ghost) {
            Ok(a) => a,
            Err(e) => return note(&format!("⚠️ I could not fetch that recording: {e}")),
        };
        self.mx.typing(room, &ghost, true);
        let heard = self.rt.transcribe(audio);
        let text = match heard {
            Ok(t) if t.trim().is_empty() => {
                self.mx.typing(room, &ghost, false);
                return note("🎙️ I did not catch anything in that recording.");
            }
            Ok(t) => t.trim().to_string(),
            Err(e) => {
                self.mx.typing(room, &ghost, false);
                let why = if e.contains("not configured") {
                    "speech-to-text is not set up on the runtime (start it with --stt-bin)"
                        .to_string()
                } else {
                    clip(&e, 300)
                };
                return note(&format!("⚠️ I could not understand that recording: {why}"));
            }
        };
        note(&format!("🎙️ “{}”", clip(&text, 1_000)));
        let (ok, answer) = self.run_with_follow_up(agent, room, reply_to, &text);
        self.mx.typing(room, &ghost, false);
        let written = if ok {
            if answer.trim().is_empty() {
                "(no answer)".to_string()
            } else {
                clip(&answer, MAX_REPLY)
            }
        } else {
            format!("⚠️ {}", clip(&answer, 2_000))
        };
        if let Err(e) = self.mx.send_markdown(room, &ghost, &written, Some(reply_to)) {
            log(&format!("{agent}: could not post the answer: {e}"));
        }
        // ...and the same answer, spoken, if the runtime can speak.
        if ok && self.rt.speech_caps().1 {
            self.speak_in(room, &ghost, &spoken_excerpt(&answer), reply_to);
        }
    }

    /// Posts `text` as a voice message from `ghost`.
    fn speak_in(&self, room: &str, ghost: &str, text: &str, reply_to: &str) {
        let spoken = match self.rt.speak(text) {
            Ok(s) => s,
            Err(e) => return log(&format!("could not speak: {e}")),
        };
        let size = spoken.audio.len();
        let uri = match self.mx.upload(spoken.audio, "audio/ogg", "voice-message.ogg", ghost) {
            Ok(u) => u,
            Err(e) => return log(&format!("could not upload the voice message: {e}")),
        };
        let content = json!({
            "msgtype": "m.audio",
            "body": "Voice message",
            "url": uri,
            "info": {"mimetype": "audio/ogg", "size": size, "duration": spoken.duration_ms},
            "org.matrix.msc1767.audio": {"duration": spoken.duration_ms, "waveform": spoken.waveform},
            "org.matrix.msc3245.voice": {},
            "m.relates_to": {"m.in_reply_to": {"event_id": reply_to}},
        });
        if let Err(e) = self.mx.send_event(room, ghost, "m.room.message", content) {
            log(&format!("could not post the voice message: {e}"));
        }
    }

    /// Runs `agent` on `text` and posts its answer, as the agent, in `room`.
    fn answer(&self, agent: &str, room: &str, reply_to: &str, text: &str) {
        let ghost = self.cfg.ghost(agent);
        // The agent may not be in this room yet (a hand-made one it was invited to).
        let _ = self.mx.join(room, &ghost);
        self.mx.typing(room, &ghost, true);
        let (ok, answer) = self.run_with_follow_up(agent, room, reply_to, text);
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
        if let Err(e) = self.mx.send_markdown(room, &ghost, &body, Some(reply_to)) {
            log(&format!("{agent}: could not post the answer: {e}"));
        }
    }

    /// The project a room belongs to, when it is one of a project's rooms: what a message
    /// written there is for, and so where a reminder it sets is delivered.
    fn project_of(&self, room: &str) -> Option<String> {
        match self.state.kind_of(room) {
            Some(RoomKind::ProjectGeneral(p))
            | Some(RoomKind::ProjectRoom(p))
            | Some(RoomKind::ProjectFeed(p)) => Some(p),
            _ => None,
        }
    }

    /// The photo a text message to `agent` is probably about, ready to attach: its file name in the
    /// agent's inbox and the sentence that tells the agent so.
    fn follow_up_photo(&self, room: &str, agent: &str, skip: &str) -> Option<(String, String)> {
        let msgs = self.mx.messages_as_owner(room, 30).ok()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let photos = self.state.read(|d| d.photos.clone());
        let (file, mins) = recent_photo(&msgs, &self.cfg.owner, skip, now, &photos, agent)?;
        let when = if mins == 0 { "just now".to_string() } else { format!("{mins} min ago") };
        Some((file, format!("\n\n(Mark's last photo, sent {when}, is attached again.)")))
    }

    /// Runs `agent` on a text message. If the message follows a photo it was given a moment ago,
    /// that photo comes along again: the agent cannot look at a picture it was only sent a message
    /// ago, and "how much is this?" is about the picture. If that photo is no longer in its inbox
    /// the question is asked without it rather than not at all.
    fn run_with_follow_up(
        &self,
        agent: &str,
        room: &str,
        reply_to: &str,
        text: &str,
    ) -> (bool, String) {
        let project = self.project_of(room);
        let run = |text: &str, attach: &[String]| {
            self.rt.run(
                agent,
                &self.task(room, reply_to, text),
                &self.why(room),
                None,
                project.as_deref(),
                attach,
            )
        };
        let Some((file, note)) = self.follow_up_photo(room, agent, reply_to) else {
            return run(text, &[]);
        };
        let (ok, answer) = run(&format!("{text}{note}"), std::slice::from_ref(&file));
        if ok {
            return (ok, answer);
        }
        run(text, &[])
    }

    /// Why an agent is being woken, for its `Trigger:` line.
    fn why(&self, room: &str) -> String {
        match self.state.kind_of(room) {
            Some(RoomKind::Dm(_)) => "mark sent you a direct message".to_string(),
            Some(RoomKind::ProjectGeneral(p))
            | Some(RoomKind::ProjectRoom(p))
            | Some(RoomKind::ProjectFeed(p)) => format!("mark wrote in the project {p}"),
            _ => "mark wrote in a room you are in".to_string(),
        }
    }

    /// What the agent is handed: the recent conversation (so it knows what the message is
    /// about and who said what), then the new message. `HOLON_HISTORY_CHARS` bounds the
    /// history (default 3000; 0 sends the bare message). `skip` is the message being answered.
    fn task(&self, room: &str, skip: &str, text: &str) -> String {
        let budget: usize =
            std::env::var("HOLON_HISTORY_CHARS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
        if budget == 0 {
            return text.to_string();
        }
        let msgs = self.mx.messages_as_owner(room, 30).unwrap_or_default();
        let lines = history_lines(&self.cfg, &msgs, skip, budget);
        if lines.is_empty() {
            text.to_string()
        } else {
            format!("Conversation so far (oldest first):\n{lines}\n\nThe new message, from mark:\n{text}")
        }
    }

    // ---- approvals as polls ---------------------------------------------------

    /// Posts a poll for every pending approval that has none yet, and forgets the
    /// polls of approvals that are no longer pending (answered elsewhere, or timed out).
    pub fn poll_approvals(&self) {
        let Ok(pending) = self.rt.approvals() else { return };
        let projects = self.rt.projects().ok();
        let have: HashSet<u64> = self.state.read(|d| d.polls.values().copied().collect());
        for a in &pending {
            if have.contains(&a.id) {
                continue;
            }
            let Some(room) = self.state.read(|d| {
                projects.as_ref().and_then(|p| crate::scheduled::approval_room(&a.agent, p, d))
            }) else {
                continue;
            };
            let via = if a.chain.is_empty() {
                String::new()
            } else {
                format!(" (woken via {})", a.chain.join(" › "))
            };
            let what = describe_approval(&a.tool, &a.args)
                .map(|d| format!("\n\nThat means: {d}"))
                .unwrap_or_default();
            let q = format!(
                "{} wants to run {} {}{via}.{what}\n\nApprove?",
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

    // ---- archived projects (ADR-0105, section 5) ------------------------------

    /// Every room of a project, its children first and its Space last, so leaving them in
    /// this order never leaves a room whose Space is gone.
    fn rooms_of(&self, project: &str) -> Vec<String> {
        self.state.read(|d| {
            let Some(r) = d.projects.get(project) else { return Vec::new() };
            let mut rooms: Vec<String> = d
                .rooms
                .iter()
                .filter(|(_, p)| p.as_str() == project)
                .map(|(room, _)| room.clone())
                .collect();
            rooms.extend([r.general.clone(), r.feed.clone()].into_iter().filter(|x| !x.is_empty()));
            rooms.push(r.space.clone());
            rooms
        })
    }

    /// A Space the owner made and the bridge adopted: the bridge is not in it, so it can
    /// only be re-entered by an agent that is.
    fn is_adopted(&self, project: &str) -> bool {
        self.state.read(|d| {
            d.projects.get(project).is_some_and(|r| r.general.is_empty() && r.feed.is_empty())
        })
    }

    /// The owner leaves and forgets the rooms of a project the runtime has archived, once.
    fn archive_rooms(&self, p: &ProjectInfo) {
        if self.state.read(|d| d.archived.contains(&p.name) || !d.projects.contains_key(&p.name)) {
            return;
        }
        if self.is_adopted(&p.name) && p.agents.is_empty() {
            log(&format!(
                "{}: archived in the runtime, but its Space has no agent in it, so nothing could \
                 invite you back; leaving its rooms alone",
                p.name
            ));
            return;
        }
        for room in self.rooms_of(&p.name) {
            if let Err(e) = self.mx.leave_and_forget_as_owner(&room) {
                log(&format!("{}: could not leave {room}: {e}", p.name));
            }
        }
        self.state.write(|d| d.archived.insert(p.name.clone()));
        self.tell_owner(&format!(
            "Archived {}: its agents are paused where nothing else holds them, and its rooms \
             are out of your list. !project unarchive {} brings them back.",
            p.name, p.name
        ));
    }

    /// The runtime says the project is live again but the owner is out of its rooms:
    /// invite them back (the bridge can in rooms it made; in an adopted Space an agent
    /// that is in it does) and place them.
    fn restore_rooms(&self, p: &ProjectInfo) {
        if !self.state.read(|d| d.archived.contains(&p.name)) {
            return;
        }
        let bridge = self.bridge_user();
        let mut back = true;
        for room in self.rooms_of(&p.name) {
            // Idempotent, because a pass can be cut short (Synapse rate-limits joins): a room
            // you are already in is done, and an invite a pass left pending is accepted as is.
            let owner = &self.cfg.owner;
            if self.mx.joined_as_owner(&room).is_ok_and(|j| j.contains(owner)) {
                continue;
            }
            if self.mx.join_as_owner(&room).is_ok() {
                continue;
            }
            let invited = self.mx.invite(&room, &bridge, owner).is_ok()
                || p.agents
                    .iter()
                    .any(|a| self.mx.invite(&room, &self.cfg.ghost(a), owner).is_ok());
            if !(invited && self.mx.join_as_owner(&room).is_ok()) {
                back = false;
                log(&format!("{}: could not bring you back into {room} yet; trying again", p.name));
            }
        }
        if back {
            self.state.write(|d| d.archived.remove(&p.name));
        }
    }

    /// `!project archive <name>`: the guard for an adopted Space, then the runtime, then the
    /// rooms.
    pub fn archive_command(&self, name: &str) -> String {
        match self.rt.projects() {
            Err(e) => return format!("could not read projects: {e}"),
            Ok(ps) => match ps.into_iter().find(|p| p.name == name) {
                None => return format!("no project named {name}"),
                Some(p) if p.archived => return format!("{name} is already archived."),
                Some(p) if self.is_adopted(name) && p.agents.is_empty() => {
                    return format!(
                        "{name} is a Space you made and no agent is in it, so nothing could invite \
                         you back after you left it. Add an agent first (!project add {name} <agent>)."
                    )
                }
                Some(_) => {}
            },
        }
        match self.rt.archive_project(name) {
            Ok(paused) => {
                let _ = self.reconcile();
                format!(
                    "{name} archived{}. !project unarchive {name} brings it back.",
                    if paused.is_empty() {
                        String::new()
                    } else {
                        format!(": {} paused", paused.join(", "))
                    }
                )
            }
            Err(e) => format!("could not archive {name}: {e}"),
        }
    }

    // ---- results nobody asked for ---------------------------------------------

    /// Posts what the runtime ran on its own and the owner wants to read: see
    /// `scheduled::due`. An agent the bridge has not seen before is only marked as
    /// seen, so its history is not posted all at once.
    pub fn deliver_scheduled(&self) {
        let (agents, projects) = match (self.rt.agents(), self.rt.projects()) {
            (Ok(a), Ok(p)) => (a, p),
            _ => return,
        };
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        for agent in agents {
            let Some(after) = self.state.read(|d| d.delivered.get(&agent.name).copied()) else {
                self.state.write(|d| d.delivered.insert(agent.name.clone(), now_ms));
                continue;
            };
            let Ok(runs) = self.rt.runs(&agent.name, 20) else { continue };
            let due = crate::scheduled::due(&agent.triggers, &runs, after);
            if due.is_empty() {
                continue;
            }
            let ghost = self.cfg.ghost(&agent.name);
            for run in due {
                // The room of the project the run was for; else the agent's own.
                let Some(room) = self
                    .state
                    .read(|d| crate::scheduled::room_for_run(run, &agent.name, &projects, d))
                else {
                    break;
                };
                let _ = self.mx.join(&room, &ghost);
                let body = crate::scheduled::render(run, MAX_REPLY);
                if let Err(e) = self.mx.send_text(&room, &ghost, &body, None) {
                    // Not marked delivered, so the next pass tries again.
                    log(&format!(
                        "{}: could not post the result of {}: {e}",
                        agent.name, run.trigger
                    ));
                    break;
                }
                let at = run.finished_at_ms();
                self.state.write(|d| d.delivered.insert(agent.name.clone(), at));
            }
        }
    }

    // ---- background loops ---------------------------------------------------

    /// Starts the reconcile, approval and scheduled-result loops on their own threads.
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
        let me = self.clone();
        std::thread::spawn(move || loop {
            me.deliver_scheduled();
            std::thread::sleep(Duration::from_secs(10));
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
        // a lead that is no longer a member does not answer; a lone member still does
        assert!(project_targets(&names(&["rower", "chef"]), Some("gone"), &[]).is_empty());
        assert_eq!(project_targets(&names(&["rower"]), Some("gone"), &[]), ["rower"]);
    }

    #[test]
    fn history_is_oldest_first_attributed_bounded_and_leaves_out_the_message_being_answered() {
        let cfg = cfg();
        let m = |id: &str, from: &str, body: &str| json!({"event_id": id, "sender": from, "content": {"body": body}});
        let agent = cfg.ghost("coach");
        let owner = cfg.owner.clone();
        // newest first, as Matrix returns them
        let msgs = vec![
            m("$4", &owner, "yes"),
            m("$3", &agent, "Shall I ask the rower?"),
            m("$2", &owner, "how was my row"),
            m("$1", &owner, ""),
        ];
        let all = history_lines(&cfg, &msgs, "$4", 10_000);
        assert_eq!(all, "mark: how was my row\ncoach: Shall I ask the rower?");
        // a tight budget keeps the newest and drops the oldest
        let tight = history_lines(&cfg, &msgs, "$4", 30);
        assert_eq!(tight, "coach: Shall I ask the rower?");
        assert_eq!(history_lines(&cfg, &[], "", 100), "");
    }

    #[test]
    fn an_admin_approval_says_what_it_destroys() {
        let del = describe_approval("agent_delete", &json!({"name": "kevin-ishot"})).unwrap();
        assert!(del.contains("run history") && del.contains("restored for 30 days"), "{del}");
        assert!(del.contains("`kevin-ishot`") && !del.contains("PERMANENTLY"), "{del}");
        let back = describe_approval("agent_restore", &json!({"name": "kevin-ishot"})).unwrap();
        assert!(back.contains("`kevin-ishot`") && back.contains("schedules again"), "{back}");
        let one = describe_approval(
            "agent_remove_schedule",
            &json!({"name": "weather", "schedule": "@every 1h"}),
        )
        .unwrap();
        assert!(one.contains("`@every 1h`") && one.contains("stay"), "{one}");
        let all = describe_approval("agent_remove_schedule", &json!({"name": "weather"})).unwrap();
        assert!(all.contains("ALL schedules"), "{all}");
        let save = describe_approval(
            "agent_save",
            &json!({"name": "w", "schedule": "@daily", "prompt": "p"}),
        )
        .unwrap();
        assert!(save.contains("fields: prompt, schedule") && save.contains("replaces"), "{save}");
        let joined = describe_approval(
            "agent_save",
            &json!({"name": "cox", "schedule": "@daily", "project": "rowing"}),
        )
        .unwrap();
        assert!(
            joined.contains("ADDED TO the project `rowing`") && joined.contains("grants"),
            "{joined}"
        );
        let narrow = describe_approval(
            "agent_remove_schedule",
            &json!({"name": "w", "schedule": "@daily", "project": "rowing"}),
        )
        .unwrap();
        assert!(narrow.contains("work for the project `rowing`"), "{narrow}");
        assert_eq!(describe_approval("write_file", &json!({"path": "x"})), None);
    }

    #[test]
    fn a_photo_keeps_its_name_or_gets_one_from_its_type_and_its_caption_is_read() {
        // no caption: Element puts the file name in body
        let plain = json!({"msgtype": "m.image", "body": "IMG_4411.heic", "info": {"mimetype": "image/heic"}});
        assert_eq!(photo_caption(&plain), "");
        assert_eq!(photo_name(&plain), "IMG_4411.heic");
        // a caption: filename holds the name, body the words
        let said = json!({"msgtype": "m.image", "body": "post-row lunch", "filename": "lunch.png"});
        assert_eq!(photo_caption(&said), "post-row lunch");
        assert_eq!(photo_name(&said), "lunch.png");
        // a name with no extension gets one from the type
        let bare =
            json!({"msgtype": "m.image", "body": "image", "info": {"mimetype": "image/png"}});
        assert_eq!(photo_name(&bare), "image.png");
        assert_eq!(photo_name(&json!({"msgtype": "m.image", "body": "a b c"})), "photo.jpg");
    }

    fn event(id: &str, sender: &str, msgtype: &str, body: &str, ts: u64) -> Value {
        json!({"event_id": id, "sender": sender, "origin_server_ts": ts,
               "content": {"msgtype": msgtype, "body": body}})
    }

    fn photo_ref(agent: &str, event: &str, file: &str) -> crate::state::PhotoRef {
        crate::state::PhotoRef { agent: agent.into(), event: event.into(), file: file.into() }
    }

    #[test]
    fn a_photo_in_the_history_is_a_photo_not_a_file_name() {
        let cfg = cfg();
        let mut caption = event("$p2", "@mark:s.net", "m.image", "post-row lunch", 2);
        caption["content"]["filename"] = json!("IMG_1.jpg");
        let msgs = vec![
            event("$t", "@mark:s.net", "m.text", "how much is this?", 3),
            caption,
            event("$p1", "@mark:s.net", "m.image", "image.png", 1),
        ];
        let h = history_lines(&cfg, &msgs, "", 1000);
        assert_eq!(h, "mark: [a photo]\nmark: [a photo] post-row lunch\nmark: how much is this?");
        assert!(!h.contains("image.png"), "the file name says nothing to the agent");
    }

    #[test]
    fn a_text_after_a_recent_photo_the_agent_was_given_brings_it_back() {
        let owner = "@mark:s.net";
        let now = 10 * 60 * 1000;
        let msgs = vec![
            event("$t", owner, "m.text", "how much is this?", now),
            event("$p", owner, "m.image", "image.png", now - 3 * 60 * 1000),
        ];
        let photos = vec![photo_ref("chef", "$p", "17-image.png")];
        assert_eq!(
            recent_photo(&msgs, owner, "$t", now, &photos, "chef"),
            Some(("17-image.png".into(), 3))
        );
        assert_eq!(
            recent_photo(&msgs, owner, "$t", now, &photos, "coach"),
            None,
            "another agent was never given it"
        );
        assert_eq!(
            recent_photo(&msgs, owner, "$t", now, &[], "chef"),
            None,
            "a photo the bridge did not keep"
        );
    }

    #[test]
    fn an_old_photo_a_stranger_s_photo_and_a_buried_one_are_not_brought_back() {
        let owner = "@mark:s.net";
        let photos = vec![photo_ref("chef", "$old", "a.png"), photo_ref("chef", "$mine", "b.png")];
        let at = |mins: u64| mins * 60 * 1000;
        // too old: past the window the text is about something else
        let msgs = vec![event("$old", owner, "m.image", "x", 0)];
        assert_eq!(recent_photo(&msgs, owner, "$t", at(16), &photos, "chef"), None);
        assert_eq!(
            recent_photo(&msgs, owner, "$t", at(14), &photos, "chef").map(|p| p.1),
            Some(14)
        );
        // somebody else's photo is not the owner's subject
        let theirs = vec![event("$mine", "@guest:s.net", "m.image", "x", at(1))];
        assert_eq!(recent_photo(&theirs, owner, "$t", at(2), &photos, "chef"), None);
        // only the NEWEST photo counts: if it did not reach this agent, no older one is dug up
        let msgs = vec![
            event("$new", owner, "m.image", "x", at(5)),
            event("$old", owner, "m.image", "x", at(4)),
        ];
        assert_eq!(recent_photo(&msgs, owner, "$t", at(6), &photos, "chef"), None);
        // and the message being answered is never its own follow-up
        let msgs = vec![event("$mine", owner, "m.image", "x", at(5))];
        assert_eq!(recent_photo(&msgs, owner, "$mine", at(5), &photos, "chef"), None);
    }

    #[test]
    fn a_project_of_one_is_answered_by_its_only_member() {
        let one = names(&["coach"]);
        assert_eq!(project_targets(&one, None, &[]), ["coach"], "no lead, no mention: the member");
        assert_eq!(project_targets(&one, None, &names(&["stranger"])), ["coach"]);
        assert!(project_targets(&[], None, &[]).is_empty(), "an empty project answers nothing");
        assert!(project_targets(&names(&["a", "b"]), None, &[]).is_empty(), "two is not one");
    }

    #[test]
    fn only_a_clear_margin_picks_a_member() {
        let r = |v: &[(&str, f32)]| v.iter().map(|(n, s)| (n.to_string(), *s)).collect::<Vec<_>>();
        assert_eq!(clear_winner(&r(&[("rower", 0.70), ("coach", 0.62)])).as_deref(), Some("rower"));
        assert_eq!(clear_winner(&r(&[("rower", 0.70), ("coach", 0.68)])), None);
        assert_eq!(clear_winner(&r(&[("rower", 0.5)])).as_deref(), Some("rower"));
        assert_eq!(clear_winner(&[]), None);
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

    #[test]
    fn a_long_answer_is_spoken_in_part_and_cut_at_a_sentence() {
        assert_eq!(spoken_excerpt("  Short answer.  "), "Short answer.");
        let long = format!("{} Final bit.", "This is a sentence. ".repeat(80));
        let said = spoken_excerpt(&long);
        assert!(said.ends_with("The rest is in the message."), "{said}");
        assert!(said.chars().count() < 800, "{}", said.chars().count());
        assert!(said.contains("This is a sentence."));
        assert!(!said.contains("Final bit."));
        // no sentence end to cut at: still bounded
        let blob = "x".repeat(2000);
        assert!(spoken_excerpt(&blob).chars().count() < 800);
    }
}
