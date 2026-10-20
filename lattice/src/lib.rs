pub mod lease;
pub mod memory;
pub mod nats;

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub value: Vec<u8>,
}

#[async_trait]
pub trait Inventory: Send + Sync {
    /// Entries expire after `ttl` without a delete; that is how a departed node disappears.
    async fn publish(&self, key: &str, value: Vec<u8>, ttl: Duration) -> Result<()>;

    /// An entry that expires mid-listing is absent, not an error.
    async fn read_all(&self) -> Result<Vec<Entry>>;
}

pub struct Command {
    pub verb: String,
    pub payload: Vec<u8>,
    /// Dropping it unsent is fine: the sender sees the same as a timeout.
    pub reply: tokio::sync::oneshot::Sender<Vec<u8>>,
}

/// Request/reply so the ack after the work means "will serve", not "is downloading".
#[async_trait]
pub trait CommandBus: Send + Sync {
    async fn serve(&self, node: &str) -> Result<tokio::sync::mpsc::Receiver<Command>>;

    /// Fail "nobody listening" promptly, not after `timeout`: it has a different fix than "slow".
    async fn send(
        &self,
        node: &str,
        verb: &str,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> Result<Vec<u8>>;
}

/// Not a trust boundary: names are digests and the caller verifies them.
#[async_trait]
pub trait Artifacts: Send + Sync {
    async fn put(&self, name: &str, bytes: Vec<u8>) -> Result<()>;
    async fn get(&self, name: &str) -> Result<Vec<u8>>;
    async fn has(&self, name: &str) -> bool;
}

/// The wire contract: node and reconciler are separate binaries that must agree on these names.
pub mod wire {
    /// In every subject so an old node mid-rollout goes quiet instead of misparsing (ADR-0039).
    /// Bump only for incompatible shape changes; serde tolerates additive fields.
    pub const V: &str = "v1";

    pub const INVENTORY: &str = "comp-inventory";
    pub const ARTIFACTS: &str = "comp-artifacts";
    /// Its own bucket because `read_all` parses every inventory entry as a `NodeInventory`.
    pub const LOAD: &str = "comp-load";

    pub fn command_subject(lattice: &str, node: &str, verb: &str) -> String {
        format!("comp.{V}.{lattice}.cmd.{node}.{verb}")
    }

    pub fn command_wildcard(lattice: &str, node: &str) -> String {
        format!("comp.{V}.{lattice}.cmd.{node}.>")
    }

    pub fn verb_of(subject: &str) -> &str {
        subject.rsplit('.').next().unwrap_or("")
    }

    pub fn rpc_prefix(lattice: &str, instance_id: &str) -> String {
        format!("comp.{V}.{lattice}.rpc.{instance_id}")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn every_subject_carries_the_version() {
            for s in [
                command_subject("l", "n1", "start"),
                command_wildcard("l", "n1"),
                rpc_prefix("l", "t/a/c"),
            ] {
                assert!(s.starts_with(&format!("comp.{V}.")), "{s}");
            }
        }

        #[test]
        fn the_verb_survives_the_extra_token() {
            assert_eq!(verb_of(&command_subject("l", "n1", "start")), "start");
            assert_eq!(verb_of(&command_subject("l", "node.with.dots", "stop")), "stop");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verb_round_trips_through_a_subject() {
        let s = wire::command_subject("prod", "box-a", "start");
        assert_eq!(s, "comp.v1.prod.cmd.box-a.start");
        assert_eq!(wire::verb_of(&s), "start");
        assert_eq!(wire::verb_of(&wire::command_subject("l", "n", "drain")), "drain");
        let w = wire::command_wildcard("prod", "box-a");
        assert_eq!(w, "comp.v1.prod.cmd.box-a.>");
        assert!(s.starts_with(w.trim_end_matches('>')));
    }
}

/// zstd: JSON snapshots hit NATS's 1 MiB message cap near 5 500 instances (ADR-0058).
pub mod snapshot {
    /// JSON starts with `{`, so the magic tells old and new nodes apart mid-rollout (ADR-0044).
    const MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

    pub fn compress(json: Vec<u8>) -> Vec<u8> {
        // Level 1: runs every heartbeat; higher levels cost several times the CPU for a few %.
        zstd::encode_all(json.as_slice(), 1).unwrap_or(json)
    }

    pub fn expand(raw: Vec<u8>) -> Vec<u8> {
        if raw.len() >= 4 && raw[..4] == MAGIC {
            // Unreadable becomes empty, which the caller's parse skips like a corrupt entry.
            zstd::decode_all(raw.as_slice()).unwrap_or_default()
        } else {
            raw
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_snapshot_round_trips_and_plain_json_still_reads() {
            let json = br#"[{"tenant":"acme","app":"shop","digest":"sha256:aaaa"}]"#.repeat(50);
            let small = compress(json.clone());
            assert!(small.len() * 4 < json.len(), "{} -> {}", json.len(), small.len());
            assert_eq!(expand(small), json);
            // A node that has not been upgraded yet publishes plain JSON.
            assert_eq!(expand(json.clone()), json);
        }
    }
}
