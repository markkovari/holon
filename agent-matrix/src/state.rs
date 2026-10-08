//! What the bridge remembers between runs: which room is which. Everything else
//! (the agents, the projects, who belongs to what) is read from the runtime each
//! time, so this file cannot disagree with it about anything that matters.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ProjectRooms {
    pub space: String,
    /// Empty for a Space the owner made themselves and the bridge adopted: its
    /// rooms are whatever the owner puts in it (see `Data::rooms`).
    #[serde(default)]
    pub general: String,
    #[serde(default)]
    pub feed: String,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Data {
    pub control_room: Option<String>,
    /// agent name -> its direct room with the owner
    pub dms: BTreeMap<String, String>,
    pub projects: BTreeMap<String, ProjectRooms>,
    /// poll event id -> runtime approval id
    pub polls: BTreeMap<String, u64>,
    /// room id -> project, for the rooms inside an ADOPTED Space (rebuilt each
    /// reconcile from the Space's `m.space.child` events).
    #[serde(default)]
    pub rooms: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RoomKind {
    Control,
    Dm(String),
    ProjectGeneral(String),
    ProjectFeed(String),
    /// The Space itself. Inviting an agent to it, or removing one, changes the project.
    ProjectSpace(String),
    /// A room inside an adopted Space: talked to like a project's general room.
    ProjectRoom(String),
}

pub struct State {
    path: PathBuf,
    data: Mutex<Data>,
}

impl State {
    pub fn load(path: PathBuf) -> Self {
        let data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { path, data: Mutex::new(data) }
    }

    pub fn read<T>(&self, f: impl FnOnce(&Data) -> T) -> T {
        f(&self.data.lock().unwrap())
    }

    /// Mutates and saves, atomically.
    pub fn write<T>(&self, f: impl FnOnce(&mut Data) -> T) -> T {
        let mut d = self.data.lock().unwrap();
        let out = f(&mut d);
        if let Ok(body) = serde_json::to_string_pretty(&*d) {
            let _ = crate::config::write_private(&self.path, &body);
        }
        out
    }

    pub fn kind_of(&self, room: &str) -> Option<RoomKind> {
        self.read(|d| {
            if d.control_room.as_deref() == Some(room) {
                return Some(RoomKind::Control);
            }
            if let Some((a, _)) = d.dms.iter().find(|(_, r)| r.as_str() == room) {
                return Some(RoomKind::Dm(a.clone()));
            }
            for (p, r) in &d.projects {
                if !r.general.is_empty() && r.general == room {
                    return Some(RoomKind::ProjectGeneral(p.clone()));
                }
                if !r.feed.is_empty() && r.feed == room {
                    return Some(RoomKind::ProjectFeed(p.clone()));
                }
                if r.space == room {
                    return Some(RoomKind::ProjectSpace(p.clone()));
                }
            }
            d.rooms.get(room).map(|p| RoomKind::ProjectRoom(p.clone()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooms_are_classified_and_state_survives_a_reload() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("state.json");
        let s = State::load(path.clone());
        s.write(|d| {
            d.control_room = Some("!c:s".into());
            d.dms.insert("rower".into(), "!dm:s".into());
            d.projects.insert(
                "rowing".into(),
                ProjectRooms { space: "!sp:s".into(), general: "!g:s".into(), feed: "!f:s".into() },
            );
        });
        let s = State::load(path);
        assert_eq!(s.kind_of("!c:s"), Some(RoomKind::Control));
        assert_eq!(s.kind_of("!dm:s"), Some(RoomKind::Dm("rower".into())));
        assert_eq!(s.kind_of("!g:s"), Some(RoomKind::ProjectGeneral("rowing".into())));
        assert_eq!(s.kind_of("!f:s"), Some(RoomKind::ProjectFeed("rowing".into())));
        assert_eq!(s.kind_of("!sp:s"), Some(RoomKind::ProjectSpace("rowing".into())));
        assert_eq!(s.kind_of("!unknown:s"), None);
        // rooms inside an adopted Space, and a Space adopted with no rooms of its own
        s.write(|d| {
            d.projects.insert(
                "nutrition".into(),
                ProjectRooms { space: "!ns:s".into(), general: String::new(), feed: String::new() },
            );
            d.rooms.insert("!meals:s".into(), "nutrition".into());
        });
        assert_eq!(s.kind_of("!meals:s"), Some(RoomKind::ProjectRoom("nutrition".into())));
        assert_eq!(s.kind_of("!ns:s"), Some(RoomKind::ProjectSpace("nutrition".into())));
        assert_eq!(
            s.kind_of(""),
            None,
            "an empty id never matches an adopted space's empty general room"
        );
    }
}
