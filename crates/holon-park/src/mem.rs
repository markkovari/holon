//! An in-memory [`ParkStore`]: for tests, and for an engine running inside a
//! component with nothing behind it yet. One `Mutex`; no await point is ever
//! held under the lock, so this is fine on any executor, including
//! wasm32-wasip2's single-threaded one.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::error::Result;
use crate::model::TicketId;
use crate::store::{ParkStore, Record, Revision};

#[derive(Default)]
struct State {
    /// The revision counter, and each ticket's record and revision — one
    /// counter across every ticket, like a JetStream stream sequence, so a
    /// revision is never reused even across different tickets.
    next_rev: u64,
    tickets: HashMap<TicketId, (Record, Revision)>,
    by_correlation: HashMap<String, TicketId>,
    by_session: HashMap<String, HashSet<TicketId>>,
}

#[derive(Default)]
pub struct MemParkStore {
    s: Mutex<State>,
}

impl MemParkStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ParkStore for MemParkStore {
    async fn get(&self, ticket: &str) -> Result<Option<(Record, Revision)>> {
        Ok(self.s.lock().unwrap().tickets.get(ticket).cloned())
    }

    async fn create(&self, ticket: &str, record: &Record) -> Result<bool> {
        let mut g = self.s.lock().unwrap();
        if g.tickets.contains_key(ticket) {
            return Ok(false);
        }
        g.next_rev += 1;
        let rev = g.next_rev;
        g.tickets.insert(ticket.to_string(), (record.clone(), rev));
        Ok(true)
    }

    async fn cas(&self, ticket: &str, expected: Revision, record: &Record) -> Result<bool> {
        let mut g = self.s.lock().unwrap();
        let Some((_, current)) = g.tickets.get(ticket) else {
            return Ok(false);
        };
        if *current != expected {
            return Ok(false);
        }
        g.next_rev += 1;
        let rev = g.next_rev;
        g.tickets.insert(ticket.to_string(), (record.clone(), rev));
        Ok(true)
    }

    async fn claim_correlation(&self, correlation: &str, ticket: &str) -> Result<Option<TicketId>> {
        let mut g = self.s.lock().unwrap();
        if let Some(existing) = g.by_correlation.get(correlation) {
            return Ok(Some(existing.clone()));
        }
        g.by_correlation.insert(correlation.to_string(), ticket.to_string());
        Ok(None)
    }

    async fn find_correlation(&self, correlation: &str) -> Result<Option<TicketId>> {
        Ok(self.s.lock().unwrap().by_correlation.get(correlation).cloned())
    }

    async fn index_session(&self, session: &str, ticket: &str) -> Result<()> {
        self.s
            .lock()
            .unwrap()
            .by_session
            .entry(session.to_string())
            .or_default()
            .insert(ticket.to_string());
        Ok(())
    }

    async fn list_session(&self, session: &str) -> Result<Vec<TicketId>> {
        Ok(self
            .s
            .lock()
            .unwrap()
            .by_session
            .get(session)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect())
    }
}
