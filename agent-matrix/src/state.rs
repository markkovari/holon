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
    pub general: String,
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
}

#[derive(Clone, Debug, PartialEq)]
pub enum RoomKind {
    Control,
    Dm(String),
    ProjectGeneral(String),
    ProjectFeed(String),
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
                if r.general == room {
                    return Some(RoomKind::ProjectGeneral(p.clone()));
                }
                if r.feed == room {
                    return Some(RoomKind::ProjectFeed(p.clone()));
                }
            }
            None
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
        assert_eq!(s.kind_of("!sp:s"), None, "the space itself is not a chat room");
        assert_eq!(s.kind_of("!unknown:s"), None);
    }
}
