//! What a node and the reconciler need from the fabric between them.
//!
//! Three traits, deliberately, because they are three different requirements that
//! one technology currently happens to satisfy:
//!
//!   [`Inventory`]  — nodes say what they are running; it expires on its own.
//!   [`CommandBus`] — the reconciler tells a node to start or stop something.
//!   [`Artifacts`]  — bulk bytes, addressed by digest.
//!
//! Writing those down as one dependency is how a system ends up unable to change
//! any of them. They are not the same: coordination wants low latency and a TTL,
//! bulk bytes want durability and cheap storage, and `--oci-mirror` in the
//! reconciler is already most of a second [`Artifacts`] implementation.
//!
//! [`nats`] implements all three. [`memory`] implements all three too — not as a
//! toy, but because an interface with exactly one implementation has never been
//! shown to be an interface at all, and because it lets the reconciler's loop be
//! tested without a broker.
//!
//! ## The TTL is the design
//!
//! [`Inventory::publish`] takes a time-to-live and entries expire without anyone
//! deleting them. That is what makes a departed node disappear on its own, and it
//! is the single property that deleted ADR-0016's orphan-reaping apparatus. An
//! implementation that cannot expire entries has to emulate it, and should say so
//! rather than quietly keep the dead.

pub mod memory;
pub mod nats;

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

/// One node's entry, as read back by the reconciler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub value: Vec<u8>,
}

/// Who is running what. Written by every node, read by the reconciler.
#[async_trait]
pub trait Inventory: Send + Sync {
    /// Publish this node's snapshot. `ttl` is how long it survives without a
    /// refresh — see the note on expiry above.
    async fn publish(&self, key: &str, value: Vec<u8>, ttl: Duration) -> Result<()>;

    /// Everything currently alive. An entry that expired between listing and
    /// reading is simply absent; that is the mechanism working, not an error.
    async fn read_all(&self) -> Result<Vec<Entry>>;
}

/// One instruction, and somewhere to answer it.
pub struct Command {
    pub verb: String,
    pub payload: Vec<u8>,
    /// Dropping this without sending is a valid outcome — it reads to the sender
    /// as "no reply", which is the same as a timeout and needs no special case.
    pub reply: tokio::sync::oneshot::Sender<Vec<u8>>,
}

/// Start/stop, from the reconciler to one node.
///
/// Request/reply rather than fire-and-forget: an ack after the work is done is
/// what lets "started" mean "will serve" instead of "is downloading".
#[async_trait]
pub trait CommandBus: Send + Sync {
    /// Take delivery of commands addressed to `node`.
    async fn serve(&self, node: &str) -> Result<tokio::sync::mpsc::Receiver<Command>>;

    /// Send one and wait for the ack.
    ///
    /// An implementation that can tell "nobody is listening on that node" from
    /// "that node is slow" should return the former promptly rather than after
    /// `timeout` — they have different fixes, and a caller that cannot distinguish
    /// them will retry the wrong one.
    async fn send(
        &self,
        node: &str,
        verb: &str,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> Result<Vec<u8>>;
}

/// Bulk bytes, addressed by content.
///
/// The name is always a digest and the caller always verifies it, so this is not
/// a trust boundary and an implementation does not have to be one.
#[async_trait]
pub trait Artifacts: Send + Sync {
    async fn put(&self, name: &str, bytes: Vec<u8>) -> Result<()>;
    async fn get(&self, name: &str) -> Result<Vec<u8>>;
    /// Cheap existence check, so a re-push is free rather than a re-upload.
    async fn has(&self, name: &str) -> bool;
}

/// Subject/key naming, in one place because both sides have to agree.
///
/// A node and the reconciler are separate binaries that never share a type, so
/// this module IS the wire contract. It lives here rather than in either of them
/// so that changing it is one edit rather than two that can disagree.
pub mod wire {
    /// The wire contract version, carried in every subject.
    ///
    /// Not decoration. A fleet is upgraded one machine at a time, so mixed versions
    /// are the NORMAL state during any rollout, and a payload that changes shape
    /// without changing its subject is misparsed in silence by whichever side is
    /// older. We watched exactly that on wasmCloud (ADR-0039): a 2.3.0 host against a
    /// 2.5.2 control plane reported `Placement: True` and then never ran the
    /// workload, with nothing in either log naming a version mismatch.
    ///
    /// With the version in the subject, an old node simply does not receive commands
    /// from a new reconciler — it goes quiet, its inventory expires, and the fleet
    /// treats it as absent (ADR-0022). "Absent" is a state this platform already
    /// handles correctly and can be seen from the outside; "present but silently
    /// misparsing" is neither.
    ///
    /// Bump this when the shape of a command or an inventory entry changes
    /// incompatibly — not for additive fields, which serde already tolerates.
    pub const V: &str = "v1";

