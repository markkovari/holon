//! The event bus: durable topics with per-consumer offsets, so an event
//! published while an agent is paused — or while the whole runtime is down —
//! is still there when it comes back.
//!
//! The shape is `components/event-bus` (an append-only log per topic, each
//! consumer acking its own offset, at-least-once), kept behind a trait so a
//! lattice-backed implementation can replace the file one without touching the
//! runtime. Every event travels in an [`Envelope`] that says who sent it and
//! what chain of wake-ups led here — which is what loop detection and the
//! trace view are built on.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::spec::validate_topic;

/// Where an event came from and how it got here.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Draft {
    pub from: String,
    pub payload: String,
    /// W3C trace id (32 hex): shared by every run one original trigger set off.
    /// Empty only for a draft not yet attached to any trace.
    pub trace_id: String,
    /// The span that emitted this (an agent's `emit_event` / `store_put` tool
    /// call), if one did. Whatever this wakes becomes its child.
    pub parent_span_id: Option<String>,
    /// Wake-ups so far in this chain.
    pub hops: u32,
    /// `agent|topic` for each wake-up so far; repeating one is a cycle.
    pub chain: Vec<String>,
    /// Tokens the rest of the chain may still spend.
    pub budget: Option<u64>,
    pub ts: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Envelope {
    pub seq: u64,
    pub id: String,
    pub topic: String,
    #[serde(flatten)]
    pub draft: Draft,
}

pub trait Bus: Send + Sync {
    /// Appends to `topic`'s log and returns the stored envelope.
    fn publish(&self, topic: &str, draft: Draft) -> Result<Envelope, String>;
    /// Up to `max` envelopes with `seq > after`, oldest first.
    fn read_after(&self, topic: &str, after: u64, max: usize) -> Result<Vec<Envelope>, String>;
    /// Newest sequence number (0 for an empty or unknown topic).
    fn head(&self, topic: &str) -> Result<u64, String>;
    /// `None` until the consumer has an offset (it has never subscribed).
    fn offset(&self, topic: &str, consumer: &str) -> Result<Option<u64>, String>;
    fn set_offset(&self, topic: &str, consumer: &str, seq: u64) -> Result<(), String>;
    fn topics(&self) -> Vec<String>;
}

pub struct FileBus {
    dir: PathBuf,
    /// Serialises writers; also guards the cached heads.
    heads: Mutex<HashMap<String, u64>>,
}

