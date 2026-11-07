//! Only which room is which; everything else is read from the runtime, so this cannot disagree with it.

use std::collections::BTreeMap;
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ProjectRooms {
    pub space: String,
    /// Empty for an adopted Space: its rooms are in `Data::rooms`.
    #[serde(default)]
    pub general: String,
    #[serde(default)]
    pub feed: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PhotoRef {
    pub agent: String,
    pub event: String,
    pub file: String,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Data {
    pub control_room: Option<String>,
    pub dms: BTreeMap<String, String>,
    pub projects: BTreeMap<String, ProjectRooms>,
    pub polls: BTreeMap<String, u64>,
    /// Rooms inside an adopted Space; rebuilt each reconcile from `m.space.child`.
    #[serde(default)]
    pub rooms: BTreeMap<String, String>,
    /// Last posted run's `finished_ms` (or first-seen time), so a restart neither repeats nor floods.
    #[serde(default)]
    pub delivered: BTreeMap<String, u64>,
    /// A trashed agent's old DM, so a restore invites the owner back to the same history.
    #[serde(default)]
    pub retired: BTreeMap<String, String>,
    /// Holds agents in no project; not a runtime project, since membership is the grant.
    #[serde(default)]
    pub agents_space: Option<String>,
    /// So a follow-up text can resend a photo: an agent cannot see one sent a message ago.
    #[serde(default)]
    pub photos: Vec<PhotoRef>,
    #[serde(default)]
    pub archived: std::collections::BTreeSet<String>,
    /// Recent event ids, so a transaction Synapse sends again is not acted on twice.
    #[serde(default)]
    pub seen: VecDeque<String>,
    #[serde(default)]
    pub absence: crate::retire::Absence,
    #[serde(default)]
    pub refusal_told: bool,
    /// Rooms the owner could not be placed in yet (Synapse rate-limits joins).
    #[serde(default)]
    pub unplaced: Vec<String>,
    /// Runs started and not yet answered: each pump pass asks after them.
    #[serde(default)]
    pub pending: Vec<Pending>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RoomKind {
    Control,
    Dm(String),
    ProjectGeneral(String),
    ProjectFeed(String),
    ProjectSpace(String),
    ProjectRoom(String),
}

/// A run the bridge started and owes an answer for.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Pending {
    pub job: String,
    pub agent: String,
    pub room: String,
    /// Who posts the answer: the agent, or the bridge in the control room.
    pub sender: String,
    pub reply_to: Option<String>,
    pub thread: Option<String>,
    /// The "thinking" message the answer replaces.
    pub placeholder: Option<String>,
    /// A voice message's answer is also spoken.
    pub speak: bool,
    /// `job` is the voice message's transcription, not yet a run.
    #[serde(default)]
    pub transcribing: bool,
    /// A run with a photo attached again: if it fails, the bare task is asked instead.
    pub retry: Option<Retry>,
    /// A steer directive's text, which the answer is reported under.
    pub steer: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Retry {
    pub task: String,
    pub why: String,
    pub project: Option<String>,
}

/// Where the one document lives: the app's keyvalue in the component, a file or memory in tests.
pub trait Store: Send + Sync {
    /// The document and its revision; revision 0 is "none yet".
    fn load(&self) -> (Option<Vec<u8>>, u64);
    /// `false`: it was written since `rev`, nothing was saved.
    fn save(&self, body: &[u8], rev: u64) -> bool;
}

/// A file for one process, revisions counted in memory. Native only: the component has no files.
#[cfg(not(target_family = "wasm"))]
pub struct FileStore {
    path: std::path::PathBuf,
    rev: std::sync::Mutex<u64>,
}

#[cfg(not(target_family = "wasm"))]
impl FileStore {
    pub fn new(path: std::path::PathBuf) -> Self {
        let rev = u64::from(path.exists());
        Self { path, rev: std::sync::Mutex::new(rev) }
    }
}

#[cfg(not(target_family = "wasm"))]
impl Store for FileStore {
    fn load(&self) -> (Option<Vec<u8>>, u64) {
        let rev = *self.rev.lock().unwrap();
        (std::fs::read(&self.path).ok(), rev)
    }

    fn save(&self, body: &[u8], rev: u64) -> bool {
        let mut g = self.rev.lock().unwrap();
        if *g != rev {
            return false;
        }
        let tmp = self.path.with_extension("tmp");
        if std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, &self.path)).is_err() {
            return false;
        }
        *g += 1;
        true
    }
}

/// Every write is a compare-and-set: a pump pass and a Synapse transaction may run at once.
pub struct State(Box<dyn Store>);

const WRITE_RETRIES: u32 = 32;
/// Synapse resends a transaction it got no answer for soon after, never hundreds of events later.
const SEEN: usize = 500;

impl State {
    pub fn new(store: Box<dyn Store>) -> Self {
        Self(store)
    }

    fn load(&self) -> (Data, u64) {
        let (body, rev) = self.0.load();
        (body.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default(), rev)
    }

    pub fn read<T>(&self, f: impl FnOnce(&Data) -> T) -> T {
        f(&self.load().0)
    }

    /// `f` may run more than once: it runs again on the newer document when a write lost.
    pub fn write<T>(&self, mut f: impl FnMut(&mut Data) -> T) -> T {
        let mut out = None;
        for _ in 0..WRITE_RETRIES {
            let (mut d, rev) = self.load();
            let v = f(&mut d);
            let Ok(body) = serde_json::to_vec(&d) else { return v };
            if self.0.save(&body, rev) {
                return v;
            }
            out = Some(v);
        }
        eprintln!("agent-matrix: state write lost {WRITE_RETRIES} races; this change is dropped");
        out.expect("at least one attempt")
    }

    /// `false` when this event was seen before.
    pub fn first_time(&self, event_id: &str) -> bool {
        if event_id.is_empty() {
            return true;
        }
        self.write(|d| {
            if d.seen.iter().any(|e| e == event_id) {
                return false;
            }
            d.seen.push_back(event_id.to_string());
            while d.seen.len() > SEEN {
                d.seen.pop_front();
            }
            true
        })
    }

    /// Takes `job` out of the pending list; only the caller that took it delivers its answer.
    pub fn take_pending(&self, job: &str) -> Option<Pending> {
        self.write(|d| {
            let i = d.pending.iter().position(|p| p.job == job)?;
            Some(d.pending.remove(i))
        })
    }

    /// Takes a whole document, only while there is none: what moves the native bridge's file in.
    pub fn import(&self, body: &[u8]) -> Result<(), String> {
        let d: Data = serde_json::from_slice(body).map_err(|e| format!("not a state file: {e}"))?;
        let body = serde_json::to_vec(&d).map_err(|e| e.to_string())?;
        if self.0.load().0.is_some() || !self.0.save(&body, 0) {
            return Err("this bridge already has state; nothing was imported".into());
        }
        Ok(())
    }

    pub fn add_pending(&self, p: Pending) {
        self.write(|d| d.pending.push(p.clone()));
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
        let s = State::new(Box::new(FileStore::new(path.clone())));
        s.write(|d| {
            d.control_room = Some("!c:s".into());
            d.dms.insert("rower".into(), "!dm:s".into());
            d.projects.insert(
                "rowing".into(),
                ProjectRooms { space: "!sp:s".into(), general: "!g:s".into(), feed: "!f:s".into() },
            );
        });
        let s = State::new(Box::new(FileStore::new(path)));
        assert_eq!(s.kind_of("!c:s"), Some(RoomKind::Control));
        assert_eq!(s.kind_of("!dm:s"), Some(RoomKind::Dm("rower".into())));
        assert_eq!(s.kind_of("!g:s"), Some(RoomKind::ProjectGeneral("rowing".into())));
        assert_eq!(s.kind_of("!f:s"), Some(RoomKind::ProjectFeed("rowing".into())));
        assert_eq!(s.kind_of("!sp:s"), Some(RoomKind::ProjectSpace("rowing".into())));
        assert_eq!(s.kind_of("!unknown:s"), None);
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