    /// Where a node's inventory entry lives. The key is the node id.
    pub const INVENTORY: &str = "comp-inventory";
    /// Where artifacts live, keyed by their own sha256.
    pub const ARTIFACTS: &str = "comp-artifacts";
    /// Observed concurrency per ingress host, published by the ingress and read by
    /// the reconciler. A separate bucket from inventory rather than a key prefix
    /// inside it: `read_all` deserialises every entry as a `NodeInventory`, and a
    /// second shape in there would be a parse error on every pass.
    pub const LOAD: &str = "comp-load";

    /// Commands addressed to one node, as `comp.<v>.<lattice>.cmd.<node>.<verb>`.
    pub fn command_subject(lattice: &str, node: &str, verb: &str) -> String {
        format!("comp.{V}.{lattice}.cmd.{node}.{verb}")
    }

    /// Everything addressed to this node.
    pub fn command_wildcard(lattice: &str, node: &str) -> String {
        format!("comp.{V}.{lattice}.cmd.{node}.>")
    }

    /// The last token of a command subject.
    pub fn verb_of(subject: &str) -> &str {
        subject.rsplit('.').next().unwrap_or("")
    }

    /// Where a component serves its exports, as `comp.<v>.<lattice>.rpc.<instance>`.
    ///
    /// Versioned for the same reason as commands, and separately from them: the
    /// data plane and the control plane can move independently, and a shared
    /// constant is what keeps them from being changed together by accident.
    pub fn rpc_prefix(lattice: &str, instance_id: &str) -> String {
        format!("comp.{V}.{lattice}.rpc.{instance_id}")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn every_subject_carries_the_version() {
            // The whole point: a fleet mid-upgrade must not have two versions
            // talking past each other on one subject.
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
            // `verb_of` takes the LAST token, so an added prefix must not disturb
            // it — this is the function that would break quietly.
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
        // Both binaries build and parse this string independently. If they ever
        // disagree, every command silently becomes "unknown verb".
        let s = wire::command_subject("prod", "box-a", "start");
        assert_eq!(s, "comp.v1.prod.cmd.box-a.start");
        assert_eq!(wire::verb_of(&s), "start");
        assert_eq!(wire::verb_of(&wire::command_subject("l", "n", "drain")), "drain");
        // The wildcard must match what `command_subject` produces.
        let w = wire::command_wildcard("prod", "box-a");
        assert_eq!(w, "comp.v1.prod.cmd.box-a.>");
        assert!(s.starts_with(w.trim_end_matches('>')));
    }
}

/// A node's inventory snapshot, on the wire.
///
/// Snapshots are the largest and most frequent message the platform sends: every
/// node, every heartbeat, the whole truth about what it runs. At two thousand
/// instances that is 360 KiB of JSON per node per beat — a thousand nodes on a
/// five-second heartbeat is 72 MB/s of bus traffic, and NATS refuses a single
/// message over 1 MiB, which caps a node at roughly 5 500 instances (ADR-0058).
///
/// It is JSON with the same six field names repeated per instance and a 71-byte
/// digest string that is usually one of a handful, so it compresses about ten to
/// one and the ceiling moves with it. Compressing the payload was preferred over
/// designing a delta protocol because it is transparent: no sequence numbers, no
/// resync, no way for the two sides to disagree about what they have seen.
pub mod snapshot {
    /// zstd's frame magic. JSON always starts `{`, so the two can never be
    /// confused — which is what lets a fleet run mixed versions during a rollout
    /// instead of needing every node upgraded at once (the lesson of ADR-0044).
    const MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

    pub fn compress(json: Vec<u8>) -> Vec<u8> {
        // Level 1: this runs on every node every heartbeat, and the difference
        // between level 1 and level 9 here is a few percent of size for several
        // times the CPU.
        zstd::encode_all(json.as_slice(), 1).unwrap_or(json)
    }

    /// Decompress if it is a frame, pass it through if it is not.
    pub fn expand(raw: Vec<u8>) -> Vec<u8> {
        if raw.len() >= 4 && raw[..4] == MAGIC {
            // A snapshot we cannot read is skipped by the caller's parse, which is
            // the same path an expired or corrupt entry already takes.
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