impl FileBus {
    pub fn open(state_dir: &std::path::Path) -> Result<Self, String> {
        let dir = state_dir.join("events");
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Self { dir, heads: Mutex::new(HashMap::new()) })
    }

    fn log(&self, topic: &str) -> PathBuf {
        self.dir.join(format!("{topic}.jsonl"))
    }

    fn offsets_path(&self, topic: &str) -> PathBuf {
        self.dir.join(format!("{topic}.offsets.json"))
    }

    fn read_all(&self, topic: &str) -> Vec<Envelope> {
        fs::read_to_string(self.log(topic))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn offsets(&self, topic: &str) -> HashMap<String, u64> {
        fs::read_to_string(self.offsets_path(topic))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
}

impl Bus for FileBus {
    fn publish(&self, topic: &str, draft: Draft) -> Result<Envelope, String> {
        validate_topic(topic)?;
        let mut heads = self.heads.lock().unwrap();
        let head = match heads.get(topic) {
            Some(h) => *h,
            None => self.read_all(topic).last().map_or(0, |e| e.seq),
        };
        let seq = head + 1;
        let env = Envelope { seq, id: format!("{topic}-{seq}"), topic: topic.to_string(), draft };
        let mut line = serde_json::to_string(&env).map_err(|e| e.to_string())?;
        line.push('\n');
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log(topic))
            .and_then(|mut f| f.write_all(line.as_bytes()))
            .map_err(|e| e.to_string())?;
        heads.insert(topic.to_string(), seq);
        Ok(env)
    }

    fn read_after(&self, topic: &str, after: u64, max: usize) -> Result<Vec<Envelope>, String> {
        validate_topic(topic)?;
        let _g = self.heads.lock().unwrap();
        Ok(self.read_all(topic).into_iter().filter(|e| e.seq > after).take(max).collect())
    }

    fn head(&self, topic: &str) -> Result<u64, String> {
        validate_topic(topic)?;
        let mut heads = self.heads.lock().unwrap();
        if let Some(h) = heads.get(topic) {
            return Ok(*h);
        }
        let h = self.read_all(topic).last().map_or(0, |e| e.seq);
        heads.insert(topic.to_string(), h);
        Ok(h)
    }

    fn offset(&self, topic: &str, consumer: &str) -> Result<Option<u64>, String> {
        validate_topic(topic)?;
        let _g = self.heads.lock().unwrap();
        Ok(self.offsets(topic).get(consumer).copied())
    }

    /// Offsets only move forward: acking an older event after a newer one is
    /// a no-op, so a slow retry can never replay what was already handled.
    fn set_offset(&self, topic: &str, consumer: &str, seq: u64) -> Result<(), String> {
        validate_topic(topic)?;
        let _g = self.heads.lock().unwrap();
        let mut o = self.offsets(topic);
        if o.get(consumer).is_some_and(|cur| *cur >= seq) {
            return Ok(());
        }
        o.insert(consumer.to_string(), seq);
        let path = self.offsets_path(topic);
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_string(&o).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    fn topics(&self) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".jsonl").map(String::from))
            .collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bus(tag: &str) -> (FileBus, PathBuf) {
        let d = std::env::temp_dir().join(format!(
            "ar-bus-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        (FileBus::open(&d).unwrap(), d)
    }

    fn draft(payload: &str) -> Draft {
        Draft { from: "t".into(), payload: payload.into(), ..Default::default() }
    }

    #[test]
    fn publish_assigns_increasing_seq_per_topic_and_reads_after_an_offset() {
        let (b, _) = bus("seq");
        assert_eq!(b.head("a").unwrap(), 0);
        let e1 = b.publish("a", draft("one")).unwrap();
        let e2 = b.publish("a", draft("two")).unwrap();
        let o1 = b.publish("other", draft("x")).unwrap();
        assert_eq!((e1.seq, e2.seq, o1.seq), (1, 2, 1));
        assert_eq!(e2.id, "a-2");
        assert_eq!(b.read_after("a", 0, 10).unwrap().len(), 2);
        let after1 = b.read_after("a", 1, 10).unwrap();
        assert_eq!(after1.len(), 1);
        assert_eq!(after1[0].draft.payload, "two");
        assert_eq!(b.read_after("a", 0, 1).unwrap().len(), 1);
        assert_eq!(b.topics(), ["a", "other"]);
    }

    #[test]
    fn the_log_and_offsets_survive_a_restart_and_offsets_only_move_forward() {
        let (b, dir) = bus("durable");
        b.publish("t", draft("1")).unwrap();
        b.publish("t", draft("2")).unwrap();
        assert_eq!(b.offset("t", "agent").unwrap(), None);
        b.set_offset("t", "agent", 2).unwrap();
        b.set_offset("t", "agent", 1).unwrap(); // a late, older ack
        drop(b);
        let again = FileBus::open(&dir).unwrap();
        assert_eq!(again.head("t").unwrap(), 2);
        assert_eq!(again.offset("t", "agent").unwrap(), Some(2));
        assert_eq!(again.publish("t", draft("3")).unwrap().seq, 3, "seq continues, never reused");
    }

    #[test]
    fn envelopes_carry_their_chain_and_topic_names_are_checked() {
        let (b, _) = bus("env");
        let d = Draft {
            from: "logwatch".into(),
            payload: "p".into(),
            trace_id: "0af7651916cd43dd8448eb211c80319c".into(),
            parent_span_id: Some("b7ad6b7169203331".into()),
            hops: 2,
            chain: vec!["a|x".into()],
            budget: Some(900),
            ts: 5,
        };
        let e = b.publish("deploy", d.clone()).unwrap();
        assert_eq!(b.read_after("deploy", 0, 1).unwrap()[0], e);
        assert_eq!(e.draft, d);
        assert!(b.publish("../escape", d).is_err());
        assert!(b.read_after("a b", 0, 1).is_err());
    }
}
