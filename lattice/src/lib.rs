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
    /// Where a node's inventory entry lives. The key is the node id.
    pub const INVENTORY: &str = "comp-inventory";
    /// Where artifacts live, keyed by their own sha256.
    pub const ARTIFACTS: &str = "comp-artifacts";

    /// Commands addressed to one node, as `comp.<lattice>.cmd.<node>.<verb>`.
    pub fn command_subject(lattice: &str, node: &str, verb: &str) -> String {
        format!("comp.{lattice}.cmd.{node}.{verb}")
    }

    /// Everything addressed to this node.
    pub fn command_wildcard(lattice: &str, node: &str) -> String {
        format!("comp.{lattice}.cmd.{node}.>")
    }

    /// The last token of a command subject.
    pub fn verb_of(subject: &str) -> &str {
        subject.rsplit('.').next().unwrap_or("")
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
        assert_eq!(s, "comp.prod.cmd.box-a.start");
        assert_eq!(wire::verb_of(&s), "start");
        assert_eq!(wire::verb_of(&wire::command_subject("l", "n", "drain")), "drain");
        // The wildcard must match what `command_subject` produces.
        let w = wire::command_wildcard("prod", "box-a");
        assert_eq!(w, "comp.prod.cmd.box-a.>");
        assert!(s.starts_with(w.trim_end_matches('>')));
    }
}
