//! The shared store: namespaced key/value with versions and compare-and-set.
//! The blackboard agents collaborate through when they should not be talking
//! to each other directly — one writes what it found, others read it, and
//! (see `Trigger::StoreChange`) the ones who care are woken when it changes.
//!
//! One JSON file per namespace under `<state>/store/`. Single process, one
//! lock: that is what makes `if_version` a real compare-and-set rather than a
//! hope (`wasi:keyvalue`, which the lattice offers, has no CAS — ADR-0008).

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::spec::validate_topic;

pub const MAX_VALUE: usize = 64 * 1024;
pub const MAX_KEY: usize = 200;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub value: String,
    /// 1 for a fresh key, +1 for every write that changed the value.
    pub version: u64,
    pub at: u64,
    pub by: String,
}

#[derive(Debug, PartialEq)]
pub struct Put {
    /// False when the value was identical: nothing was written, the version
    /// did not move, and nobody should be woken.
    pub changed: bool,
    pub version: u64,
}

pub struct Kv {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl Kv {
    pub fn open(state_dir: &std::path::Path) -> Result<Self, String> {
        let dir = state_dir.join("store");
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Self { dir, lock: Mutex::new(()) })
    }

    fn load(&self, ns: &str) -> Result<BTreeMap<String, Entry>, String> {
        validate_topic(ns)?;
        match fs::read_to_string(self.dir.join(format!("{ns}.json"))) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| format!("store {ns} is corrupt: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn get(&self, ns: &str, key: &str) -> Result<Option<Entry>, String> {
        let _g = self.lock.lock().unwrap();
        Ok(self.load(ns)?.get(key).cloned())
    }

    /// Keys under `prefix`, in key order.
    pub fn list(&self, ns: &str, prefix: &str) -> Result<Vec<(String, Entry)>, String> {
        let _g = self.lock.lock().unwrap();
        Ok(self.load(ns)?.into_iter().filter(|(k, _)| k.starts_with(prefix)).collect())
    }

    /// Writes `value`. With `if_version`, the write only happens if the key's
    /// current version equals it (0 means "must not exist yet").
    pub fn put(
        &self,
        ns: &str,
        key: &str,
        value: &str,
        by: &str,
        now: u64,
        if_version: Option<u64>,
    ) -> Result<Put, String> {
        if key.is_empty() || key.len() > MAX_KEY || key.chars().any(char::is_control) {
            return Err(format!("key must be 1-{MAX_KEY} characters with no control characters"));
        }
        if value.len() > MAX_VALUE {
            return Err(format!("value is {} bytes; the limit is {MAX_VALUE}", value.len()));
        }
        let _g = self.lock.lock().unwrap();
        let mut map = self.load(ns)?;
        let current = map.get(key).map_or(0, |e| e.version);
        if let Some(want) = if_version {
            if want != current {
                return Err(format!("conflict: {ns}/{key} is at version {current}, not {want}"));
            }
        }
        if map.get(key).is_some_and(|e| e.value == value) {
            return Ok(Put { changed: false, version: current });
        }
        let version = current + 1;
        map.insert(
            key.to_string(),
            Entry { value: value.to_string(), version, at: now, by: by.to_string() },
        );
        let path = self.dir.join(format!("{ns}.json"));
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(&map).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        Ok(Put { changed: true, version })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(tag: &str) -> Kv {
        let d = crate::testutil::dir(&format!("ar-kv-{tag}"));
        Kv::open(&d).unwrap()
    }

    #[test]
    fn writes_version_and_only_changes_count() {
        let k = kv("v");
        assert_eq!(
            k.put("rowing", "latest", "a", "bot", 1, None).unwrap(),
            Put { changed: true, version: 1 }
        );
        assert_eq!(
            k.put("rowing", "latest", "a", "bot", 2, None).unwrap(),
            Put { changed: false, version: 1 }
        );
        assert_eq!(
            k.put("rowing", "latest", "b", "bot", 3, None).unwrap(),
            Put { changed: true, version: 2 }
        );
        let e = k.get("rowing", "latest").unwrap().unwrap();
        assert_eq!((e.value.as_str(), e.version, e.at, e.by.as_str()), ("b", 2, 3, "bot"));
        assert!(k.get("rowing", "missing").unwrap().is_none());
    }

    #[test]
    fn compare_and_set_rejects_a_stale_writer() {
        let k = kv("cas");
        assert!(k.put("n", "k", "x", "a", 1, Some(0)).is_ok(), "0 = must not exist yet");
        assert!(k.put("n", "k", "y", "b", 2, Some(0)).unwrap_err().contains("conflict"));
        assert!(k.put("n", "k", "y", "b", 2, Some(1)).is_ok());
        assert!(k.put("n", "k", "z", "a", 3, Some(1)).unwrap_err().contains("version 2"));
    }

    #[test]
    fn list_filters_by_prefix_and_data_survives_reopening() {
        let k = kv("list");
        for (key, v) in [("a/1", "x"), ("a/2", "y"), ("b/1", "z")] {
            k.put("ns", key, v, "t", 1, None).unwrap();
        }
        let l = k.list("ns", "a/").unwrap();
        assert_eq!(l.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["a/1", "a/2"]);
        let again = Kv::open(k.dir.parent().unwrap()).unwrap();
        assert_eq!(again.get("ns", "b/1").unwrap().unwrap().value, "z");
    }

    #[test]
    fn bad_names_and_oversized_values_are_refused() {
        let k = kv("bad");
        assert!(k.put("../x", "k", "v", "t", 1, None).is_err());
        assert!(k.put("ns", "", "v", "t", 1, None).is_err());
        assert!(k.put("ns", "k\n", "v", "t", 1, None).is_err());
        assert!(k.put("ns", "k", &"x".repeat(MAX_VALUE + 1), "t", 1, None).is_err());
        assert!(k.get("a b", "k").is_err());
    }
}
